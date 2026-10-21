#include "PxPhysicsAPI.h"

#include <atomic>
#include <cstdio>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <map>
#include <stdexcept>
#include <string>
#include <vector>

using namespace physx;

namespace {

template <typename T> T* require(T* value, const char* name) {
    if (!value) throw std::runtime_error(std::string("Cannot create ") + name);
    return value;
}

struct Options {
    std::string capture;
    std::string truth;
    unsigned frames = 133;
};

Options options(int argc, char** argv) {
    Options result;
    for (int index = 1; index < argc; ++index) {
        const std::string flag = argv[index];
        if (index + 1 >= argc) throw std::runtime_error("Missing value for " + flag);
        const std::string value = argv[++index];
        if (flag == "--file") result.capture = value;
        else if (flag == "--truth") result.truth = value;
        else if (flag == "--frames") {
            std::size_t consumed = 0;
            const auto count = std::stoul(value, &consumed);
            if (consumed != value.size() || count < 100 || count > 10000)
                throw std::runtime_error("--frames must be between 100 and 10000");
            result.frames = static_cast<unsigned>(count);
        } else throw std::runtime_error("Unknown option: " + flag);
    }
    if (result.capture.empty() || result.truth.empty() || result.capture == result.truth)
        throw std::runtime_error("Use --file capture.pvdstream --truth truth.json [--frames 133]");
    return result;
}

class Errors : public PxErrorCallback {
public:
    std::atomic<unsigned> failures{0};

    void reportError(PxErrorCode::Enum code, const char* message, const char* file, int line) override {
        std::fprintf(stderr, "PhysX %u: %s (%s:%d)\n", static_cast<unsigned>(code), message, file, line);
        if (code != PxErrorCode::eDEBUG_INFO && code != PxErrorCode::eDEBUG_WARNING)
            failures.fetch_add(1, std::memory_order_relaxed);
    }
};

struct Simulation {
    PxDefaultAllocator allocator;
    Errors errors;
    PxFoundation* foundation = nullptr;
    PxPvdTransport* transport = nullptr;
    PxPvd* pvd = nullptr;
    PxPhysics* physics = nullptr;
    PxDefaultCpuDispatcher* dispatcher = nullptr;
    PxScene* scene = nullptr;
    PxMaterial* material = nullptr;
    PxRigidStatic* ground = nullptr;
    PxRigidDynamic* box = nullptr;

    ~Simulation() {
        if (box) box->release();
        if (ground) ground->release();
        if (scene) scene->release();
        if (material) material->release();
        if (dispatcher) dispatcher->release();
        if (physics) physics->release();
        if (pvd) {
            pvd->disconnect();
            pvd->release();
        }
        if (transport) transport->release();
        if (foundation) foundation->release();
    }

    void initialize(const std::string& capture) {
        foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, allocator, errors), "foundation");
        pvd = require(PxCreatePvd(*foundation), "PVD client");
        transport = require(PxDefaultPvdFileTransportCreate(capture.c_str()), "PVD file transport");
        if (!pvd->connect(*transport, PxPvdInstrumentationFlag::eDEBUG))
            throw std::runtime_error("Cannot connect PVD file transport");
        physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *foundation, PxTolerancesScale(), false, pvd), "physics");
        dispatcher = require(PxDefaultCpuDispatcherCreate(1), "CPU dispatcher");
        PxSceneDesc description(physics->getTolerancesScale());
        description.gravity = PxVec3(0, -9.81f, 0);
        description.cpuDispatcher = dispatcher;
        description.filterShader = PxDefaultSimulationFilterShader;
        scene = require(physics->createScene(description), "scene");
        require(scene->getScenePvdClient(), "scene PVD client");
        material = require(physics->createMaterial(0.5f, 0.5f, 0.2f), "material");
        ground = require(PxCreatePlane(*physics, PxPlane(0, 1, 0, 0), *material), "ground");
        ground->setName("CameraProbeGround");
        scene->addActor(*ground);
        box = require(PxCreateDynamic(*physics, PxTransform(PxVec3(0, 4, 0)), PxBoxGeometry(0.75f, 0.75f, 0.75f),
            *material, 10.0f), "box");
        box->setName("CameraProbeBox");
        scene->addActor(*box);
        check();
    }

    void check() const {
        if (errors.failures.load(std::memory_order_relaxed))
            throw std::runtime_error("PhysX reported an error");
        if (!pvd->isConnected(false)) throw std::runtime_error("PVD file transport disconnected");
    }
};

struct Camera {
    PxVec3 position;
    PxVec3 up;
    PxVec3 target;
};

struct Call {
    unsigned sequence;
    unsigned frame;
    std::string phase;
    std::string name;
    Camera pose;
};

