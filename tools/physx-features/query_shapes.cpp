#include "feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>

static void vector_json(std::ostream& out, const PxVec3& value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void hit_json(std::ostream& out, const PxOverlapHit& hit) {
    out << "{\"actor\":" << reinterpret_cast<std::uintptr_t>(hit.actor)
        << ",\"shape\":" << reinterpret_cast<std::uintptr_t>(hit.shape)
        << ",\"face_index\":" << hit.faceIndex << ",\"flags\":0,\"distance\":0}";
}

template <typename T> static void location_hit_json(std::ostream& out, const T& hit) {
    out << "{\"actor\":" << reinterpret_cast<std::uintptr_t>(hit.actor)
        << ",\"shape\":" << reinterpret_cast<std::uintptr_t>(hit.shape)
        << ",\"face_index\":" << hit.faceIndex << ",\"flags\":" << static_cast<PxU16>(hit.flags)
        << ",\"distance\":" << hit.distance << ",\"position\":";
    vector_json(out, hit.position);
    out << ",\"normal\":";
    vector_json(out, hit.normal);
    out << '}';
}
static void hit_json(std::ostream& out, const PxSweepHit& hit) { location_hit_json(out, hit); }
static void hit_json(std::ostream& out, const PxRaycastHit& hit) { location_hit_json(out, hit); }

struct QueryInput {
    PxGeometryHolder geometry;
    PxTransform pose;
    PxQueryFilterData filter;
    PxVec3 direction = PxVec3(0, -1, 0);
    float distance = 10;
};

template <typename Result> static void result_json(std::ostream& out, bool& first, unsigned frame,
    const char* type, unsigned index, bool batched, const QueryInput& input, const Result& result) {
    if (!first) out << ',';
    first = false;
    out << "{\"frame\":" << frame << ",\"type\":\"" << type << "\",\"index\":" << index
        << ",\"batched\":" << (batched ? "true" : "false") << ",\"position\":";
    vector_json(out, input.pose.p);
    out << ",\"rotation\":[" << input.pose.q.x << ',' << input.pose.q.y << ',' << input.pose.q.z << ',' << input.pose.q.w
        << "],\"direction\":";
    vector_json(out, input.direction);
    out << ",\"distance\":" << input.distance << ",\"filter_flags\":" << static_cast<PxU16>(input.filter.flags)
        << ",\"filter_data\":[" << input.filter.data.word0 << ',' << input.filter.data.word1 << ','
        << input.filter.data.word2 << ',' << input.filter.data.word3 << "],\"geometry_type\":" << input.geometry.getType();
    switch (input.geometry.getType()) {
        case PxGeometryType::eBOX:
            out << ",\"half_extents\":";
            vector_json(out, input.geometry.box().halfExtents);
            break;
        case PxGeometryType::eSPHERE: out << ",\"radius\":" << input.geometry.sphere().radius; break;
        case PxGeometryType::eCAPSULE:
            out << ",\"radius\":" << input.geometry.capsule().radius << ",\"half_height\":" << input.geometry.capsule().halfHeight;
            break;
        case PxGeometryType::eCONVEXMESH: {
            const auto* mesh = input.geometry.convexMesh().convexMesh;
            out << ",\"vertices\":" << mesh->getNbVertices() << ",\"points\":[";
            for (PxU32 i = 0; i < mesh->getNbVertices(); ++i) {
                if (i) out << ',';
                vector_json(out, mesh->getVertices()[i]);
            }
            out << ']';
            break;
        }
        default: throw std::runtime_error("Unexpected query geometry");
    }
    out << ",\"hits\":[";
    for (PxU32 i = 0; i < result.getNbAnyHits(); ++i) {
        if (i) out << ',';
        hit_json(out, result.getAnyHit(i));
    }
    out << "]}";
}

static PxRigidStatic* target(FeatureContext& context) {
    auto* actor = feature_require(context.physics->createRigidStatic(PxTransform(PxVec3(0, 0, 0))), "query target");
    auto* shape = feature_require(PxRigidActorExt::createExclusiveShape(*actor, PxBoxGeometry(1, 1, 1), *context.material), "query target shape");
    shape->setQueryFilterData(PxFilterData(1, 2, 4, 8));
    actor->setName("Query target");
    context.scene->addActor(*actor);
    return actor;
}

struct QueryOptions {
    FeatureConnection connection;
    const char* truth;
};

static QueryOptions query_options(int argc, char** argv) {
    if (argc == 3) return { FeatureConnection::file(argv[1]), argv[2] };
    if (argc == 6 && std::string(argv[1]) == "--host" && std::string(argv[3]) == "--port") {
        const std::string value(argv[4]);
        if (value.empty() || value.find_first_not_of("0123456789") != std::string::npos) {
            throw std::runtime_error("Port must be an integer between 1 and 65535");
        }
        const auto port = std::stoul(value);
        if (!port || port > 65535) throw std::runtime_error("Port must be between 1 and 65535");
        return { FeatureConnection::socket(argv[2], static_cast<unsigned>(port)), argv[5] };
    }
    throw std::runtime_error("Usage: pvd-query-shapes capture.pvdstream truth.json | --host ADDRESS --port PORT truth.json");
}

int main(int argc, char** argv) {
    try {
        const auto options = query_options(argc, argv);
        FeatureContext context(options.connection);
        std::ofstream truth(options.truth);
        if (!truth) throw std::runtime_error("Cannot create query truth JSON");
        truth << std::setprecision(9) << "{\"sdk\":\"" << feature_sdk_version() << "\",\"frames\":12,\"queries\":[";
        bool first = true;
        auto* actor = target(context);
        auto* second_actor = target(context);
        second_actor->setName("Second overlap target");
        const PxVec3 vertices[] = { PxVec3(-0.4f,-0.3f,-0.2f), PxVec3(0.4f,-0.3f,-0.2f), PxVec3(-0.4f,0.3f,-0.2f), PxVec3(0.4f,0.3f,-0.2f),
            PxVec3(-0.4f,-0.3f,0.2f), PxVec3(0.4f,-0.3f,0.2f), PxVec3(-0.4f,0.3f,0.2f), PxVec3(0.4f,0.3f,0.2f) };
        PxConvexMeshDesc description;
        description.points.count = 8;
        description.points.stride = sizeof(PxVec3);
        description.points.data = vertices;
        description.flags = PxConvexFlag::eCOMPUTE_CONVEX;
        auto* convex = feature_require(context.cooking->createConvexMesh(description, context.physics->getPhysicsInsertionCallback()), "query convex");
        std::vector<PxGeometryHolder> geometries = { PxGeometryHolder(PxBoxGeometry(0.45f, 0.25f, 0.3f)),
            PxGeometryHolder(PxSphereGeometry(0.35f)), PxGeometryHolder(PxCapsuleGeometry(0.2f, 0.5f)), PxGeometryHolder(PxConvexMeshGeometry(convex)) };
        for (unsigned frame = 0; frame < 12; ++frame) {
            if (frame == 6) { actor->release(); actor = target(context); }
            std::vector<QueryInput> sweep_inputs;
            std::vector<QueryInput> overlap_inputs;
            for (unsigned geometry = 0; geometry < geometries.size(); ++geometry) {
                for (unsigned miss = 0; miss < 2; ++miss) {
                    QueryInput input;
                    input.geometry = geometries[geometry];
                    input.pose = PxTransform(PxVec3(miss ? 7.0f : 0.1f, 5.0f, 0.2f), PxQuat(0.3f + frame * 0.02f, PxVec3(0, 0, 1)));
                    input.filter = PxQueryFilterData(PxFilterData(1, 2, 4, 8), frame % 2 ? PxQueryFlag::eSTATIC : PxQueryFlag::eSTATIC | PxQueryFlag::eDYNAMIC);
                    sweep_inputs.push_back(input);
                    PxSweepBuffer sweep;
                    context.scene->sweep(input.geometry.any(), input.pose, input.direction, input.distance, sweep, PxHitFlag::eDEFAULT, input.filter);
                    result_json(truth, first, frame, "sweep", geometry * 2 + miss, false, input, sweep);
                    input.pose.p.y = 0.1f;
                    input.filter.flags |= PxQueryFlag::eNO_BLOCK;
                    overlap_inputs.push_back(input);
                    PxOverlapHit hits[16];
                    PxOverlapBuffer overlap(hits, 16);
                    context.scene->overlap(input.geometry.any(), input.pose, overlap, input.filter);
                    result_json(truth, first, frame, "overlap", geometry * 2 + miss, false, input, overlap);
                }
            }
            QueryInput penetration = sweep_inputs[2];
            penetration.pose.p.y = 0.1f;
            PxSweepBuffer mtd;
            context.scene->sweep(penetration.geometry.any(), penetration.pose, penetration.direction,
                penetration.distance, mtd, PxHitFlag::eDEFAULT | PxHitFlag::eMTD, penetration.filter);
            if (!mtd.hasBlock || mtd.block.distance >= 0) throw std::runtime_error("Expected initial-overlap MTD hit");
            result_json(truth, first, frame, "sweep", 8, false, penetration, mtd);
            PxRaycastQueryResult rays[2];
            PxSweepQueryResult sweeps[8];
            PxOverlapQueryResult overlaps[8];
            PxOverlapHit overlap_hits[128];
            PxBatchQueryDesc batch_description(2, 8, 8);
            batch_description.queryMemory.userRaycastResultBuffer = rays;
            batch_description.queryMemory.userSweepResultBuffer = sweeps;
            batch_description.queryMemory.userOverlapResultBuffer = overlaps;
            batch_description.queryMemory.userOverlapTouchBuffer = overlap_hits;
            batch_description.queryMemory.overlapTouchBufferSize = 128;
            auto* batch = feature_require(context.scene->createBatchQuery(batch_description), "batch query");
            for (unsigned i = 0; i < 2; ++i) {
                const auto& input = sweep_inputs[i];
                batch->raycast(input.pose.p, input.direction, input.distance, 0, PxHitFlag::eDEFAULT, input.filter);
            }
            for (unsigned i = 0; i < 8; ++i) {
                const auto& s = sweep_inputs[i];
                batch->sweep(s.geometry.any(), s.pose, s.direction, s.distance, 0, PxHitFlag::eDEFAULT, s.filter);
                const auto& o = overlap_inputs[i];
                batch->overlap(o.geometry.any(), o.pose, 16, o.filter);
            }
            batch->execute();
            for (unsigned i = 0; i < 2; ++i) {
                if (rays[i].queryStatus != PxBatchQueryStatus::eSUCCESS) throw std::runtime_error("Batch ray failed");
                result_json(truth, first, frame, "ray", i, true, sweep_inputs[i], rays[i]);
            }
            for (unsigned i = 0; i < 8; ++i) {
                if (sweeps[i].queryStatus != PxBatchQueryStatus::eSUCCESS || overlaps[i].queryStatus != PxBatchQueryStatus::eSUCCESS) throw std::runtime_error("Batch shape query failed");
                result_json(truth, first, frame, "sweep", i, true, sweep_inputs[i], sweeps[i]);
                result_json(truth, first, frame, "overlap", i, true, overlap_inputs[i], overlaps[i]);
            }
            batch->release();
            context.step();
        }
        actor->release();
        second_actor->release();
        convex->release();
        truth << "]}";
        truth.close();
        if (!truth) throw std::runtime_error("Query truth write failed");
        if (options.connection.host) context.flush_and_require_connected();
        std::cout << "12 frames, 204 sweeps, 192 overlaps and 24 batch rays\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
