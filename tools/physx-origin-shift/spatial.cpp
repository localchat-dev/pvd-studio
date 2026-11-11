#include "../physx-features/feature_harness.h"
#include "PxPvdDataStream.h"
#include "PxPvdUserRenderer.h"
#include <array>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <vector>

static std::uintptr_t id(const void* value) { return reinterpret_cast<std::uintptr_t>(value); }
static void vector(std::ostream& out, PxVec3 value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}
static void vectors(std::ostream& out, const std::vector<PxVec3>& values) {
    out << '[';
    for (std::size_t i = 0; i < values.size(); ++i) {
        if (i) out << ',';
        vector(out, values[i]);
    }
    out << ']';
}
static PxFilterFlags filter(PxFilterObjectAttributes, PxFilterData, PxFilterObjectAttributes,
    PxFilterData, PxPairFlags& flags, const void*, PxU32) {
    flags = PxPairFlag::eCONTACT_DEFAULT | PxPairFlag::eNOTIFY_TOUCH_FOUND |
        PxPairFlag::eNOTIFY_TOUCH_PERSISTS | PxPairFlag::eNOTIFY_CONTACT_POINTS;
    return PxFilterFlag::eDEFAULT;
}

struct Contact {
    std::array<std::uintptr_t, 2> shapes;
    PxContactPairPoint point;
};
struct Contacts : PxSimulationEventCallback {
    std::vector<Contact> values;
    void onConstraintBreak(PxConstraintInfo*, PxU32) override {}
    void onWake(PxActor**, PxU32) override {}
    void onSleep(PxActor**, PxU32) override {}
    void onTrigger(PxTriggerPair*, PxU32) override {}
    void onAdvance(const PxRigidBody* const*, const PxTransform*, const PxU32) override {}
    void onContact(const PxContactPairHeader&, const PxContactPair* pairs, PxU32 count) override {
        for (PxU32 pair = 0; pair < count; ++pair) {
            std::vector<PxContactPairPoint> points(pairs[pair].contactCount);
            points.resize(pairs[pair].extractContacts(points.data(), static_cast<PxU32>(points.size())));
            for (const auto& point : points)
                values.push_back({{{id(pairs[pair].shapes[0]), id(pairs[pair].shapes[1])}}, point});
        }
    }
};

struct Renderer : pvdsdk::RendererEventClient {
    pvdsdk::PvdDataStream* stream;
    pvdsdk::PvdUserRenderer* renderer;
    Renderer(PxPvd* pvd, const void* instance) :
        stream(feature_require(pvdsdk::PvdDataStream::create(pvd), "render stream")),
        renderer(feature_require(pvdsdk::PvdUserRenderer::create(), "renderer")) {
        stream->createInstance(renderer);
        renderer->setClient(this);
        renderer->setInstanceId(instance);
    }
    ~Renderer() {
        renderer->flushRenderEvents();
        stream->destroyInstance(renderer);
        renderer->release();
        stream->release();
    }
    void handleBufferFlush(const uint8_t* data, uint32_t size) override {
        stream->setPropertyValue(renderer, "events", data, size);
    }
};

struct Query {
    PxVec3 position, direction, sweep_position, overlap_position;
    float distance;
    std::vector<PxRaycastHit> ray;
    std::vector<PxSweepHit> sweep;
    std::vector<PxOverlapHit> overlap;
};
template <typename Hit> static void batch_hits(std::vector<Hit>& target, const PxBatchQueryResult<Hit>& result) {
    if (result.queryStatus != PxBatchQueryStatus::eSUCCESS)
        throw std::runtime_error("Batch query did not complete");
    target.clear();
    for (PxU32 i = 0; i < result.getNbAnyHits(); ++i) target.push_back(result.getAnyHit(i));
    if (target.empty()) throw std::runtime_error("Expected a batch query hit");
}
template <typename Hit> static void location_hit(std::ostream& out, const Hit& hit) {
    out << "{\"actor\":" << id(hit.actor) << ",\"shape\":" << id(hit.shape)
        << ",\"position\":"; vector(out, hit.position);
    out << ",\"normal\":"; vector(out, hit.normal);
    out << ",\"distance\":" << hit.distance << '}';
}
template <typename Hit> static void location_hits(std::ostream& out, const std::vector<Hit>& hits) {
    out << '[';
    for (std::size_t i = 0; i < hits.size(); ++i) {
        if (i) out << ',';
        location_hit(out, hits[i]);
    }
    out << ']';
}