struct Boundary {
    unsigned sequence;
    unsigned frame;
    std::string phase;
};

struct Frame {
    unsigned frame;
    std::map<std::string, Camera> cameras;
};

struct Truth {
    unsigned sequence = 0;
    std::map<std::string, Camera> current;
    std::vector<Call> calls;
    std::vector<Boundary> boundaries;
    std::vector<Frame> frames;

    void boundary(unsigned frame, const char* phase) {
        boundaries.push_back(Boundary{sequence++, frame, phase});
    }

    void update(Simulation& simulation, unsigned frame, const char* phase, const char* name,
        const PxVec3& position, const PxVec3& up, const PxVec3& target) {
        const Camera camera{position, up, target};
        simulation.scene->getScenePvdClient()->updateCamera(name, position, up, target);
        simulation.check();
        current[name] = camera;
        calls.push_back(Call{sequence++, frame, phase, name, camera});
    }
};

void before_simulate(Simulation& simulation, Truth& truth, unsigned frame) {
    const char* phase = "before_simulate";
    switch (frame) {
    case 1:
        truth.update(simulation, frame, phase, "Main camera", PxVec3(12, 6, 18), PxVec3(0, 1, 0), PxVec3(0, 2, 0));
        break;
    case 2:
        truth.update(simulation, frame, phase, "Inspection camera", PxVec3(-8, 4, 5), PxVec3(0, 0, 1), PxVec3(0, 1, 0));
        break;
    case 4:
        truth.update(simulation, frame, phase, "Main camera", PxVec3(20, 8, 10), PxVec3(0, 1, 0), PxVec3(1, 2, 0));
        truth.update(simulation, frame, phase, "Main camera", PxVec3(8, 5, 14), PxVec3(0, 1, 0), PxVec3(0, 1, 0));
        break;
    case 8:
        truth.update(simulation, frame, phase, "Inspection camera", PxVec3(-6, 7, 12), PxVec3(0.25f, 1, 0.5f), PxVec3(0, 1, 0));
        break;
    case 20:
        truth.update(simulation, frame, phase, "Main camera", PxVec3(18, 7, -6), PxVec3(0, 1, 0), PxVec3(0, 1, 0));
        truth.update(simulation, frame, phase, "Main camera", PxVec3(9, 4, -10), PxVec3(0, 1, 0), PxVec3(0, 1, 0));
        break;
    case 64:
        truth.update(simulation, frame, phase, "Main camera", PxVec3(6, 9, 11), PxVec3(-0.5f, 1, 0.25f), PxVec3(0, 0.5f, 0));
        break;
    case 65:
        truth.update(simulation, frame, phase, "Inspection camera", PxVec3(2, 6, 13), PxVec3(1, 0, 0), PxVec3(0, 1, 0));
        truth.update(simulation, frame, phase, "Inspection camera", PxVec3(3, 8, 15), PxVec3(1, 0.25f, 0), PxVec3(0, 1, 0));
        break;
    case 96:
        truth.update(simulation, frame, phase, "Main camera", PxVec3(10, 5, 10), PxVec3(0, 1, 0), PxVec3(0, 1, 0));
        truth.update(simulation, frame, phase, "Inspection camera", PxVec3(-10, 3, 8), PxVec3(0, 0, 1), PxVec3(0, 1, 0));
        break;
    default:
        break;
    }
}

void capture(Simulation& simulation, Truth& truth, unsigned frames) {
    for (unsigned frame = 0; frame < frames; ++frame) {
        before_simulate(simulation, truth, frame);
        truth.boundary(frame, "simulate_call");
        simulation.scene->simulate(1.0f / 60.0f);
        truth.boundary(frame, "simulate_return");
        if (frame == 30) {
            if (!simulation.scene->checkResults(true)) throw std::runtime_error("Simulation did not complete");
            truth.update(simulation, frame, "after_simulate_before_fetch", "Main camera",
                PxVec3(4, 6, 16), PxVec3(0.5f, 1, 0), PxVec3(0, 1, 0));
        }
        truth.boundary(frame, "fetch_call");
        // The SDK emits EndSection(frame) inside fetchResults; later camera calls belong to its successor.
        truth.frames.push_back(Frame{frame, truth.current});
        if (!simulation.scene->fetchResults(true)) throw std::runtime_error("Fetching simulation results failed");
        truth.boundary(frame, "fetch_return");
        simulation.check();
        if (frame == 5)
            truth.update(simulation, frame, "after_fetch", "Main camera", PxVec3(14, 3, 7), PxVec3(0, 1, 0), PxVec3(0, 1, 0));
        if (frame == 40)
            truth.update(simulation, frame, "after_fetch", "Inspection camera", PxVec3(-7, 6, 2), PxVec3(0, 0, 1), PxVec3(0, 1, 0));
    }
    truth.update(simulation, frames - 1, "after_final_fetch", "Main camera",
        PxVec3(100, 200, 300), PxVec3(0, 1, 0), PxVec3(1, 2, 3));
    truth.update(simulation, frames - 1, "after_final_fetch", "Inspection camera",
        PxVec3(400, 500, 600), PxVec3(0, 0, 1), PxVec3(4, 5, 6));
    simulation.check();
}

