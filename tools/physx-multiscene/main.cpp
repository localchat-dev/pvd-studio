#define NOMINMAX
#include <Windows.h>
#include "PxPhysicsAPI.h"
#include "foundation/PxProfiler.h"
#include <array>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <map>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

using namespace physx;

namespace {
template <class T> T* require(T* value, const char* name) {
    if (!value) throw std::runtime_error(std::string("Cannot create ") + name);
    return value;
}

std::uint64_t counter() {
    LARGE_INTEGER value;
    if (!QueryPerformanceCounter(&value)) throw std::runtime_error("QPC failed");
    return static_cast<std::uint64_t>(value.QuadPart);
}

struct Errors : PxErrorCallback {
    std::atomic<unsigned> failures{0};
    void reportError(PxErrorCode::Enum code, const char* message, const char* file, int line) override {
        std::fprintf(stderr, "PhysX %u: %s (%s:%d)\n", unsigned(code), message, file, line);
        if (code != PxErrorCode::eDEBUG_INFO && code != PxErrorCode::eDEBUG_WARNING) ++failures;
    }
};

struct Probe : PxProfilerCallback {
    struct Start { unsigned local_step; std::uint64_t lower, upper; };
    struct Span { std::string event; std::uint64_t context; Start start; std::uint64_t lower, upper; };
    PxProfilerCallback* delegate;
    std::mutex mutex;
    std::map<std::uint64_t, unsigned> steps;
    std::map<std::pair<std::string, std::uint64_t>, std::vector<Start>> pending;
    std::vector<Span> spans;
    unsigned failures = 0;

    Probe() : delegate(PxGetProfilerCallback()) {
        if (!delegate) throw std::runtime_error("PVD profiler callback is missing");
        PxSetProfilerCallback(this);
    }
    ~Probe() { PxSetProfilerCallback(delegate); }
    static bool relevant(const char* event, bool detached) {
        return detached && (std::string(event) == "Basic.simulate" ||
            std::string(event) == "Basic.collision" || std::string(event) == "Basic.rigidBodySolver");
    }
    void step(std::uint64_t scene, unsigned index) {
        std::lock_guard<std::mutex> lock(mutex);
        steps[scene] = index;
    }
    void* zoneStart(const char* event, bool detached, std::uint64_t context) override {
        if (!relevant(event, detached)) return delegate->zoneStart(event, detached, context);
        std::lock_guard<std::mutex> lock(mutex);
        const auto lower = counter();
        auto* token = delegate->zoneStart(event, detached, context);
        const auto upper = counter();
        const auto step = steps.find(context);
        if (step == steps.end()) {
            ++failures;
            std::fprintf(stderr, "Unknown profile context: %s context=%llu\n",
                event, static_cast<unsigned long long>(context));
        } else {
            pending[{event, context}].push_back(Start{step->second, lower, upper});
        }
        return token;
    }
    void zoneEnd(void* token, const char* event, bool detached, std::uint64_t context) override {
        if (!relevant(event, detached)) { delegate->zoneEnd(token, event, detached, context); return; }
        std::lock_guard<std::mutex> lock(mutex);
        const auto lower = counter();
        delegate->zoneEnd(token, event, detached, context);
        const auto upper = counter();
        const auto start = pending.find({event, context});
        if (start == pending.end() || start->second.empty()) {
            ++failures;
            std::fprintf(stderr, "Profile stop mismatch: %s context=%llu\n", event, static_cast<unsigned long long>(context));
            return;
        }
        spans.push_back({event, context, start->second.back(), lower, upper});
        start->second.pop_back();
        if (start->second.empty()) pending.erase(start);
    }
};

struct Scene {
    PxScene* value = nullptr;
    PxRigidDynamic* box = nullptr;
    PxRigidStatic* ground = nullptr;
    PxTransform published{PxIdentity};
    unsigned local_step = 0;
    bool pending = false;
    std::uint64_t id() const { return reinterpret_cast<std::uintptr_t>(value); }
};

struct Simulation {
    PxDefaultAllocator allocator;
    Errors errors;
    PxFoundation* foundation = nullptr;
    PxPvdTransport* transport = nullptr;
    PxPvd* pvd = nullptr;
    PxPhysics* physics = nullptr;
    PxDefaultCpuDispatcher* dispatcher = nullptr;
    PxMaterial* material = nullptr;
    std::array<Scene, 2> scenes;
    std::unique_ptr<Probe> probe;

