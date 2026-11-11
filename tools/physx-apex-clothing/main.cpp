#include "feature_harness.h"
#include "ApexSDK.h"
#include "Asset.h"
#include "Module.h"
#include "Scene.h"
#include "ResourceCallback.h"
#include "UserRenderResourceManager.h"
#include "clothing/ClothingActor.h"
#include "clothing/ClothingAsset.h"
#include "clothing/ModuleClothing.h"
#include "nvparameterized/NvParamUtils.h"
#include "filebuf/PxFileBuf.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>

using namespace nvidia::apex;

static void require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}

static std::uintptr_t id(const void* value) { return reinterpret_cast<std::uintptr_t>(value); }

class NoRenderResources final : public UserRenderResourceManager {
public:
    UserRenderVertexBuffer* createVertexBuffer(const UserRenderVertexBufferDesc&) override { return nullptr; }
    void releaseVertexBuffer(UserRenderVertexBuffer&) override {}
    UserRenderIndexBuffer* createIndexBuffer(const UserRenderIndexBufferDesc&) override { return nullptr; }
    void releaseIndexBuffer(UserRenderIndexBuffer&) override {}
    UserRenderBoneBuffer* createBoneBuffer(const UserRenderBoneBufferDesc&) override { return nullptr; }
    void releaseBoneBuffer(UserRenderBoneBuffer&) override {}
    UserRenderInstanceBuffer* createInstanceBuffer(const UserRenderInstanceBufferDesc&) override { return nullptr; }
    void releaseInstanceBuffer(UserRenderInstanceBuffer&) override {}
    UserRenderSpriteBuffer* createSpriteBuffer(const UserRenderSpriteBufferDesc&) override { return nullptr; }
    void releaseSpriteBuffer(UserRenderSpriteBuffer&) override {}
    UserRenderSurfaceBuffer* createSurfaceBuffer(const UserRenderSurfaceBufferDesc&) override { return nullptr; }
    void releaseSurfaceBuffer(UserRenderSurfaceBuffer&) override {}
    UserRenderResource* createResource(const UserRenderResourceDesc&) override { return nullptr; }
    void releaseResource(UserRenderResource&) override {}
    uint32_t getMaxBonesForMaterial(void*) override { return 0; }
    bool getSpriteLayoutData(uint32_t, uint32_t, UserRenderSpriteBufferDesc*) override { return false; }
    bool getInstanceLayoutData(uint32_t, uint32_t, UserRenderInstanceBufferDesc*) override { return false; }
};

class NoExternalResources final : public ResourceCallback {
public:
    void* requestResource(const char* space, const char* name) override {
        std::cerr << "External resource requested: " << space << '/' << name << '\n';
        return nullptr;
    }
    void releaseResource(const char*, const char*, void*) override {}
};

static NvParameterized::Handle parameter(const NvParameterized::Interface& object, const char* path) {
    NvParameterized::Handle handle(object);
    require(object.getParameterHandle(path, handle) == NvParameterized::ERROR_NONE, path);
    return handle;
}

static uint32_t scalar(const NvParameterized::Interface& object, const char* path) {
    uint32_t result = 0;
    require(parameter(object, path).getParamU32(result) == NvParameterized::ERROR_NONE, path);
    return result;
}

static NvParameterized::Interface& reference(const NvParameterized::Interface& object, const char* path) {
    NvParameterized::Interface* result = nullptr;
    require(parameter(object, path).getParamRef(result) == NvParameterized::ERROR_NONE && result, path);
    return *result;
}