struct World {
    PxScene* scene;
    Contacts contacts;
    Renderer renderer;
    Renderer scene_renderer;
    PxRigidStatic* ground;
    PxRigidDynamic* body;
    Query query;
    std::array<Query, 2> batch_queries;
    std::array<PxVec3, 7> packet;
    std::array<PxVec3, 7> scene_packet;

    World(FeatureContext& context, PxScene* target, float offset) :
        scene(target), renderer(context.pvd, nullptr), scene_renderer(context.pvd, target) {
        scene->setSimulationEventCallback(&contacts);
        scene->getScenePvdClient()->setScenePvdFlags(PxPvdSceneFlag::eTRANSMIT_CONTACTS |
            PxPvdSceneFlag::eTRANSMIT_SCENEQUERIES | PxPvdSceneFlag::eTRANSMIT_CONSTRAINTS);
        scene->setVisualizationParameter(PxVisualizationParameter::eSCALE, 1);
        scene->setVisualizationParameter(PxVisualizationParameter::eACTOR_AXES, 1);
        scene->setVisualizationParameter(PxVisualizationParameter::eCOLLISION_SHAPES, 1);
        ground = feature_require(PxCreateStatic(*context.physics, PxTransform(PxVec3(offset, -1, 0)),
            PxBoxGeometry(5, 1, 5), *context.material), "ground");
        body = feature_require(PxCreateDynamic(*context.physics, PxTransform(PxVec3(offset, 0.49f, 0)),
            PxBoxGeometry(0.5f, 0.5f, 0.5f), *context.material, 1), "body");
        body->setSleepThreshold(0);
        scene->addActor(*ground); scene->addActor(*body);
    }
    ~World() { body->release(); ground->release(); }

