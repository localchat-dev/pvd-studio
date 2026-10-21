#include "PxPhysicsAPI.h"
#include "PsAllocator.h"
#include "PsPvd.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

using namespace physx;

struct Options {
    std::string file;
    std::string host;
    std::string truth;
    std::string mesh_truth;
    std::string query_truth;
    std::string stats_truth;
    std::string analysis_truth;
    std::string export_repx;
    std::string import_repx;
    std::string roundtrip_repx;
    std::string flags = "debug";
    unsigned port = 5425;
    unsigned frames = 120;
    unsigned duration_seconds = 0;
    unsigned interval_ms = 0;
    unsigned export_frame = 0;
    unsigned extra_boxes = 0;
    unsigned extra_static_boxes = 0;
    unsigned resident_allocations = 0;
    unsigned analysis_batches_per_frame = 0;
    bool lifecycle = false;
    bool rich = false;
    bool emit_error = false;
    bool continuous_motion = false;
    bool frames_explicit = false;
};

unsigned number(const std::string& value, unsigned maximum) {
    std::size_t end = 0;
    const auto parsed = std::stoul(value, &end);
    if (end != value.size() || parsed > maximum || value.empty() || value[0] == '-') {
        throw std::runtime_error("Invalid number: " + value);
    }
    return static_cast<unsigned>(parsed);
}

Options parse(int argc, char** argv) {
    Options options;
    for (int index = 1; index < argc; ++index) {
        const std::string key = argv[index];
        if (key == "--lifecycle") {
            options.lifecycle = true;
            continue;
        }
        if (key == "--rich") {
            options.rich = true;
            continue;
        }
        if (key == "--emit-error") {
            options.emit_error = true;
            continue;
        }
        if (key == "--continuous-motion") {
            options.continuous_motion = true;
            continue;
        }
        if (++index >= argc) {
            throw std::runtime_error("Missing value for " + key);
        }
        const std::string value = argv[index];
        if (key == "--file") options.file = value;
        else if (key == "--host") options.host = value;
        else if (key == "--truth") options.truth = value;
        else if (key == "--mesh-truth") options.mesh_truth = value;
        else if (key == "--query-truth") options.query_truth = value;
        else if (key == "--stats-truth") options.stats_truth = value;
        else if (key == "--analysis-truth") options.analysis_truth = value;
        else if (key == "--export-repx") options.export_repx = value;
        else if (key == "--import-repx") options.import_repx = value;
        else if (key == "--roundtrip-repx") options.roundtrip_repx = value;
        else if (key == "--export-frame") options.export_frame = number(value, 999999);
        else if (key == "--flags") options.flags = value;
        else if (key == "--port") options.port = number(value, 65535);
        else if (key == "--frames") {
            options.frames = number(value, 1000000);
            options.frames_explicit = true;
        }
        else if (key == "--duration-seconds") {
            options.duration_seconds = number(value, 3600);
            if (!options.duration_seconds) throw std::runtime_error("Duration must be between 1 and 3600 seconds.");
        }
        else if (key == "--extra-boxes") options.extra_boxes = number(value, 10000);
        else if (key == "--extra-static-boxes") options.extra_static_boxes = number(value, 20000);
        else if (key == "--resident-allocations") options.resident_allocations = number(value, 200000);
        else if (key == "--analysis-batches-per-frame") options.analysis_batches_per_frame = number(value, 64);
        else if (key == "--interval-ms") options.interval_ms = number(value, 1000);
        else throw std::runtime_error("Unknown argument: " + key);
    }
    const auto modes = !options.file.empty() + !options.host.empty() + !options.import_repx.empty();
    if (modes != 1) {
        throw std::runtime_error("Choose exactly one of --file PATH, --host ADDRESS or --import-repx PATH.");
    }
    if (!options.port || !options.frames) {
        throw std::runtime_error("Port and frame count must be positive.");
    }
    if (options.duration_seconds && !options.frames_explicit) options.frames = 10000000;
    if (!options.export_repx.empty() && options.export_frame >= options.frames) {
        throw std::runtime_error("RepX export frame must be inside the generated recording.");
    }
    if (options.continuous_motion && !options.extra_boxes) {
        throw std::runtime_error("--continuous-motion requires --extra-boxes greater than zero.");
    }
    const bool analysis_load = options.resident_allocations || options.analysis_batches_per_frame;
    if (analysis_load && (options.flags != "all" || !options.import_repx.empty())) {
        throw std::runtime_error("Analysis load requires --file or --host with --flags all.");
    }
    if (!options.analysis_truth.empty() && !analysis_load) {
        throw std::runtime_error("--analysis-truth requires resident allocations or analysis batches.");
    }
    if (!options.import_repx.empty() && (options.continuous_motion || !options.stats_truth.empty()
        || options.duration_seconds || options.extra_static_boxes)) {
        throw std::runtime_error("Motion, duration and simulation statistics require --file or --host.");
    }
    return options;
}

