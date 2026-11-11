#pragma once

#include "PxPhysicsAPI.h"
#include <mutex>
#include <stdexcept>
#include <string>

using namespace physx;

inline std::string feature_sdk_version() {
    return std::to_string(PX_PHYSICS_VERSION_MAJOR) + "." +
        std::to_string(PX_PHYSICS_VERSION_MINOR) + "." + std::to_string(PX_PHYSICS_VERSION_BUGFIX);
}

template <typename T> T* feature_require(T* pointer, const char* name) {
    if (!pointer) throw std::runtime_error(std::string("Cannot create ") + name);
    return pointer;
}

struct FeatureConnection {
    const char* capture = nullptr;
    const char* host = nullptr;
    unsigned port = 5425;
    unsigned timeout_ms = 1000;

    static FeatureConnection file(const char* path) {
        FeatureConnection connection;
        connection.capture = path;
        return connection;
    }

    static FeatureConnection socket(const char* address, unsigned port_number) {
        FeatureConnection connection;
        connection.host = address;
        connection.port = port_number;
        return connection;
    }
};

struct FeatureContext {
    PxDefaultAllocator allocator;
    PxDefaultErrorCallback errors;
    PxFoundation* foundation = nullptr;
    PxPvdTransport* transport = nullptr;
    PxPvd* pvd = nullptr;
    PxPhysics* physics = nullptr;
    PxCooking* cooking = nullptr;
    PxDefaultCpuDispatcher* dispatcher = nullptr;
    PxScene* scene = nullptr;
    PxMaterial* material = nullptr;
    bool extensions = false;

    explicit FeatureContext(const char* capture,
        PxPvdInstrumentationFlags instrumentation = PxPvdInstrumentationFlag::eALL)
        : FeatureContext(FeatureConnection::file(capture), instrumentation) {}

    explicit FeatureContext(const FeatureConnection& connection,
        PxPvdInstrumentationFlags instrumentation = PxPvdInstrumentationFlag::eALL) {
        try {
            if ((connection.capture == nullptr) == (connection.host == nullptr)) {
                throw std::runtime_error("Specify exactly one PVD file path or socket host");
            }
            if (connection.host && (!*connection.host || connection.port == 0 || connection.port > 65535)) {
                throw std::runtime_error("PVD socket requires a nonempty host and port 1..65535");
            }
            foundation = feature_require(PxCreateFoundation(PX_FOUNDATION_VERSION, allocator, errors), "foundation");
            pvd = feature_require(PxCreatePvd(*foundation), "PVD client");
            transport = feature_require(connection.host
                ? PxDefaultPvdSocketTransportCreate(connection.host, static_cast<int>(connection.port), connection.timeout_ms)
                : PxDefaultPvdFileTransportCreate(connection.capture), "PVD transport");
            if (!pvd->connect(*transport, instrumentation)) {
                throw std::runtime_error(connection.host ? "PVD socket connection failed" : "PVD file connection failed");
            }
            physics = feature_require(PxCreatePhysics(PX_PHYSICS_VERSION, *foundation, PxTolerancesScale(), true, pvd), "physics");
            extensions = PxInitExtensions(*physics, pvd);
            if (!extensions) throw std::runtime_error("PhysX extensions initialization failed");
            cooking = feature_require(PxCreateCooking(PX_PHYSICS_VERSION, *foundation, PxCookingParams(physics->getTolerancesScale())), "cooking");
            dispatcher = feature_require(PxDefaultCpuDispatcherCreate(1), "dispatcher");
            PxSceneDesc settings(physics->getTolerancesScale());
            settings.gravity = PxVec3(0, -9.81f, 0);
            settings.cpuDispatcher = dispatcher;
            settings.filterShader = PxDefaultSimulationFilterShader;
            scene = feature_require(physics->createScene(settings), "scene");
            if (auto* client = scene->getScenePvdClient()) {
                client->setScenePvdFlag(PxPvdSceneFlag::eTRANSMIT_CONSTRAINTS, true);
                client->setScenePvdFlag(PxPvdSceneFlag::eTRANSMIT_CONTACTS, true);
                client->setScenePvdFlag(PxPvdSceneFlag::eTRANSMIT_SCENEQUERIES, true);
            }
            scene->setVisualizationParameter(PxVisualizationParameter::eSCALE, 1.0f);
            scene->setVisualizationParameter(PxVisualizationParameter::eJOINT_LOCAL_FRAMES, 1.0f);
            scene->setVisualizationParameter(PxVisualizationParameter::eJOINT_LIMITS, 1.0f);
            material = feature_require(physics->createMaterial(0.5f, 0.5f, 0.2f), "material");
        } catch (...) {
            release();
            throw;
        }
    }

    FeatureContext(const FeatureContext&) = delete;
    FeatureContext& operator=(const FeatureContext&) = delete;

    void step(float dt = 1.0f / 60.0f) {
        scene->simulate(dt);
        scene->fetchResults(true);
    }

    void flush_and_require_connected() {
        std::lock_guard<PxPvdTransport> lock(*transport);
        transport->flush();
        if (!pvd->isConnected(false)) {
            throw std::runtime_error("PVD transport connection lost during capture");
        }
    }

    ~FeatureContext() { release(); }

private:
    void release() {
        if (scene) scene->release();
        if (material) material->release();
        if (cooking) cooking->release();
        if (dispatcher) dispatcher->release();
        if (extensions) PxCloseExtensions();
        if (physics) physics->release();
        if (pvd) pvd->release();
        if (transport) transport->release();
        if (foundation) foundation->release();
    }
};
