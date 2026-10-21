#include "../physx-features/feature_harness.h"
#include "PxPvdDataStream.h"
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
static void pose(std::ostream& out, PxTransform value) {
    out << "{\"position\":"; vector(out, value.p);
    out << ",\"rotation\":[" << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << "]}";
}

struct World {
    PxScene* scene;
    std::vector<PxRigidActor*> rigid;
    PxArticulation* articulation;
    PxCloth* cloth;
    std::array<PxParticleBase*, 2> particles;

    World(FeatureContext& context, PxScene* target, float offset) : scene(target) {
        auto* ground = feature_require(PxCreateStatic(*context.physics,
            PxTransform(PxVec3(offset, -2, 0)), PxBoxGeometry(5, 1, 5), *context.material), "static");
        auto* body = feature_require(PxCreateDynamic(*context.physics,
            PxTransform(PxVec3(offset + 1, 4, 2), PxQuat(0.31f, PxVec3(0, 1, 0))),
            PxBoxGeometry(0.5f, 0.6f, 0.7f), *context.material, 2), "dynamic");
        auto* kinematic = feature_require(PxCreateDynamic(*context.physics,
            PxTransform(PxVec3(offset - 2, 6, -3)), PxSphereGeometry(0.5f), *context.material, 2), "kinematic");
        kinematic->setRigidBodyFlag(PxRigidBodyFlag::eKINEMATIC, true);
        rigid = {ground, body, kinematic};
        for (auto* actor : rigid) scene->addActor(*actor);
        articulation = feature_require(context.physics->createArticulation(), "articulation");
        auto* root = feature_require(articulation->createLink(nullptr, PxTransform(PxVec3(offset, 8, 0))), "root link");
        auto* child = feature_require(articulation->createLink(root, PxTransform(PxVec3(offset, 9, 0))), "child link");
        for (auto* link : {root, child}) {
            feature_require(PxRigidActorExt::createExclusiveShape(*link, PxBoxGeometry(0.2f, 0.4f, 0.2f), *context.material), "link shape");
            PxRigidBodyExt::updateMassAndInertia(*link, 1.0f);
            rigid.push_back(link);
        }
        child->getInboundJoint()->setParentPose(PxTransform(PxVec3(0, 0.5f, 0)));
        child->getInboundJoint()->setChildPose(PxTransform(PxVec3(0, -0.5f, 0)));
        scene->addArticulation(*articulation);
        std::array<PxClothParticle, 4> points = {{
            {PxVec3(0, 0, 0), 0}, {PxVec3(1, 0, 0), 0},
            {PxVec3(0, 1, 0), 1}, {PxVec3(1, 1, 0), 1}}};
        std::array<PxU32, 6> triangles = {{0, 1, 2, 1, 3, 2}};
        PxClothMeshDesc mesh;
        mesh.points.count = mesh.invMasses.count = 4;
        mesh.points.stride = mesh.invMasses.stride = sizeof(PxClothParticle);
        mesh.points.data = points.data(); mesh.invMasses.data = &points[0].invWeight;
        mesh.triangles.count = 2; mesh.triangles.stride = 3 * sizeof(PxU32); mesh.triangles.data = triangles.data();
        auto* fabric = feature_require(PxClothFabricCreate(*context.physics, mesh, PxVec3(0, -1, 0)), "fabric");
        cloth = feature_require(context.physics->createCloth(
            PxTransform(PxVec3(offset, 12, 0), PxQuat(0.23f, PxVec3(0, 1, 0))), *fabric, points.data(), PxClothFlags(0)), "cloth");
        fabric->release(); scene->addActor(*cloth);
        particles = {{feature_require(context.physics->createParticleSystem(8), "particle system"),
                      feature_require(context.physics->createParticleFluid(8), "particle fluid")}};
        for (unsigned kind = 0; kind < particles.size(); ++kind) {
            auto* actor = particles[kind];
            actor->setParticleBaseFlag(PxParticleBaseFlag::eGPU, false);
            actor->setParticleReadDataFlag(PxParticleReadDataFlag::ePOSITION_BUFFER, true);
            actor->setParticleReadDataFlag(PxParticleReadDataFlag::eVELOCITY_BUFFER, true);
            actor->setParticleReadDataFlag(PxParticleReadDataFlag::eFLAGS_BUFFER, true);
            scene->addActor(*actor);
            std::array<PxU32, 2> indices = {{1, 5}};
            std::array<PxVec3, 2> positions = {{PxVec3(offset + kind, 15, 2), PxVec3(offset + kind + 0.25f, 15.5f, -2)}};
            std::array<PxVec3, 2> velocities = {{PxVec3(0), PxVec3(0)}};
            PxParticleCreationData data;
            data.numParticles = 2;
            data.indexBuffer = PxStrideIterator<const PxU32>(indices.data());
            data.positionBuffer = PxStrideIterator<const PxVec3>(positions.data());
            data.velocityBuffer = PxStrideIterator<const PxVec3>(velocities.data());
            if (!actor->createParticles(data)) throw std::runtime_error("particle creation failed");
        }
    }