void quoted(std::ostream& output, const std::string& text) {
    output << '"';
    for (unsigned char character : text) {
        switch (character) {
        case '"': output << "\\\""; break;
        case '\\': output << "\\\\"; break;
        case '\n': output << "\\n"; break;
        case '\r': output << "\\r"; break;
        case '\t': output << "\\t"; break;
        default:
            if (character < 0x20) {
                const char* hex = "0123456789abcdef";
                output << "\\u00" << hex[character >> 4] << hex[character & 15];
            } else output << static_cast<char>(character);
            break;
        }
    }
    output << '"';
}

void vector(std::ostream& output, const PxVec3& value) {
    output << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

void camera(std::ostream& output, const std::string& name, const Camera& value) {
    output << "{\"name\":";
    quoted(output, name);
    output << ",\"position\":";
    vector(output, value.position);
    output << ",\"up\":";
    vector(output, value.up);
    output << ",\"target\":";
    vector(output, value.target);
    output << '}';
}

void cameras(std::ostream& output, const std::map<std::string, Camera>& values) {
    output << '[';
    bool first = true;
    for (const auto& value : values) {
        if (!first) output << ',';
        camera(output, value.first, value.second);
        first = false;
    }
    output << ']';
}

void write_truth(std::ostream& output, const Options& arguments, const Truth& truth) {
    output << std::setprecision(std::numeric_limits<float>::max_digits10);
    output << "{\n\"schema\":1,\"sdk_version\":\"" << PX_PHYSICS_VERSION_MAJOR << '.'
        << PX_PHYSICS_VERSION_MINOR << '.' << PX_PHYSICS_VERSION_BUGFIX << "\",\n"
        << "\"sdk_version_number\":" << PX_PHYSICS_VERSION << ",\"instrumentation\":\"eDEBUG\",\n"
        << "\"api\":\"PxPvdSceneClient::updateCamera\",\"capture\":";
    quoted(output, arguments.capture);
    output << ",\n\"frame_count\":" << arguments.frames << ",\n\"frames\":[\n";
    for (std::size_t index = 0; index < truth.frames.size(); ++index) {
        if (index) output << ",\n";
        const auto& frame = truth.frames[index];
        output << "{\"frame\":" << frame.frame << ",\"cameras\":";
        cameras(output, frame.cameras);
        output << '}';
    }
    output << "\n],\n\"trailing_cameras\":";
    cameras(output, truth.current);
    output << ",\n\"calls\":[\n";
    for (std::size_t index = 0; index < truth.calls.size(); ++index) {
        if (index) output << ",\n";
        const auto& call = truth.calls[index];
        output << "{\"sequence\":" << call.sequence << ",\"frame\":" << call.frame << ",\"phase\":";
        quoted(output, call.phase);
        output << ",\"camera\":";
        camera(output, call.name, call.pose);
        output << '}';
    }
    output << "\n],\n\"boundaries\":[\n";
    for (std::size_t index = 0; index < truth.boundaries.size(); ++index) {
        if (index) output << ",\n";
        const auto& boundary = truth.boundaries[index];
        output << "{\"sequence\":" << boundary.sequence << ",\"frame\":" << boundary.frame << ",\"phase\":";
        quoted(output, boundary.phase);
        output << '}';
    }
    output << "\n]}\n";
    output.flush();
    if (!output) throw std::runtime_error("Writing camera truth failed");
}

}

int main(int argc, char** argv) {
    try {
        const auto arguments = options(argc, argv);
        std::ofstream truth_output(arguments.truth, std::ios::binary | std::ios::trunc);
        if (!truth_output) throw std::runtime_error("Cannot open truth file");
        Truth truth;
        {
            Simulation simulation;
            simulation.initialize(arguments.capture);
            capture(simulation, truth, arguments.frames);
        }
        write_truth(truth_output, arguments, truth);
        std::cout << "SDK " << PX_PHYSICS_VERSION_MAJOR << '.' << PX_PHYSICS_VERSION_MINOR << '.'
            << PX_PHYSICS_VERSION_BUGFIX << ": " << arguments.frames << " frames, " << truth.calls.size()
            << " updateCamera calls; capture=" << arguments.capture << "; truth=" << arguments.truth << '\n';
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