PxPvdInstrumentationFlags instrumentation(const std::string& flags) {
    if (flags == "debug") return PxPvdInstrumentationFlag::eDEBUG;
    if (flags == "all") return PxPvdInstrumentationFlag::eALL;
    if (flags == "profile") return PxPvdInstrumentationFlag::ePROFILE;
    if (flags == "memory") return PxPvdInstrumentationFlag::eMEMORY;
    throw std::runtime_error("Flags must be debug, all, profile or memory.");
}

template <typename T> T* require(T* pointer, const char* name) {
    if (!pointer) throw std::runtime_error(std::string("Cannot create ") + name);
    return pointer;
}

#include "repx_probe.h"

struct AnalysisLoad {
    PxAllocatorCallback& allocator;
    pvdsdk::PsPvd& pvd;
    std::vector<void*> resident;
    std::ofstream truth;
    std::uint64_t allocations = 0;
    std::uint64_t frees = 0;
    std::uint64_t profile_pairs = 0;
    unsigned batches = 0;
    bool enabled = false;

    explicit AnalysisLoad(PxPvd& client)
        : allocator(shdfnd::getAllocator()), pvd(static_cast<pvdsdk::PsPvd&>(client)) {}

    ~AnalysisLoad() {
        release();
    }

    void initialize(const Options& options) {
        enabled = options.resident_allocations || options.analysis_batches_per_frame;
        batches = options.analysis_batches_per_frame;
        if (!enabled) return;
        if (!options.analysis_truth.empty()) {
            truth.open(options.analysis_truth);
            if (!truth) throw std::runtime_error("Cannot create analysis truth CSV: " + options.analysis_truth);
            truth << "phase,frame,resident_live,allocation_bytes,allocation_calls,free_calls,profile_pairs\n";
        }
        resident.reserve(options.resident_allocations);
        for (unsigned index = 0; index < options.resident_allocations; ++index) {
            auto* block = require(allocator.allocate(16, "AnalysisResident", "analysis-load", 1), "analysis resident allocation");
            resident.push_back(block);
            ++allocations;
        }
        pvd.flush();
        record("initialized", 0);
    }

    void step(unsigned frame) {
        if (!enabled) return;
        for (unsigned batch = 0; batch < batches; ++batch) {
            const auto context = static_cast<std::uint64_t>(frame) * 64 + batch;
            auto* token = pvd.zoneStart("PvdStudio.AnalysisBatch", false, context);
            auto* block = require(allocator.allocate(16, "AnalysisChurn", "analysis-load", 2), "analysis churn allocation");
            ++allocations;
            allocator.deallocate(block);
            ++frees;
            pvd.zoneEnd(token, "PvdStudio.AnalysisBatch", false, context);
            ++profile_pairs;
            pvd.flush();
        }
        record("step", frame);
        if (!pvd.isConnected(false)) {
            throw std::runtime_error("PVD connection lost during analysis load at frame " + std::to_string(frame));
        }
    }

    void release() {
        for (auto* block : resident) {
            allocator.deallocate(block);
            ++frees;
        }
        resident.clear();
        if (enabled) pvd.flush();
    }

    void finish(unsigned frame) {
        if (!enabled) return;
        release();
        record("released", frame);
        if (truth.is_open()) {
            truth.flush();
            if (!truth) throw std::runtime_error("Writing analysis truth CSV failed.");
        }
        enabled = false;
    }

    void record(const char* phase, unsigned frame) {
        if (!truth.is_open()) return;
        truth << phase << ',' << frame << ',' << resident.size() << ',' << resident.size() * 16
            << ',' << allocations << ',' << frees << ',' << profile_pairs << '\n';
        if (frame % 60 == 0) truth.flush();
        if (!truth) throw std::runtime_error("Writing analysis truth CSV failed.");
    }
};

