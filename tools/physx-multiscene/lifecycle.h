#pragma once

#include "PxPhysicsAPI.h"
#include <array>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

namespace room_lifecycle {
using namespace physx;

inline void check(bool value, const char* message) {
    if (!value) throw std::runtime_error(message);
}

template <class T> T* require(T* value, const char* message) {
    check(value != nullptr, message);
    return value;
}

inline std::uint64_t identity(const void* value) {
    return reinterpret_cast<std::uintptr_t>(value);
}

struct Options {
    const char* capture;
    const char* truth;
    const char* host = nullptr;
    int port = 0;
    bool controlled = false;

    Options(int argc, char** argv) {
        check(argc >= 4, "usage: --lifecycle capture.pvdstream truth.json [--host HOST --port PORT] [--step-control]");
        capture = argv[2];
        truth = argv[3];
        check(std::string(capture) != truth, "capture and truth paths must differ");
        for (int i = 4; i < argc; ++i) {
            const std::string option = argv[i];
            if (option == "--host" && !host && i + 1 < argc) host = argv[++i];
            else if (option == "--port" && !port && i + 1 < argc) {
                const std::string value = argv[++i];
                check(!value.empty() && value.find_first_not_of("0123456789") == std::string::npos, "invalid port");
                port = std::stoi(value);
                check(port >= 1 && port <= 65535, "invalid port");
            } else if (option == "--step-control" && !controlled) controlled = true;
            else throw std::runtime_error("unsupported or repeated lifecycle option: " + option);
        }
        check((host != nullptr) == (port != 0), "host and port are required together");
    }

    void wait(const char* command) const {
        if (!controlled) return;
        std::string value;
        check(static_cast<bool>(std::getline(std::cin, value)), "step control input ended");
        check(value == command, "unexpected step control command");
    }
};

class Transport final : public PxPvdTransport {
    PxPvdTransport* file = nullptr;
    PxPvdTransport* socket = nullptr;
    std::mutex mutex;
public:
    explicit Transport(const Options& options) {
        file = require(PxDefaultPvdFileTransportCreate(options.capture), "file transport creation failed");
        if (options.host) {
            socket = PxDefaultPvdSocketTransportCreate(options.host, options.port, 10000);
            if (!socket) { file->release(); file = nullptr; throw std::runtime_error("socket transport creation failed"); }
        }
    }
    ~Transport() override {
        if (socket) socket->release();
        if (file) file->release();
    }
    bool connect() override {
        if (!file->connect()) return false;
        if (socket && !socket->connect()) { file->disconnect(); return false; }
        return true;
    }
    void disconnect() override { if (socket) socket->disconnect(); file->disconnect(); }
    bool isConnected() override { return file->isConnected() && (!socket || socket->isConnected()); }
    bool write(const uint8_t* bytes, uint32_t length) override {
        std::lock_guard<PxPvdTransport> file_lock(*file);
        if (!file->write(bytes, length)) return false;
        if (!socket) return true;
        std::lock_guard<PxPvdTransport> socket_lock(*socket);
        return socket->write(bytes, length);
    }
    PxPvdTransport& lock() override { mutex.lock(); return *this; }
    void unlock() override { mutex.unlock(); }
    void flush() override {
        std::lock_guard<PxPvdTransport> file_lock(*file);
        file->flush();
        if (socket) { std::lock_guard<PxPvdTransport> socket_lock(*socket); socket->flush(); }
    }
    uint64_t getWrittenDataSize() override { return file->getWrittenDataSize(); }
    void release() override { delete this; }
};

struct Errors : PxErrorCallback {
    std::atomic<unsigned> failures{0};
    void reportError(PxErrorCode::Enum code, const char* message, const char* file, int line) override {
        std::fprintf(stderr, "PhysX %u: %s (%s:%d)\n", unsigned(code), message, file, line);
        if (code != PxErrorCode::eDEBUG_INFO && code != PxErrorCode::eDEBUG_WARNING) ++failures;
    }
};

struct Room {
    PxScene* scene = nullptr;
    PxRigidDynamic* actor = nullptr;
    std::uint64_t scene_id = 0, actor_id = 0;
    unsigned steps = 0;
    bool destroyed = false;
    PxTransform published{PxIdentity};
};

struct Frame {
    unsigned room, step;
    std::uint64_t before, after;
    PxTransform pose;
};

struct Checkpoint {
    std::string name;
    std::uint64_t bytes;
    unsigned frames;
    std::vector<unsigned> alive;
};

struct Context {
    PxDefaultAllocator allocator;
    Errors errors;
    PxFoundation* foundation = nullptr;
    Transport* transport = nullptr;
    PxPvd* pvd = nullptr;
    PxPhysics* physics = nullptr;
    PxDefaultCpuDispatcher* dispatcher = nullptr;
    PxMaterial* material = nullptr;
    std::array<Room, 4> rooms;
    std::vector<Frame> frames;
    std::vector<Checkpoint> checkpoints;
    bool disconnected = false;