    ~Simulation() {
        for (auto& scene : scenes) if (scene.pending) scene.value->fetchResults(true);
        probe.reset();
        for (auto& scene : scenes) {
            if (scene.box) scene.box->release();
            if (scene.ground) scene.ground->release();
            if (scene.value) scene.value->release();
        }
        if (material) material->release();
        if (dispatcher) dispatcher->release();
        if (physics) physics->release();
        if (pvd) { pvd->disconnect(); pvd->release(); }
        if (transport) transport->release();
        if (foundation) foundation->release();
    }

    void initialize(const char* path) {
        foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, allocator, errors), "foundation");
        pvd = require(PxCreatePvd(*foundation), "PVD");
        transport = require(PxDefaultPvdFileTransportCreate(path), "file transport");
        if (!pvd->connect(*transport, PxPvdInstrumentationFlag::eALL)) throw std::runtime_error("PVD connection failed");
        physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *foundation, PxTolerancesScale(), false, pvd), "physics");
        dispatcher = require(PxDefaultCpuDispatcherCreate(4), "dispatcher");
        material = require(physics->createMaterial(0.5f, 0.5f, 0.2f), "material");
        for (unsigned index = 0; index < scenes.size(); ++index) {
            auto& scene = scenes[index];
            PxSceneDesc description(physics->getTolerancesScale());
            description.gravity = PxVec3(0, index ? -5.0f : -9.81f, 0);
            description.cpuDispatcher = dispatcher;
            description.filterShader = PxDefaultSimulationFilterShader;
            scene.value = require(physics->createScene(description), "scene");
            scene.ground = require(PxCreatePlane(*physics, PxPlane(0, 1, 0, 0), *material), "ground");
            scene.value->addActor(*scene.ground);
            scene.box = require(PxCreateDynamic(*physics, PxTransform(PxVec3(index ? 12.0f : -12.0f, 4, 0)),
                PxBoxGeometry(0.5f, 0.5f, 0.5f), *material, 10.0f), "box");
            scene.box->setName(index ? "AsyncBoxB" : "AsyncBoxA");
            scene.box->setLinearVelocity(PxVec3(index ? -0.25f : 0.5f, 0, 0));
            scene.value->addActor(*scene.box);
            scene.published = scene.box->getGlobalPose();
        }
        probe.reset(new Probe());
        check();
    }

    void check() const {
        if (errors.failures.load()) throw std::runtime_error("PhysX reported an error");
        if (!pvd->isConnected(false)) throw std::runtime_error("PVD disconnected");
    }

    void start(unsigned index) {
        auto& scene = scenes[index];
        if (scene.pending) throw std::runtime_error("Scene already simulating");
        probe->step(scene.id(), scene.local_step);
        scene.value->simulate(index ? 1.0f / 30.0f : 1.0f / 60.0f);
        scene.pending = true;
        check();
    }
};

struct Frame {
    unsigned scene, local_step;
    std::uint64_t lower, upper;
    std::array<PxTransform, 2> poses;
};

void finish(Simulation& simulation, unsigned index, std::vector<Frame>& frames) {
    auto& scene = simulation.scenes[index];
    if (!scene.pending) throw std::runtime_error("No pending simulation");
    // EndSection precedes dynamic actor messages in the SDK; this is the published state at that boundary.
    Frame frame{index, scene.local_step, 0, 0, {simulation.scenes[0].published, simulation.scenes[1].published}};
    frame.lower = counter();
    const bool fetched = scene.value->fetchResults(true);
    frame.upper = counter();
    if (!fetched) throw std::runtime_error("fetchResults failed");
    scene.pending = false;
    simulation.check();
    scene.published = scene.box->getGlobalPose();
    ++scene.local_step;
    frames.push_back(frame);
}