struct Simulation {
    PxDefaultAllocator allocator;
    PxDefaultErrorCallback errors;
    PxFoundation* foundation = nullptr;
    PxPvdTransport* transport = nullptr;
    PxPvd* pvd = nullptr;
    PxPhysics* physics = nullptr;
    PxDefaultCpuDispatcher* dispatcher = nullptr;
    PxScene* scene = nullptr;
    PxMaterial* material = nullptr;
    PxCooking* cooking = nullptr;
    PxJoint* joint = nullptr;
    bool extensions = false;

    ~Simulation() {
        if (joint) joint->release();
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

    void initialize(const Options& options) {
        const auto flags = instrumentation(options.flags);
        foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, allocator, errors), "foundation");
        pvd = require(PxCreatePvd(*foundation), "PVD client");
        transport = require(options.file.empty()
            ? PxDefaultPvdSocketTransportCreate(options.host.c_str(), options.port, 1000)
            : PxDefaultPvdFileTransportCreate(options.file.c_str()), "PVD transport");
        if (!pvd->connect(*transport, flags)) {
            throw std::runtime_error("PVD connection failed; check the path or listener address and port.");
        }
        physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *foundation, PxTolerancesScale(), true, pvd), "physics");
        if (options.rich || !options.export_repx.empty()) {
            extensions = PxInitExtensions(*physics, pvd);
            if (!extensions) throw std::runtime_error("Cannot initialize PhysX extensions.");
            cooking = require(PxCreateCooking(PX_PHYSICS_VERSION, *foundation, PxCookingParams(physics->getTolerancesScale())), "cooking");
        }
        dispatcher = require(PxDefaultCpuDispatcherCreate(1), "CPU dispatcher");
        PxSceneDesc settings(physics->getTolerancesScale());
        settings.gravity = PxVec3(0, -9.81f, 0);
        settings.cpuDispatcher = dispatcher;
        settings.filterShader = PxDefaultSimulationFilterShader;
        scene = require(physics->createScene(settings), "scene");
        if (auto* client = scene->getScenePvdClient()) {
            client->setScenePvdFlag(PxPvdSceneFlag::eTRANSMIT_CONSTRAINTS, true);
            client->setScenePvdFlag(PxPvdSceneFlag::eTRANSMIT_CONTACTS, true);
            client->setScenePvdFlag(PxPvdSceneFlag::eTRANSMIT_SCENEQUERIES, true);
        }
        material = require(physics->createMaterial(0.5f, 0.5f, 0.2f), "material");
        auto* ground = require(PxCreatePlane(*physics, PxPlane(0, 1, 0, 0), *material), "plane");
        ground->setName("PvdStudioGround");
        scene->addActor(*ground);
    }

    PxRigidDynamic* body(const char* name, const PxVec3& position, const PxGeometry& geometry) {
        auto* actor = require(PxCreateDynamic(*physics, PxTransform(position), geometry, *material, 10.0f), "actor");
        actor->setName(name);
        scene->addActor(*actor);
        return actor;
    }

    std::vector<PxRigidDynamic*> rich_bodies(std::ofstream& mesh_truth) {
        auto* capsule = body("PvdStudioCapsule", PxVec3(5, 4, 0), PxCapsuleGeometry(0.4f, 0.8f));
        joint = require(PxSphericalJointCreate(*physics, nullptr, PxTransform(PxVec3(5, 6, 0)),
            capsule, PxTransform(PxVec3(0, 1.2f, 0))), "spherical joint");
        joint->setName("PvdStudioSphericalJoint");
        joint->setConstraintFlag(PxConstraintFlag::eVISUALIZATION, true);
        const PxVec3 points[] = {
            PxVec3(-0.7f, -0.5f, -0.6f), PxVec3(0.8f, -0.5f, -0.6f),
            PxVec3(0.8f, -0.5f, 0.6f), PxVec3(-0.7f, -0.5f, 0.6f), PxVec3(0, 1.1f, 0)
        };
        PxConvexMeshDesc mesh_description;
        mesh_description.points.count = 5;
        mesh_description.points.stride = sizeof(PxVec3);
        mesh_description.points.data = points;
        mesh_description.flags = PxConvexFlag::eCOMPUTE_CONVEX;
        PxDefaultMemoryOutputStream bytes;
        if (!cooking->cookConvexMesh(mesh_description, bytes)) {
            throw std::runtime_error("Cannot cook convex validation mesh.");
        }
        PxDefaultMemoryInputData input(bytes.getData(), bytes.getSize());
        auto* mesh = require(physics->createConvexMesh(input), "convex mesh");
        const PxMeshScale convex_scale(PxVec3(1.25f, 0.75f, 1.5f), PxQuat(0.35f, PxVec3(0, 1, 0)));
        auto* convex = body("PvdStudioConvex", PxVec3(-5, 5, 0), PxConvexMeshGeometry(mesh, convex_scale));
        if (mesh_truth.is_open()) {
            mesh_truth << "{\"meshes\":[{\"kind\":\"convex\",\"actor_id\":" << reinterpret_cast<std::uintptr_t>(convex);
            write_mesh_vertices(mesh_truth, mesh->getVertices(), mesh->getNbVertices(), convex_scale);
            mesh_truth << ",\"polygons\":[";
            for (PxU32 polygon_index = 0; polygon_index < mesh->getNbPolygons(); ++polygon_index) {
                PxHullPolygon polygon;
                mesh->getPolygonData(polygon_index, polygon);
                if (polygon_index) mesh_truth << ',';
                mesh_truth << '[';
                for (PxU32 vertex = 0; vertex < polygon.mNbVerts; ++vertex) {
                    if (vertex) mesh_truth << ',';
                    mesh_truth << static_cast<unsigned>(mesh->getIndexBuffer()[polygon.mIndexBase + vertex]);
                }
                mesh_truth << ']';
            }
            mesh_truth << "]},";
        }
        mesh->release();

        const PxVec3 ramp_vertices[] = {PxVec3(0, 0, 0), PxVec3(4, 0, 0), PxVec3(0, 1.5f, 4), PxVec3(4, 1.5f, 4)};
        const PxU32 ramp_triangles[] = {0, 2, 1, 1, 2, 3};
        PxTriangleMeshDesc ramp_description;
        ramp_description.points.count = 4;
        ramp_description.points.stride = sizeof(PxVec3);
        ramp_description.points.data = ramp_vertices;
        ramp_description.triangles.count = 2;
        ramp_description.triangles.stride = sizeof(PxU32) * 3;
        ramp_description.triangles.data = ramp_triangles;
        PxDefaultMemoryOutputStream ramp_bytes;
        if (!cooking->cookTriangleMesh(ramp_description, ramp_bytes)) {
            throw std::runtime_error("Cannot cook triangle validation mesh.");
        }
        PxDefaultMemoryInputData ramp_input(ramp_bytes.getData(), ramp_bytes.getSize());
        auto* ramp_mesh = require(physics->createTriangleMesh(ramp_input), "triangle mesh");
        const PxMeshScale ramp_scale(PxVec3(1.2f, 1.0f, 0.8f), PxQuat(PxIdentity));
        auto* ramp = require(PxCreateStatic(*physics, PxTransform(PxVec3(8, 0, -2)),
            PxTriangleMeshGeometry(ramp_mesh, ramp_scale), *material), "ramp actor");
        ramp->setName("PvdStudioTriangleRamp");
        scene->addActor(*ramp);
        if (mesh_truth.is_open()) {
            mesh_truth << "{\"kind\":\"triangle\",\"actor_id\":" << reinterpret_cast<std::uintptr_t>(ramp);
            write_mesh_vertices(mesh_truth, ramp_mesh->getVertices(), ramp_mesh->getNbVertices(), ramp_scale);
            mesh_truth << ",\"triangles\":[";
            for (PxU32 triangle = 0; triangle < ramp_mesh->getNbTriangles(); ++triangle) {
                if (triangle) mesh_truth << ',';
                mesh_truth << '[';
                for (PxU32 corner = 0; corner < 3; ++corner) {
                    if (corner) mesh_truth << ',';
                    const auto index = triangle * 3 + corner;
                    mesh_truth << (ramp_mesh->getTriangleMeshFlags() & PxTriangleMeshFlag::e16_BIT_INDICES
                        ? static_cast<const PxU16*>(ramp_mesh->getTriangles())[index]
                        : static_cast<const PxU32*>(ramp_mesh->getTriangles())[index]);
                }
                mesh_truth << ']';
            }
            mesh_truth << "]}]}\n";
        }
        ramp_mesh->release();
        return {capsule, convex};
    }

    static void write_mesh_vertices(std::ofstream& output, const PxVec3* vertices, PxU32 count, const PxMeshScale& scale) {
        output << ",\"scale\":[" << scale.scale.x << ',' << scale.scale.y << ',' << scale.scale.z
            << "],\"rotation\":[" << scale.rotation.x << ',' << scale.rotation.y << ',' << scale.rotation.z << ',' << scale.rotation.w
            << "],\"vertices\":[";
        for (PxU32 index = 0; index < count; ++index) {
            if (index) output << ',';
            output << '[' << vertices[index].x << ',' << vertices[index].y << ',' << vertices[index].z << ']';
        }
        output << ']';
    }
};