    ~World() {
        for (auto* actor : particles) actor->release();
        cloth->release(); articulation->release();
        for (unsigned i = 0; i < 3; ++i) rigid[i]->release();
    }

    void write(std::ostream& out) const {
        out << "{\"scene\":" << id(scene) << ",\"poses\":[";
        bool comma = false;
        auto emit = [&](std::uintptr_t object, PxTransform value) {
            if (comma) out << ','; comma = true;
            out << "{\"id\":" << object << ",\"pose\":"; pose(out, value); out << '}';
        };
        for (auto* actor : rigid) emit(id(actor), actor->getGlobalPose());
        emit(id(cloth), cloth->getGlobalPose());
        out << "],\"local_poses\":["; comma = false;
        for (auto* actor : rigid) {
            std::vector<PxShape*> shapes(actor->getNbShapes());
            actor->getShapes(shapes.data(), static_cast<PxU32>(shapes.size()));
            for (auto* shape : shapes) emit(id(shape), shape->getLocalPose());
        }
        out << "],\"particles\":[";
        for (unsigned kind = 0; kind < particles.size(); ++kind) {
            if (kind) out << ',';
            auto* actor = particles[kind];
            auto* data = feature_require(actor->lockParticleReadData(), "particle read data");
            out << "{\"id\":" << id(actor) << ",\"values\":["; bool first = true;
            for (PxU32 index = 0; index < data->validParticleRange; ++index) {
                if (!(data->validParticleBitmap[index >> 5] & (1u << (index & 31)))) continue;
                if (!first) out << ','; first = false;
                out << "{\"index\":" << index << ",\"position\":"; vector(out, data->positionBuffer[index]);
                out << ",\"velocity\":"; vector(out, data->velocityBuffer[index]); out << '}';
            }
            out << "]}"; data->unlock();
        }
        out << "]}";
    }
};

int main(int argc, char** argv) {
    if (argc != 3 || std::string(argv[1]) == argv[2]) {
        std::cerr << "usage: pvd-origin-shift capture.pvdstream truth.json\n"; return 2;
    }
    try {
        std::ofstream truth(argv[2], std::ios::binary | std::ios::trunc);
        if (!truth) throw std::runtime_error("Cannot open truth output");
        FeatureContext context(argv[1], PxPvdInstrumentationFlag::eDEBUG);
        PxSceneDesc settings(context.physics->getTolerancesScale());
        settings.gravity = PxVec3(0, -3, 0); settings.cpuDispatcher = context.dispatcher;
        settings.filterShader = PxDefaultSimulationFilterShader;
        auto* other = feature_require(context.physics->createScene(settings), "second scene");
        {
            World a(context, context.scene, 0), b(context, other, 100);
            for (auto* scene : {context.scene, other}) {
                scene->simulate(1.0f / 60); scene->fetchResults(true);
            }
            auto* marker = feature_require(pvdsdk::PvdDataStream::create(context.pvd), "marker stream");
            truth << std::setprecision(std::numeric_limits<float>::max_digits10)
                  << "{\"sdk\":\"3.4.2\",\"warmup_frames\":2,\"frames\":[";
            for (unsigned frame = 0; frame < 144; ++frame) {
                int shifted = -1; PxVec3 shift(0);
                if (frame == 1) { shifted = 0; shift = PxVec3(10000.25f, -2000.125f, 50.75f); }
                if (frame == 2) { shifted = 0; shift = PxVec3(-123.5f, 0.125f, -6); }
                if (frame == 3) { shifted = 1; shift = PxVec3(-200.125f, 300.5f, 12.25f); }
                if (frame == 5) { shifted = 0; shift = PxVec3(-9876.75f, 2000, -44.75f); }
                if (frame == 6) {
                    a.rigid[1]->setGlobalPose(PxTransform(PxVec3(12.125f, 4.25f, -3.5f), PxQuat(0.4f, PxVec3(1, 0, 0))));
                }
                if (frame == 8) { shifted = 1; shift = PxVec3(0.125f, -0.25f, 0.5f); }
                if (shifted >= 0) (shifted == 0 ? context.scene : other)->shiftOrigin(shift);
                marker->beginSection(context.scene, "frame"); marker->endSection(context.scene, "frame");
                if (frame) truth << ',';
                truth << "{\"frame\":" << frame + 2 << ",\"shifted_scene\":" << shifted << ",\"shift\":";
                vector(truth, shift); truth << ",\"worlds\":["; a.write(truth); truth << ','; b.write(truth); truth << "]}";
            }
            truth << "]}\n"; truth.flush();
            marker->release();
        }
        other->release();
        if (!truth) throw std::runtime_error("Writing truth failed");
        std::cout << "2 SDK warmup frames and 144 explicit SDK frame boundaries; 5 origin shifts across two Scenes\n";
        return 0;
    } catch (const std::exception& failure) {
        std::cerr << failure.what() << '\n'; return 1;
    }
}