    void queries() {
        const PxVec3 target = body->getGlobalPose().p;
        query.position = target + PxVec3(0, 4, 0);
        query.direction = PxVec3(0, -1, 0); query.distance = 10;
        PxRaycastBuffer ray;
        scene->raycast(query.position, query.direction, query.distance, ray);
        query.ray.clear(); if (ray.hasBlock) query.ray.push_back(ray.block);
        PxSweepBuffer sweep;
        query.sweep_position = target + PxVec3(-3, 0, 0);
        scene->sweep(PxSphereGeometry(0.2f), PxTransform(query.sweep_position), PxVec3(1, 0, 0), 6, sweep);
        query.sweep.clear(); if (sweep.hasBlock) query.sweep.push_back(sweep.block);
        PxOverlapHit hits[8]; PxOverlapBuffer overlap(hits, 8);
        query.overlap_position = target;
        scene->overlap(PxSphereGeometry(0.2f), PxTransform(query.overlap_position), overlap,
            PxQueryFilterData(PxQueryFlag::eSTATIC | PxQueryFlag::eDYNAMIC | PxQueryFlag::eNO_BLOCK));
        query.overlap.assign(overlap.getTouches(), overlap.getTouches() + overlap.getNbTouches());
        batched_queries();
    }
    void batched_queries() {
        PxRaycastQueryResult rays[2];
        PxSweepQueryResult sweeps[2];
        PxOverlapQueryResult overlaps[2];
        PxOverlapHit overlap_hits[16];
        PxBatchQueryDesc description(2, 2, 2);
        description.queryMemory.userRaycastResultBuffer = rays;
        description.queryMemory.userSweepResultBuffer = sweeps;
        description.queryMemory.userOverlapResultBuffer = overlaps;
        description.queryMemory.userOverlapTouchBuffer = overlap_hits;
        description.queryMemory.overlapTouchBufferSize = 16;
        auto* batch = feature_require(scene->createBatchQuery(description), "batch query");
        for (unsigned i = 0; i < batch_queries.size(); ++i) {
            auto& input = batch_queries[i];
            const PxVec3 target = body->getGlobalPose().p + PxVec3(0, 0, 0.1f * i);
            input.position = target + PxVec3(0, 4, 0);
            input.direction = PxVec3(0, -1, 0); input.distance = 10;
            input.sweep_position = target + PxVec3(-3, 0, 0);
            input.overlap_position = target;
            batch->raycast(input.position, input.direction, input.distance, 0);
            batch->sweep(PxSphereGeometry(0.2f), PxTransform(input.sweep_position), PxVec3(1, 0, 0), 6, 0);
            batch->overlap(PxSphereGeometry(0.2f), PxTransform(input.overlap_position), 8,
                PxQueryFilterData(PxQueryFlag::eSTATIC | PxQueryFlag::eDYNAMIC | PxQueryFlag::eNO_BLOCK));
        }
        batch->execute();
        for (unsigned i = 0; i < batch_queries.size(); ++i) {
            batch_hits(batch_queries[i].ray, rays[i]);
            batch_hits(batch_queries[i].sweep, sweeps[i]);
            batch_hits(batch_queries[i].overlap, overlaps[i]);
        }
        batch->release();
    }
    void simulate() {
        contacts.values.clear();
        scene->simulate(1.0f / 60); scene->fetchResults(true);
        if (contacts.values.empty()) throw std::runtime_error("Expected contact callback points");
    }
    static std::array<PxVec3, 7> packet_positions(const PxVec3& origin) {
        return {{origin + PxVec3(1, 2, 3), origin + PxVec3(2, 3, 4), origin + PxVec3(3, 4, 5),
            origin + PxVec3(4, 5, 6), origin + PxVec3(5, 5, 6), origin + PxVec3(4, 6, 6),
            origin + PxVec3(7, 8, 9)}};
    }
    static void draw(Renderer& renderer, const std::array<PxVec3, 7>& packet) {
        const pvdsdk::PvdDebugPoint point(packet[0], 0xff00ff00);
        const pvdsdk::PvdDebugLine line(packet[1], packet[2], 0xff0000ff);
        const pvdsdk::PvdDebugTriangle triangle(packet[3], packet[4], packet[5], 0xffff0000);
        const pvdsdk::PvdDebugText text(packet[6], 0.75f, 0xffffffff, "origin marker");
        renderer.renderer->drawPoints(&point, 1); renderer.renderer->drawLines(&line, 1);
        renderer.renderer->drawTriangles(&triangle, 1); renderer.renderer->drawText(text);
        renderer.renderer->flushRenderEvents();
    }
    void draw(bool refresh) {
        if (refresh) {
            const PxVec3 origin = ground->getGlobalPose().p;
            packet = packet_positions(origin);
            scene_packet = packet_positions(origin + PxVec3(10, 20, 30));
        }
        draw(renderer, packet);
        draw(scene_renderer, scene_packet);
    }
    std::vector<PxVec3> render_positions() const {
        const auto& buffer = scene->getRenderBuffer();
        std::vector<PxVec3> values;
        for (PxU32 i = 0; i < buffer.getNbPoints(); ++i) values.push_back(buffer.getPoints()[i].pos);
        for (PxU32 i = 0; i < buffer.getNbLines(); ++i) {
            values.push_back(buffer.getLines()[i].pos0); values.push_back(buffer.getLines()[i].pos1);
        }
        for (PxU32 i = 0; i < buffer.getNbTriangles(); ++i) {
            values.push_back(buffer.getTriangles()[i].pos0); values.push_back(buffer.getTriangles()[i].pos1);
            values.push_back(buffer.getTriangles()[i].pos2);
        }
        if (values.empty()) throw std::runtime_error("Expected SDK render-buffer geometry");
        return values;
    }
    void write(std::ostream& out, const std::vector<PxVec3>& before) const {
        out << "{\"scene\":" << id(scene) << ",\"actor\":" << id(body) << ",\"position\":";
        vector(out, body->getGlobalPose().p);
        out << ",\"ray_origin\":"; vector(out, query.position);
        out << ",\"ray_hits\":"; location_hits(out, query.ray);
        out << ",\"sweep_origin\":"; vector(out, query.sweep_position);
        out << ",\"sweep_hits\":"; location_hits(out, query.sweep);
        out << ",\"overlap_origin\":"; vector(out, query.overlap_position);
        out << ",\"overlap_hits\":[";
        for (std::size_t i = 0; i < query.overlap.size(); ++i) {
            if (i) out << ',';
            out << "{\"actor\":" << id(query.overlap[i].actor) << ",\"shape\":" << id(query.overlap[i].shape) << '}';
        }
        out << "],\"batch_queries\":[";
        for (std::size_t i = 0; i < batch_queries.size(); ++i) {
            if (i) out << ',';
            const auto& input = batch_queries[i];
            out << "{\"index\":" << i << ",\"ray_origin\":"; vector(out, input.position);
            out << ",\"ray_hits\":"; location_hits(out, input.ray);
            out << ",\"sweep_origin\":"; vector(out, input.sweep_position);
            out << ",\"sweep_hits\":"; location_hits(out, input.sweep);
            out << ",\"overlap_origin\":"; vector(out, input.overlap_position);
            out << ",\"overlap_hits\":[";
            for (std::size_t hit = 0; hit < input.overlap.size(); ++hit) {
                if (hit) out << ',';
                out << "{\"actor\":" << id(input.overlap[hit].actor)
                    << ",\"shape\":" << id(input.overlap[hit].shape) << '}';
            }
            out << "]}";
        }
        out << "],\"contacts\":[";
        for (std::size_t i = 0; i < contacts.values.size(); ++i) {
            if (i) out << ',';
            const auto& contact = contacts.values[i];
            out << "{\"shapes\":[" << contact.shapes[0] << ',' << contact.shapes[1] << "],\"position\":";
            vector(out, contact.point.position); out << ",\"normal\":"; vector(out, contact.point.normal);
            out << ",\"separation\":" << contact.point.separation << '}';
        }
        out << "],\"renderer\":" << id(renderer.renderer) << ",\"packet_positions\":[";
        for (std::size_t i = 0; i < packet.size(); ++i) { if (i) out << ','; vector(out, packet[i]); }
        out << "],\"scene_renderer\":" << id(scene_renderer.renderer) << ",\"scene_packet_positions\":[";
        for (std::size_t i = 0; i < scene_packet.size(); ++i) { if (i) out << ','; vector(out, scene_packet[i]); }
        out << "],\"render_buffer_before\":"; vectors(out, before);
        out << ",\"render_buffer_after\":"; vectors(out, render_positions());
        out << '}';
    }
};