void write_pose(std::ofstream& output, unsigned frame, const PxRigidDynamic& actor) {
    const auto pose = actor.getGlobalPose();
    output << frame << ',' << reinterpret_cast<std::uintptr_t>(&actor) << ',' << actor.getName()
        << ',' << pose.p.x << ',' << pose.p.y << ',' << pose.p.z
        << ',' << pose.q.x << ',' << pose.q.y << ',' << pose.q.z << ',' << pose.q.w << '\n';
}

void sample_raycast(PxScene& scene, std::ofstream& output, bool& first, unsigned frame,
    unsigned index, const PxVec3& origin, const PxVec3& direction, float distance) {
    PxRaycastBuffer query;
    scene.raycast(origin, direction, distance, query);
    if (!output.is_open()) return;
    if (!first) output << ',';
    first = false;
    output << "{\"frame\":" << frame << ",\"index\":" << index
        << ",\"origin\":[" << origin.x << ',' << origin.y << ',' << origin.z
        << "],\"direction\":[" << direction.x << ',' << direction.y << ',' << direction.z
        << "],\"distance\":" << distance << ",\"hits\":[";
    if (query.hasBlock) {
        const auto& hit = query.block;
        output << "{\"actor\":" << reinterpret_cast<std::uintptr_t>(hit.actor)
            << ",\"shape\":" << reinterpret_cast<std::uintptr_t>(hit.shape)
            << ",\"flags\":" << static_cast<PxU16>(hit.flags)
            << ",\"position\":[" << hit.position.x << ',' << hit.position.y << ',' << hit.position.z
            << "],\"normal\":[" << hit.normal.x << ',' << hit.normal.y << ',' << hit.normal.z
            << "],\"distance\":" << hit.distance << ",\"face_index\":" << hit.faceIndex;
        if (hit.flags & PxHitFlag::eUV) output << ",\"uv\":[" << hit.u << ',' << hit.v << ']';
        output << '}';
    }
    output << "]}\n";
}