    ~Context() {
        for (auto& room : rooms) {
            if (room.actor) room.actor->release();
            if (room.scene) room.scene->release();
        }
        if (material) material->release();
        if (dispatcher) dispatcher->release();
        if (physics) physics->release();
        if (pvd) { pvd->disconnect(); pvd->release(); }
        if (transport) transport->release();
        if (foundation) foundation->release();
    }

    void initialize(const Options& options) {
        foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, allocator, errors), "foundation creation failed");
        pvd = require(PxCreatePvd(*foundation), "PVD creation failed");
        transport = new Transport(options);
        check(pvd->connect(*transport, PxPvdInstrumentationFlag::eDEBUG), "PVD connection failed");
        physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *foundation, PxTolerancesScale(), false, pvd), "physics creation failed");
        dispatcher = require(PxDefaultCpuDispatcherCreate(2), "dispatcher creation failed");
        material = require(physics->createMaterial(0.5f, 0.5f, 0.2f), "material creation failed");
    }

    std::uint64_t offset() {
        std::lock_guard<PxPvdTransport> lock(*transport);
        transport->flush();
        return transport->getWrittenDataSize();
    }

    void create(unsigned index, bool with_actor) {
        auto& room = rooms.at(index);
        check(room.scene == nullptr && room.scene_id == 0, "room already created");
        PxSceneDesc desc(physics->getTolerancesScale());
        desc.gravity = PxVec3(0, -9.81f, 0);
        desc.cpuDispatcher = dispatcher;
        desc.filterShader = PxDefaultSimulationFilterShader;
        room.scene = require(physics->createScene(desc), "scene creation failed");
        room.scene_id = identity(room.scene);
        if (with_actor) {
            room.actor = require(PxCreateDynamic(*physics, PxTransform(PxVec3(float(index) * 12, 4, 0)),
                PxBoxGeometry(0.5f, 0.5f, 0.5f), *material, 10.0f), "actor creation failed");
            room.actor_id = identity(room.actor);
            room.scene->addActor(*room.actor);
            room.published = room.actor->getGlobalPose();
        }
    }

    void destroy(unsigned index) {
        auto& room = rooms.at(index);
        check(room.scene != nullptr, "room already destroyed");
        if (room.actor) { room.actor->release(); room.actor = nullptr; }
        room.scene->release(); room.scene = nullptr;
        room.destroyed = true;
    }

    void step(unsigned index) {
        auto& room = rooms.at(index);
        check(room.scene && room.actor, "room cannot simulate");
        Frame frame{index, room.steps, offset(), 0, room.published};
        room.scene->simulate(index == 1 ? 1.0f / 30.0f : 1.0f / 60.0f);
        check(room.scene->fetchResults(true), "fetchResults failed");
        frame.after = offset();
        room.published = room.actor->getGlobalPose();
        ++room.steps;
        frames.push_back(frame);
    }

    void write(const Options& options) {
        std::ofstream out(options.truth, std::ios::binary | std::ios::trunc);
        check(static_cast<bool>(out), "cannot open lifecycle truth");
        out << std::setprecision(std::numeric_limits<float>::max_digits10) << std::boolalpha;
        out << "{\"schema\":1,\"scenario\":\"room-lifecycle\",\"sdk\":\"3.4.2\",\"instrumentation\":\"eDEBUG\",\"socket\":"
            << (options.host != nullptr) << ",\"controlled\":" << options.controlled
            << ",\"disconnected\":" << disconnected << ",\"replacement_reused_address\":"
            << (rooms[3].scene_id != 0 && rooms[3].scene_id == rooms[1].scene_id) << ",\"rooms\":[";
        bool first = true;
        for (unsigned index = 0; index != rooms.size(); ++index) {
            const auto& room = rooms[index];
            if (!room.scene_id) continue;
            if (!first) out << ',';
            first = false;
            out << "{\"number\":" << index + 1 << ",\"id\":" << room.scene_id << ",\"actor\":" << room.actor_id
                << ",\"steps\":" << room.steps << ",\"destroyed\":" << room.destroyed << '}';
        }
        out << "],\"frames\":[";
        for (unsigned index = 0; index != frames.size(); ++index) {
            if (index) out << ',';
            const auto& frame = frames[index];
            const auto& p = frame.pose.p;
            const auto& q = frame.pose.q;
            out << "{\"frame\":" << index << ",\"room\":" << frame.room + 1 << ",\"step\":" << frame.step
                << ",\"before_bytes\":" << frame.before << ",\"after_bytes\":" << frame.after
                << ",\"pose\":[" << p.x << ',' << p.y << ',' << p.z << ',' << q.x << ',' << q.y << ',' << q.z << ',' << q.w << "]}";
        }
        out << "],\"checkpoints\":[";
        for (unsigned index = 0; index != checkpoints.size(); ++index) {
            if (index) out << ',';
            const auto& point = checkpoints[index];
            out << "{\"name\":\"" << point.name << "\",\"bytes\":" << point.bytes << ",\"frames\":" << point.frames << ",\"alive\":[";
            for (unsigned n = 0; n != point.alive.size(); ++n) { if (n) out << ','; out << point.alive[n]; }
            out << "]}";
        }
        out << "]}\n";
        out.flush();
        check(static_cast<bool>(out), "lifecycle truth write failed");
    }

    void checkpoint(const Options& options, const char* name) {
        check(errors.failures.load() == 0, "PhysX reported an error");
        check(disconnected || pvd->isConnected(false), "PVD disconnected unexpectedly");
        Checkpoint point{name, offset(), static_cast<unsigned>(frames.size()), {}};
        for (unsigned index = 0; index != rooms.size(); ++index) if (rooms[index].scene) point.alive.push_back(index + 1);
        checkpoints.push_back(point);
        write(options);
        std::cout << "PVD_ROOMS " << name << ' ' << point.bytes << ' ' << point.frames << std::endl;
    }
};

inline int run(int argc, char** argv) {
    const Options options(argc, argv);
    Context context;
    context.initialize(options);
    context.create(0, true);
    context.create(1, true);
    context.create(2, false);
    context.checkpoint(options, "ready");
    options.wait("destroy-zero");
    context.destroy(2);
    context.checkpoint(options, "zero-destroyed");
    options.wait("step");
    for (unsigned n = 0; n != 2; ++n) { context.step(0); context.step(1); }
    context.checkpoint(options, "frames");
    options.wait("destroy-b");
    context.destroy(1);
    context.checkpoint(options, "b-destroyed");
    options.wait("replace");
    context.create(3, true);
    for (unsigned n = 0; n != 2; ++n) { context.step(3); context.step(0); }
    context.checkpoint(options, "replacement");
    options.wait("destroy-a");
    context.destroy(0);
    context.checkpoint(options, "tail-destroyed");
    options.wait("disconnect");
    context.pvd->disconnect();
    context.disconnected = true;
    context.checkpoint(options, "disconnected");
    options.wait("finish");
    return 0;
}
}

inline int run_lifecycle(int argc, char** argv) {
    try { return room_lifecycle::run(argc, argv); }
    catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