static void vector(std::ostream& out, PxVec3 value) {
    require(value.isFinite(), "non-finite getter position");
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void matrix(std::ostream& out, const PxMat44& value) {
    out << '[';
    for (unsigned column = 0; column != 4; ++column) {
        for (unsigned row = 0; row != 4; ++row) {
            if (column || row) out << ',';
            out << value(row, column);
        }
    }
    out << ']';
}

struct ActorEntry {
    ClothingActor* actor = nullptr;
    bool local_space = false;
    unsigned serial = 0;
};

struct PhysicsContext {
    PxDefaultAllocator allocator;
    PxDefaultErrorCallback errors;
    PxFoundation* foundation = nullptr;
    PxPvdTransport* transport = nullptr;
    PxPvd* pvd = nullptr;
    PxPhysics* physics = nullptr;
    PxCooking* cooking = nullptr;
    PxDefaultCpuDispatcher* dispatcher = nullptr;
    PxScene* scene = nullptr;
    bool extensions = false;

    explicit PhysicsContext(const FeatureConnection& connection) {
        try {
            foundation = feature_require(PxCreateFoundation(PX_FOUNDATION_VERSION, allocator, errors), "foundation");
            pvd = feature_require(PxCreatePvd(*foundation), "PVD client");
            transport = feature_require(connection.host
                ? PxDefaultPvdSocketTransportCreate(connection.host, static_cast<int>(connection.port), connection.timeout_ms)
                : PxDefaultPvdFileTransportCreate(connection.capture), "PVD transport");
            physics = feature_require(PxCreatePhysics(PX_PHYSICS_VERSION, *foundation, PxTolerancesScale(), true, pvd), "physics");
            extensions = PxInitExtensions(*physics, pvd);
            require(extensions, "PhysX extensions initialization failed");
            cooking = feature_require(PxCreateCooking(PX_PHYSICS_VERSION, *foundation, PxCookingParams(physics->getTolerancesScale())), "cooking");
            dispatcher = feature_require(PxDefaultCpuDispatcherCreate(1), "dispatcher");
            PxSceneDesc description(physics->getTolerancesScale());
            description.gravity = PxVec3(0, -9.81f, 0);
            description.cpuDispatcher = dispatcher;
            description.filterShader = PxDefaultSimulationFilterShader;
            scene = feature_require(physics->createScene(description), "PhysX scene");
        } catch (...) {
            release();
            throw;
        }
    }

    void connect(PxPvdInstrumentationFlags instrumentation) {
        require(pvd->connect(*transport, instrumentation), "PVD connection failed");
    }

    void flush_and_require_connected() {
        std::lock_guard<PxPvdTransport> lock(*transport);
        transport->flush();
        require(pvd->isConnected(false), "PVD transport connection lost during capture");
    }

    void release() {
        if (scene) scene->release();
        if (cooking) cooking->release();
        if (dispatcher) dispatcher->release();
        if (extensions) PxCloseExtensions();
        if (physics) physics->release();
        if (pvd) pvd->release();
        if (transport) transport->release();
        if (foundation) foundation->release();
    }

    ~PhysicsContext() { release(); }
};

struct ApexContext {
    PhysicsContext physics;
    NoRenderResources renderer;
    NoExternalResources resources;
    ApexSDK* sdk = nullptr;
    Scene* scene = nullptr;
    ClothingAsset* asset = nullptr;
    ActorEntry actors[2];
    std::vector<uint32_t> indices;
    uint32_t physical_vertices = 0;

    ApexContext(const FeatureConnection& connection, const char* asset_path, bool async_fetch, bool debug_only)
        : physics(connection) {
        try {
            ApexSDKDesc description;
            description.foundation = physics.foundation;
            description.physXSDK = physics.physics;
            description.cooking = physics.cooking;
            description.pvd = physics.pvd;
            description.renderResourceManager = &renderer;
            description.resourceCallback = &resources;
            require(description.isValid(), "invalid APEX SDK description");
            ApexCreateError creation_error;
            sdk = feature_require(CreateApexSDK(description, &creation_error), "APEX SDK");
            feature_require(sdk->createModule("Legacy", &creation_error), "APEX Legacy module");
            auto* clothing = static_cast<ModuleClothing*>(feature_require(
                sdk->createModule("Clothing", &creation_error), "APEX Clothing module"));
            auto* module_params = feature_require(clothing->getDefaultModuleDesc(), "Clothing module parameters");
            require(NvParameterized::setParamBool(*module_params, "asyncFetchResults", async_fetch), "asyncFetchResults parameter");
            clothing->init(*module_params);
            SceneDesc scene_description;
            scene_description.scene = physics.scene;
            scene = feature_require(sdk->createScene(scene_description), "APEX scene");
            load(asset_path);
            physics.connect(debug_only ? PxPvdInstrumentationFlag::eDEBUG : PxPvdInstrumentationFlag::eALL);
        } catch (...) {
            release();
            throw;
        }
    }

    void load(const char* path) {
        auto* stream = feature_require(sdk->createStream(path, nvidia::PxFileBuf::OPEN_READ_ONLY), "asset stream");
        if (!stream->isOpen()) {
            stream->release();
            throw std::runtime_error("cannot open Clothing asset");
        }
        auto* serializer = sdk->createSerializer(sdk->getSerializeType(*stream));
        if (!serializer) {
            stream->release();
            throw std::runtime_error("cannot create APEX serializer");
        }
        NvParameterized::Serializer::DeserializedData data;
        const auto result = serializer->deserialize(*stream, data);
        stream->release();
        serializer->release();
        require(result == NvParameterized::Serializer::ERROR_NONE && data.size() == 1, "asset deserialization failed");
        auto* loaded = sdk->createAsset(data[0], "plane32");
        if (!loaded) {
            data[0]->destroy();
            throw std::runtime_error("cannot create Clothing asset");
        }
        asset = static_cast<ClothingAsset*>(loaded);
        const auto& params = *feature_require(asset->getAssetNvParameterized(), "asset parameters");
        auto& mesh = reference(params, "physicalMeshes[0]");
        physical_vertices = scalar(mesh, "physicalMesh.numVertices");
        auto handle = parameter(mesh, "physicalMesh.indices");
        int32_t count = 0;
        require(handle.getArraySize(count) == NvParameterized::ERROR_NONE, "physical index count");
        require(count > 0 && count <= 1000000 && count % 3 == 0, "physical index count outside probe budget");
        require(physical_vertices > 0 && physical_vertices <= 100000, "physical vertex count outside probe budget");
        indices.resize(static_cast<std::size_t>(count));
        require(handle.getParamU32Array(indices.data(), count) == NvParameterized::ERROR_NONE, "physical index data");
        for (uint32_t index : indices) require(index < physical_vertices, "physical index outside vertices");
    }

    void create(unsigned slot, unsigned serial, bool local_space, float x) {
        require(!actors[slot].actor, "actor slot already occupied");
        auto* params = feature_require(asset->getDefaultActorDesc(), "actor parameters");
        require(NvParameterized::setParamBool(*params, "useHardwareCloth", false), "CPU cloth parameter");
        require(NvParameterized::setParamBool(*params, "localSpaceSim", local_space), "local space parameter");
        require(NvParameterized::setParamBool(*params, "flags.ComputeRenderData", false), "render data parameter");
        PxMat44 pose(PxTransform(PxVec3(x, 6.0f, -120.0f), PxQuat(0.31f, PxVec3(0, 1, 0))));
        require(NvParameterized::setParamMat44(*params, "globalPose", pose), "global pose parameter");
        auto* actor = static_cast<ClothingActor*>(feature_require(asset->createApexActor(*params, *scene), "Clothing actor"));
        actors[slot] = ActorEntry{actor, local_space, serial};
    }

    void destroy(unsigned slot) {
        if (actors[slot].actor) actors[slot].actor->release();
        actors[slot].actor = nullptr;
    }

    uint64_t offset() {
        physics.flush_and_require_connected();
        std::lock_guard<PxPvdTransport> lock(*physics.transport);
        return physics.transport->getWrittenDataSize();
    }

    void write_actor(std::ostream& out, ActorEntry& entry) {
        auto& actor = *entry.actor;
        const PxVec3* positions = feature_require(actor.getSimulationPositions(), "simulation positions");
        const uint32_t count = actor.getNumSimulationVertices();
        require(count > 0 && count <= physical_vertices, "simulation vertex count outside physical mesh");
        const PxMat44 render_pose = actor.getRenderGlobalPose();
        out << "{\"serial\":" << entry.serial << ",\"api_id\":" << id(&actor)
            << ",\"actor_params_id\":" << id(actor.getActorDesc()) << ",\"local_space\":" << entry.local_space
            << ",\"simulation_vertices\":" << count << ",\"global_pose\":";
        matrix(out, actor.getGlobalPose());
        out << ",\"render_pose\":";
        matrix(out, render_pose);
        out << ",\"simulation_positions\":[";
        for (uint32_t index = 0; index != count; ++index) {
            if (index) out << ',';
            vector(out, positions[index]);
        }
        out << "],\"world_positions\":[";
        for (uint32_t index = 0; index != count; ++index) {
            if (index) out << ',';
            vector(out, render_pose.transform(positions[index]));
        }
        out << "]}";
    }

    void release() {
        destroy(0);
        destroy(1);
        if (asset) { asset->release(); asset = nullptr; }
        if (scene) { scene->release(); scene = nullptr; }
        if (sdk) { sdk->release(); sdk = nullptr; }
    }

    ~ApexContext() { release(); }
};

struct Options {
    FeatureConnection connection;
    const char* truth = nullptr;
    const char* asset = nullptr;
    bool async_fetch = false;
    bool debug_only = false;
    bool controlled = false;
    unsigned cycles = 1;

    Options(int argc, char** argv) {
        const char* usage = "Usage: pvd-apex-clothing capture.pvdstream truth.json plane32.apx [--async-fetch] [--debug-only] [--cycles 1..32]\n"
            "       pvd-apex-clothing --host HOST --port PORT truth.json plane32.apx [--async-fetch] [--debug-only] [--step-control] [--cycles 1..32]";
        require(argc >= 4, usage);
        int first_option = 4;
        if (std::string(argv[1]) == "--host") {
            require(argc >= 7 && std::string(argv[3]) == "--port", usage);
            const std::string port_text(argv[4]);
            require(!port_text.empty() && port_text.find_first_not_of("0123456789") == std::string::npos, "port must be an integer");
            const unsigned long port = std::stoul(port_text);
            require(*argv[2] && port > 0 && port <= 65535, "socket host and port 1..65535 are required");
            connection = FeatureConnection::socket(argv[2], static_cast<unsigned>(port));
            truth = argv[5];
            asset = argv[6];
            first_option = 7;
        } else {
            connection = FeatureConnection::file(argv[1]);
            truth = argv[2];
            asset = argv[3];
        }
        bool cycles_set = false;
        for (int index = first_option; index != argc; ++index) {
            const std::string option(argv[index]);
            if (option == "--async-fetch" && !async_fetch) async_fetch = true;
            else if (option == "--debug-only" && !debug_only) debug_only = true;
            else if (option == "--step-control" && !controlled) controlled = true;
            else if (option == "--cycles" && !cycles_set) {
                require(++index < argc, "--cycles requires a count");
                const std::string count(argv[index]);
                require(!count.empty() && count.find_first_not_of("0123456789") == std::string::npos,
                    "cycle count must be an integer");
                const unsigned long parsed = std::stoul(count);
                require(parsed >= 1 && parsed <= 32, "cycle count must be 1..32");
                cycles = static_cast<unsigned>(parsed);
                cycles_set = true;
            }
            else throw std::runtime_error("unsupported or repeated producer option: " + option);
        }
        require(!controlled || connection.host, "step control requires a socket connection");
    }

    void wait_for(const std::string& expected) const {
        if (!controlled) return;
        std::string command;
        require(static_cast<bool>(std::getline(std::cin, command)), "step control input ended");
        require(command == expected, "unexpected step control command");
    }
};

int main(int argc, char** argv) {
    try {
        const Options options(argc, argv);
        const bool async_fetch = options.async_fetch;
        const bool debug_only = options.debug_only;
        std::ofstream truth(options.truth, std::ios::trunc);
        require(static_cast<bool>(truth), "cannot open truth output");
        truth << std::setprecision(9) << std::boolalpha;
        ApexContext context(options.connection, options.asset, async_fetch, debug_only);
        truth << "{\"sdk\":\"" << feature_sdk_version()
            << "\",\"apex_commit\":\"5e42a5f112351a223c19c17bb331e6c55037b8eb\",\"async_fetch\":" << async_fetch
            << ",\"instrumentation\":\"" << (debug_only ? "DEBUG" : "ALL") << '"'
            << ",\"transport\":\"" << (options.connection.host ? "socket" : "file") << '"'
            << ",\"controlled_steps\":" << options.controlled
            << ",\"cycles\":" << options.cycles
            << ",\"physx_scene_id\":" << id(context.physics.scene) << ",\"apex_scene_api_id\":" << id(context.scene)
            << ",\"asset_params_id\":" << id(context.asset->getAssetNvParameterized())
            << ",\"physical_vertices\":" << context.physical_vertices << ",\"physical_indices\":[";
        for (std::size_t index = 0; index != context.indices.size(); ++index) {
            if (index) truth << ',';
            truth << context.indices[index];
        }
        truth << "],\"steps\":[";
        if (options.controlled) {
            std::cout << "PVD_APEX_READY " << id(context.physics.scene) << ' ' << id(context.scene)
                << ' ' << context.offset() << std::endl;
        }
        for (unsigned step = 0; step != 36 * options.cycles; ++step) {
            options.wait_for("step " + std::to_string(step));
            const unsigned phase = step % 36;
            const unsigned serial = step / 36 * 3;
            if (phase == 0) {
                context.destroy(0);
                context.create(0, serial, false, -55.0f);
            }
            if (phase == 4) context.create(1, serial + 1, true, 55.0f);
            if (phase == 16) context.destroy(0);
            if (phase == 20) context.create(0, serial + 2, false, -35.0f);
            if (phase == 30) context.destroy(1);
            if (phase == 8 || phase == 24) {
                for (auto& entry : context.actors) if (entry.actor) {
                    PxMat44 pose = entry.actor->getGlobalPose();
                    pose.setPosition(pose.getPosition() + PxVec3(3.0f, 2.0f, -1.0f));
                    entry.actor->updateState(pose, nullptr, 0, 0, ClothingTeleportMode::Teleport);
                }
            }
            for (auto& entry : context.actors) if (entry.actor)
                entry.actor->setWind(0.35f, PxVec3(0.25f * phase, 0.0f, 4.0f));
            const auto before = context.offset();
            context.scene->simulate(1.0f / 60.0f);
            const auto after_simulate = context.offset();
            uint32_t error = 0;
            require(context.scene->fetchResults(true, &error) && error == 0, "APEX fetchResults failed");
            const auto after_fetch = context.offset();
            if (step) truth << ',';
            truth << "{\"step\":" << step << ",\"before_simulate_bytes\":" << before
                << ",\"after_simulate_bytes\":" << after_simulate << ",\"after_fetch_bytes\":" << after_fetch
                << ",\"actors\":[";
            bool first = true;
            for (auto& entry : context.actors) if (entry.actor) {
                if (!first) truth << ',';
                first = false;
                context.write_actor(truth, entry);
            }
            const auto after_getters = context.offset();
            truth << "],\"after_getters_bytes\":" << after_getters << '}';
            if (options.controlled) {
                truth.flush();
                require(static_cast<bool>(truth), "truth write failed");
                std::cout << "PVD_APEX_STEP " << step << ' ' << after_getters << std::endl;
            }
        }
        options.wait_for("finish");
        const auto before_release = context.offset();
        context.release();
        truth << "],\"before_apex_release_bytes\":" << before_release
            << ",\"after_apex_release_bytes\":" << context.offset() << "}\n";
        truth.flush();
        require(static_cast<bool>(truth), "truth write failed");
        std::cout << 36 * options.cycles << " APEX CPU Clothing steps, shared asset, world/local simulation, actor creation/replacement/destruction; instrumentation="
            << (debug_only ? "DEBUG" : "ALL") << "; asyncFetchResults=" << async_fetch << '\n';
        return 0;
    } catch (const std::exception& problem) {
        std::cerr << problem.what() << '\n';
        return 1;
    }
}