int main(int argc, char** argv) {
    if (argc != 3 || std::string(argv[1]) == argv[2]) {
        std::cerr << "usage: pvd-origin-shift-spatial capture.pvdstream truth.json\n"; return 2;
    }
    try {
        FeatureContext context(argv[1], PxPvdInstrumentationFlag::eDEBUG);
        PxSceneDesc settings(context.physics->getTolerancesScale());
        settings.gravity = PxVec3(0); settings.cpuDispatcher = context.dispatcher; settings.filterShader = filter;
        auto* first = feature_require(context.physics->createScene(settings), "first scene");
        auto* second = feature_require(context.physics->createScene(settings), "second scene");
        {
            World a(context, first, 0), b(context, second, 100);
            a.queries(); a.simulate(); b.queries(); b.simulate();
            auto* marker = feature_require(pvdsdk::PvdDataStream::create(context.pvd), "marker stream");
            std::ofstream truth(argv[2], std::ios::binary | std::ios::trunc);
            if (!truth) throw std::runtime_error("Cannot open truth output");
            truth << std::setprecision(std::numeric_limits<float>::max_digits10)
                << "{\"sdk\":\"3.4.2\",\"warmup_frames\":2,\"frames\":[";
            for (unsigned frame = 0; frame < 7; ++frame) {
                if (frame == 6) { a.queries(); a.simulate(); b.queries(); b.simulate(); }
                const auto before_a = a.render_positions(), before_b = b.render_positions();
                const bool refresh = frame == 0 || frame == 3 || frame == 6;
                a.draw(refresh); b.draw(refresh);
                int shifted = -1; PxVec3 shift(0);
                if (frame == 1) { shifted = 0; shift = PxVec3(1000.25f, -2000.5f, 30.125f); }
                if (frame == 2) { shifted = 1; shift = PxVec3(-100.5f, 50.25f, -0.125f); }
                if (frame == 3) { shifted = 0; shift = PxVec3(-0.25f, 0.5f, 0.125f); }
                if (frame == 4) { shifted = 0; shift = PxVec3(-1000, 2000, -30.25f); }
                if (shifted >= 0) (shifted == 0 ? first : second)->shiftOrigin(shift);
                marker->beginSection(first, "frame"); marker->endSection(first, "frame");
                if (frame) truth << ',';
                truth << "{\"frame\":" << frame + (frame == 6 ? 4 : 2)
                    << ",\"shifted_scene\":" << shifted << ",\"shift\":"; vector(truth, shift);
                truth << ",\"worlds\":["; a.write(truth, before_a); truth << ','; b.write(truth, before_b); truth << "]}";
            }
            truth << "]}\n"; truth.flush();
            marker->release();
            if (!truth) throw std::runtime_error("Writing truth failed");
        }
        first->release(); second->release();
        std::cout << "2 Scenes, 4 origin shifts, ordinary and batched SDK query truth, contacts, render buffers and explicit Scene-bound/unbound renderer packets\n";
        return 0;
    } catch (const std::exception& failure) {
        std::cerr << failure.what() << '\n'; return 1;
    }
}