void pose(std::ostream& out, const PxTransform& value) {
    out << '[' << value.p.x << ',' << value.p.y << ',' << value.p.z << ',' << value.q.x << ','
        << value.q.y << ',' << value.q.z << ',' << value.q.w << ']';
}

void write_truth(std::ostream& out, Simulation& simulation, const std::vector<Frame>& frames) {
    LARGE_INTEGER frequency;
    if (!QueryPerformanceFrequency(&frequency)) throw std::runtime_error("QPC frequency failed");
    std::lock_guard<std::mutex> lock(simulation.probe->mutex);
    if (simulation.probe->failures) {
        std::fprintf(stderr, "Profile failures=%u pending=%zu scenes=%llu,%llu\n", simulation.probe->failures,
            simulation.probe->pending.size(), static_cast<unsigned long long>(simulation.scenes[0].id()),
            static_cast<unsigned long long>(simulation.scenes[1].id()));
        throw std::runtime_error("Profile callback mismatch");
    }
    out << std::setprecision(std::numeric_limits<float>::max_digits10);
    out << "{\"schema\":1,\"sdk\":\"3.4.2\",\"instrumentation\":\"eALL\",\"rounds\":48,\"counter_frequency\":"
        << frequency.QuadPart << ",\"scenes\":[";
    for (unsigned index = 0; index < 2; ++index) {
        if (index) out << ',';
        const auto& scene = simulation.scenes[index];
        out << "{\"id\":" << scene.id() << ",\"actor\":" << reinterpret_cast<std::uintptr_t>(scene.box)
            << ",\"steps\":" << scene.local_step << '}';
    }
    out << "],\"frames\":[";
    for (std::size_t index = 0; index < frames.size(); ++index) {
        if (index) out << ',';
        const auto& frame = frames[index];
        out << "{\"frame\":" << index << ",\"scene\":" << simulation.scenes[frame.scene].id()
            << ",\"local_step\":" << frame.local_step << ",\"lower\":" << frame.lower << ",\"upper\":" << frame.upper
            << ",\"poses\":[";
        pose(out, frame.poses[0]); out << ','; pose(out, frame.poses[1]); out << "]}";
    }
    out << "],\"spans\":[";
    for (std::size_t index = 0; index < simulation.probe->spans.size(); ++index) {
        if (index) out << ',';
        const auto& span = simulation.probe->spans[index];
        out << "{\"event\":\"" << span.event << "\",\"context\":" << span.context
            << ",\"local_step\":" << span.start.local_step << ",\"start_lower\":" << span.start.lower
            << ",\"start_upper\":" << span.start.upper << ",\"end_lower\":" << span.lower
            << ",\"end_upper\":" << span.upper << '}';
    }
    out << "],\"open_spans\":[";
    bool first = true;
    for (const auto& group : simulation.probe->pending) {
        for (const auto& start : group.second) {
            if (!first) out << ',';
            first = false;
            out << "{\"event\":\"" << group.first.first << "\",\"context\":" << group.first.second
                << ",\"local_step\":" << start.local_step << ",\"start_lower\":" << start.lower
                << ",\"start_upper\":" << start.upper << '}';
        }
    }
    out << "]}\n";
    out.flush();
    if (!out) throw std::runtime_error("Writing truth failed");
}
}

int main(int argc, char** argv) {
    if (argc != 3 || std::string(argv[1]) == argv[2]) {
        std::cerr << "usage: pvd-physx-multiscene capture.pvdstream truth.json\n";
        return 2;
    }
    try {
        std::ofstream truth(argv[2], std::ios::binary | std::ios::trunc);
        if (!truth) throw std::runtime_error("Cannot open truth output");
        Simulation simulation;
        simulation.initialize(argv[1]);
        std::vector<Frame> frames;
        for (unsigned round = 0; round < 48; ++round) {
            simulation.start(0);
            simulation.start(1);
            finish(simulation, round % 2, frames);
            finish(simulation, 1 - round % 2, frames);
            simulation.start(0);
            finish(simulation, 0, frames);
        }
        write_truth(truth, simulation, frames);
        std::cout << "144 merged boundaries; Scene A=96, B=48; 48 rounds with both simulate calls before either fetch\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