int main(int argc, char** argv) {
    try {
        const auto options = parse(argc, argv);
        if (!options.import_repx.empty()) {
            import_repx(options.import_repx, options.roundtrip_repx);
            return 0;
        }
        std::ofstream truth;
        std::ofstream mesh_truth;
        std::ofstream query_truth;
        std::ofstream stats_truth;
        bool first_query = true;
        if (!options.truth.empty()) {
            truth.open(options.truth);
            if (!truth) throw std::runtime_error("Cannot create truth CSV: " + options.truth);
            truth << "frame,actor_id,name,x,y,z,qx,qy,qz,qw\n" << std::setprecision(9);
        }
        if (!options.mesh_truth.empty()) {
            if (!options.rich) throw std::runtime_error("--mesh-truth requires --rich.");
            mesh_truth.open(options.mesh_truth);
            if (!mesh_truth) throw std::runtime_error("Cannot create mesh truth JSON: " + options.mesh_truth);
            mesh_truth << std::setprecision(9);
        }
        if (!options.query_truth.empty()) {
            query_truth.open(options.query_truth);
            if (!query_truth) throw std::runtime_error("Cannot create query truth JSON: " + options.query_truth);
            query_truth << "{\"sdk\":\"" << PX_PHYSICS_VERSION_MAJOR << '.' << PX_PHYSICS_VERSION_MINOR
                << '.' << PX_PHYSICS_VERSION_BUGFIX << "\",\"queries\":[" << std::setprecision(9);
        }
        if (!options.stats_truth.empty()) {
            stats_truth.open(options.stats_truth);
            if (!stats_truth) throw std::runtime_error("Cannot create statistics CSV: " + options.stats_truth);
            stats_truth << "frame,elapsed_ms,active_dynamic,active_kinematic,total_dynamic,total_static,continuous_boxes,awake_continuous_boxes,moving_continuous_boxes\n"
                << std::setprecision(9);
        }
        std::vector<std::string> box_names;
        box_names.reserve(options.extra_boxes);
        for (unsigned index = 0; index < options.extra_boxes; ++index) {
            box_names.push_back("PvdStudioGridBox" + std::to_string(index));
        }
        std::vector<std::string> static_names;
        static_names.reserve(options.extra_static_boxes);
        for (unsigned index = 0; index < options.extra_static_boxes; ++index) {
            static_names.push_back("PvdStudioStaticBox" + std::to_string(index));
        }
        Simulation simulation;
        simulation.initialize(options);
        auto* box = simulation.body("PvdStudioBox", PxVec3(-2, 5, 0), PxBoxGeometry(0.5f, 0.75f, 1.0f));
        auto* sphere = simulation.body("PvdStudioSphere", PxVec3(2, 3, 0), PxSphereGeometry(0.6f));
        auto extra_bodies = options.rich ? simulation.rich_bodies(mesh_truth) : std::vector<PxRigidDynamic*>();
        std::vector<PxRigidDynamic*> continuous_bodies;
        if (options.continuous_motion) continuous_bodies.reserve(options.extra_boxes);
        for (unsigned index = 0; index < options.extra_boxes; ++index) {
            const PxVec3 position(10.0f + static_cast<float>(index % 25) * 2.0f,
                2.0f + static_cast<float>((index / 25) % 8) * 0.2f,
                static_cast<float>(index / 25) * 2.0f);
            auto* actor = simulation.body(box_names[index].c_str(), position,
                PxBoxGeometry(0.35f + static_cast<float>(index % 5) * 0.01f, 0.4f, 0.45f));
            extra_bodies.push_back(actor);
            if (options.continuous_motion) {
                actor->setActorFlag(PxActorFlag::eDISABLE_GRAVITY, true);
                actor->setSleepThreshold(0.0f);
                actor->setAngularDamping(0.0f);
                continuous_bodies.push_back(actor);
            }
        }
        std::ofstream static_truth;
        if (options.extra_static_boxes && !options.stats_truth.empty()) {
            static_truth.open(options.stats_truth + ".statics.csv");
            if (!static_truth) throw std::runtime_error("Cannot open static actor truth CSV.");
            static_truth << "actor_id,name,shape_id,px,py,pz,qx,qy,qz,qw,hx,hy,hz\n" << std::setprecision(9);
        }
        for (unsigned index = 0; index < options.extra_static_boxes; ++index) {
            const PxVec3 position(-1000.0f - static_cast<float>(index % 128) * 2.0f, 1.0f,
                static_cast<float>(index / 128) * 2.0f);
            auto* actor = require(PxCreateStatic(*simulation.physics, PxTransform(position),
                PxBoxGeometry(0.4f, 0.5f, 0.6f), *simulation.material), "static box");
            actor->setName(static_names[index].c_str());
            simulation.scene->addActor(*actor);
            if (static_truth.is_open()) {
                PxShape* shape = nullptr;
                if (actor->getShapes(&shape, 1) != 1) throw std::runtime_error("Static box shape missing.");
                PxBoxGeometry geometry;
                if (!shape->getBoxGeometry(geometry)) throw std::runtime_error("Static box geometry missing.");
                const auto pose = actor->getGlobalPose();
                static_truth << reinterpret_cast<std::uintptr_t>(actor) << ',' << actor->getName()
                    << ',' << reinterpret_cast<std::uintptr_t>(shape)
                    << ',' << pose.p.x << ',' << pose.p.y << ',' << pose.p.z
                    << ',' << pose.q.x << ',' << pose.q.y << ',' << pose.q.z << ',' << pose.q.w
                    << ',' << geometry.halfExtents.x << ',' << geometry.halfExtents.y << ',' << geometry.halfExtents.z << '\n';
            }
        }
        if (static_truth.is_open()) {
            static_truth.close();
            if (!static_truth) throw std::runtime_error("Writing static actor truth CSV failed.");
        }
        AnalysisLoad analysis_load(*simulation.pvd);
        analysis_load.initialize(options);
        const auto static_actor_count = simulation.scene->getNbActors(PxActorTypeFlag::eRIGID_STATIC);
        bool reused_id = false;
        const auto started = std::chrono::steady_clock::now();
        PxU32 minimum_active = std::numeric_limits<PxU32>::max();
        PxU32 maximum_active = 0;
        unsigned completed_steps = 0;
        bool duration_reached = false;
        for (unsigned frame = 0; frame < options.frames; ++frame) {
            analysis_load.step(frame);
            if (options.continuous_motion) {
                // Periodic velocities keep the SDK-simulated load near its initial grid for long runs.
                const auto phase = static_cast<double>(frame) / 60.0;
                const PxVec3 velocity(static_cast<float>(0.5 * std::cos(phase)), 0.0f,
                    static_cast<float>(0.5 * std::sin(phase)));
                for (auto* actor : continuous_bodies) {
                    actor->setLinearVelocity(velocity);
                    actor->setAngularVelocity(PxVec3(0.0f, 0.8f, 0.0f));
                }
            }
            if (options.emit_error && frame == options.frames / 4) {
                const auto mass = box->getMass();
                box->setMass(-1.0f);
                if (box->getMass() != mass) throw std::runtime_error("SDK accepted invalid mass unexpectedly.");
            }
            if (frame == options.frames / 2) {
                box->setName("PvdStudioBoxRenamed");
            }
            if (options.lifecycle && frame == options.frames * 3 / 4) {
                const auto old_id = reinterpret_cast<std::uintptr_t>(sphere);
                sphere->release();
                sphere = simulation.body("PvdStudioSphereReplacement", PxVec3(2, 4, 1), PxSphereGeometry(0.4f));
                reused_id = old_id == reinterpret_cast<std::uintptr_t>(sphere);
            }
            sample_raycast(*simulation.scene, query_truth, first_query, frame, 0,
                PxVec3(0, 10, 0), PxVec3(0, -1, 0), 20.0f);
            if (query_truth.is_open()) {
                sample_raycast(*simulation.scene, query_truth, first_query, frame, 1,
                    box->getGlobalPose().p + PxVec3(0, 4, 0), PxVec3(0, -1, 0), 10.0f);
                sample_raycast(*simulation.scene, query_truth, first_query, frame, 2,
                    sphere->getGlobalPose().p + PxVec3(0, 4, 0), PxVec3(0, -1, 0), 10.0f);
                sample_raycast(*simulation.scene, query_truth, first_query, frame, 3,
                    PxVec3(0, 10, 0), PxVec3(0, 1, 0), 20.0f);
            }
            if (truth.is_open()) {
                write_pose(truth, frame, *box);
                write_pose(truth, frame, *sphere);
                for (auto* actor : extra_bodies) write_pose(truth, frame, *actor);
            }
            if (!options.export_repx.empty() && frame == options.export_frame) {
                export_repx(*simulation.physics, *simulation.scene, options.export_repx);
            }
            simulation.scene->simulate(1.0f / 60.0f);
            simulation.scene->fetchResults(true);
            const auto step_finished = std::chrono::steady_clock::now();
            if (options.continuous_motion || stats_truth.is_open()) {
                PxSimulationStatistics statistics;
                simulation.scene->getSimulationStatistics(statistics);
                if (statistics.nbStaticBodies != static_actor_count) {
                    throw std::runtime_error("SDK static body count differs from the configured fixture.");
                }
                minimum_active = std::min(minimum_active, statistics.nbActiveDynamicBodies);
                maximum_active = std::max(maximum_active, statistics.nbActiveDynamicBodies);
                unsigned awake = 0;
                unsigned moving = 0;
                for (auto* actor : continuous_bodies) {
                    if (!actor->isSleeping()) ++awake;
                    if (actor->getLinearVelocity().magnitudeSquared() > 1e-8f) ++moving;
                }
                if (stats_truth.is_open()) {
                    const auto elapsed_ms = std::chrono::duration<double, std::milli>(
                        step_finished - started).count();
                    stats_truth << frame << ',' << elapsed_ms << ',' << statistics.nbActiveDynamicBodies
                        << ',' << statistics.nbActiveKinematicBodies << ',' << statistics.nbDynamicBodies
                        << ',' << statistics.nbStaticBodies << ',' << continuous_bodies.size()
                        << ',' << awake << ',' << moving << '\n';
                    if (frame % 60 == 0) stats_truth.flush();
                    if (!stats_truth) throw std::runtime_error("Writing statistics CSV failed.");
                }
                if (awake != continuous_bodies.size() || moving != continuous_bodies.size()
                    || statistics.nbActiveDynamicBodies < continuous_bodies.size()) {
                    throw std::runtime_error("Continuous load lost active or moving SDK bodies at frame " + std::to_string(frame));
                }
                if (options.continuous_motion && !simulation.pvd->isConnected(false)) {
                    throw std::runtime_error("PVD connection lost during continuous load at frame " + std::to_string(frame));
                }
            }
            ++completed_steps;
            if (options.duration_seconds && step_finished - started
                >= std::chrono::seconds(options.duration_seconds)) {
                duration_reached = true;
                break;
            }
            if (options.interval_ms) {
                std::this_thread::sleep_for(std::chrono::milliseconds(options.interval_ms));
            }
        }
        if (truth.is_open()) {
            truth.flush();
            if (!truth) throw std::runtime_error("Writing truth CSV failed.");
        }
        if (mesh_truth.is_open()) {
            mesh_truth.flush();
            if (!mesh_truth) throw std::runtime_error("Writing mesh truth JSON failed.");
        }
        if (query_truth.is_open()) {
            query_truth << "]}\n";
            query_truth.flush();
            if (!query_truth) throw std::runtime_error("Writing query truth JSON failed.");
        }
        if (stats_truth.is_open()) {
            stats_truth.flush();
            if (!stats_truth) throw std::runtime_error("Writing statistics CSV failed.");
        }
        analysis_load.finish(completed_steps);
        std::cout << "sdk=" << PX_PHYSICS_VERSION_MAJOR << '.' << PX_PHYSICS_VERSION_MINOR << '.' << PX_PHYSICS_VERSION_BUGFIX
            << " frames=" << completed_steps << " flags=" << options.flags
            << " extra_boxes=" << options.extra_boxes
            << " lifecycle=" << options.lifecycle << " reused_id=" << reused_id << '\n';
        if (options.resident_allocations || options.analysis_batches_per_frame) {
            std::cout << "resident_allocations=" << options.resident_allocations
                << " allocation_size=16 analysis_batches_per_frame=" << options.analysis_batches_per_frame
                << " allocation_calls=" << analysis_load.allocations << " free_calls=" << analysis_load.frees
                << " profile_pairs=" << analysis_load.profile_pairs << '\n';
        }
        if (options.continuous_motion || stats_truth.is_open()) {
            std::cout << "continuous_motion=" << options.continuous_motion
                << " continuous_boxes=" << continuous_bodies.size()
                << " min_active_dynamic=" << minimum_active << " max_active_dynamic=" << maximum_active
                << " measured_steps=" << completed_steps << '\n';
        }
        if (options.duration_seconds) {
            const auto elapsed_ms = std::chrono::duration<double, std::milli>(
                std::chrono::steady_clock::now() - started).count();
            std::cout << std::setprecision(12) << "duration_seconds=" << options.duration_seconds
                << " elapsed_ms=" << elapsed_ms << " step_limit=" << options.frames << '\n';
            if (!duration_reached) {
                throw std::runtime_error("Frame limit reached before the requested duration completed.");
            }
        }
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n'
            << "Usage: pvd-physx-sample (--file PATH | --host ADDRESS) [--port 5425] [--frames 120] [--duration-seconds 1..3600]"
               " [--flags debug|all|profile|memory] [--truth PATH] [--lifecycle] [--rich] [--extra-boxes 0] [--continuous-motion] [--stats-truth PATH] [--mesh-truth PATH] [--query-truth PATH] [--emit-error] [--interval-ms 0]"
               " [--extra-static-boxes 0..20000] [--resident-allocations 0..200000] [--analysis-batches-per-frame 0..64] [--analysis-truth PATH]"
               " [--export-repx PATH --export-frame 0]\n"
            << "       pvd-physx-sample --import-repx PATH [--roundtrip-repx PATH]\n";
        return 1;
    }
}
