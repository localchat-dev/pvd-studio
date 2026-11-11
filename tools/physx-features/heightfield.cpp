#include "feature_harness.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <stdexcept>
#include <vector>

using namespace physx;

static PxHeightField* terrain(FeatureContext& context, bool ceiling) {
    const PxI16 heights[] = {-5, 3, 8, -2, 1, 4, -6, 9, 2, 7, 0, -3, 6, -1, 5, 11, 2, -4, 10, 3};
    std::vector<PxHeightFieldSample> samples(20);
    for (PxU32 i = 0; i < samples.size(); ++i) {
        samples[i].height = heights[i] + (ceiling ? 2 : 0);
        samples[i].materialIndex0 = 0;
        samples[i].materialIndex1 = 0;
        if (i % 2) samples[i].setTessFlag();
        else samples[i].clearTessFlag();
    }
    samples[0].materialIndex0 = PxHeightFieldMaterial::eHOLE;
    samples[1].materialIndex1 = PxHeightFieldMaterial::eHOLE;
    samples[6].materialIndex0 = PxHeightFieldMaterial::eHOLE;
    samples[6].materialIndex1 = PxHeightFieldMaterial::eHOLE;
    PxHeightFieldDesc description;
    description.nbRows = 4;
    description.nbColumns = 5;
    description.samples.data = samples.data();
    description.samples.stride = sizeof(PxHeightFieldSample);
#if PX_PHYSICS_VERSION_MAJOR == 3
    description.thickness = ceiling ? 1.5f : -2.0f;
#endif
    return feature_require(context.cooking->createHeightField(description, context.physics->getPhysicsInsertionCallback()), "height field");
}

static PxRigidStatic* actor(FeatureContext& context, PxHeightField* field, PxVec3 position, float angle, PxVec3 scale) {
    auto* body = feature_require(context.physics->createRigidStatic(PxTransform(position, PxQuat(angle, PxVec3(0, 1, 0)))), "terrain actor");
    body->setName("HeightField validation");
    PxHeightFieldGeometry geometry(field, PxMeshGeometryFlags(), scale.y, scale.x, scale.z);
    feature_require(PxRigidActorExt::createExclusiveShape(*body, geometry, *context.material), "terrain shape");
    context.scene->addActor(*body);
    return body;
}

static void vector(std::ostream& out, PxVec3 value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void write_actor(std::ostream& out, PxRigidStatic* body) {
    PxShape* shape = nullptr;
    body->getShapes(&shape, 1);
    PxHeightFieldGeometry geometry;
    if (!shape || !shape->getHeightFieldGeometry(geometry)) throw std::runtime_error("height field shape missing");
    const auto* field = geometry.heightField;
    out << "{\"id\":" << reinterpret_cast<std::uintptr_t>(body)
        << ",\"shape\":" << reinterpret_cast<std::uintptr_t>(shape)
        << ",\"resource\":" << reinterpret_cast<std::uintptr_t>(field)
        << ",\"triangles\":[";
    bool first = true;
    for (PxU32 row = 0; row + 1 < field->getNbRows(); ++row) {
        for (PxU32 column = 0; column + 1 < field->getNbColumns(); ++column) {
            for (PxU32 half = 0; half != 2; ++half) {
                const PxU32 index = (row * field->getNbColumns() + column) * 2 + half;
                if (field->getTriangleMaterialIndex(index) == PxHeightFieldMaterial::eHOLE) continue;
                PxTriangle triangle;
                PxU32 indices[3];
                PxMeshQuery::getTriangle(geometry, body->getGlobalPose() * shape->getLocalPose(), index, triangle, indices);
                if (!first) out << ',';
                first = false;
                out << "{\"id\":" << index << ",\"indices\":[" << indices[0] << ',' << indices[1] << ',' << indices[2] << "],\"vertices\":[";
                for (unsigned vertex = 0; vertex != 3; ++vertex) {
                    if (vertex) out << ',';
                    vector(out, triangle.verts[vertex]);
                }
                out << "]}";
            }
        }
    }
    out << "]}";
}

int main(int argc, char** argv) {
    try {
        if (argc != 3) throw std::runtime_error("Usage: pvd-heightfield capture.pvdstream truth.json");
        std::ofstream truth(argv[2], std::ios::trunc);
        if (!truth) throw std::runtime_error("cannot open truth output");
        truth << std::setprecision(9) << "{\"sdk\":\"" << feature_sdk_version() << "\",\"frames\":[";
        FeatureContext context(argv[1]);
        auto* field = terrain(context, false);
        auto* ceiling = terrain(context, true);
        std::vector<PxRigidStatic*> actors = {
            actor(context, field, PxVec3(-7, 1, -4), 0.31f, PxVec3(1.75f, 0.25f, 0.6f)),
            actor(context, field, PxVec3(4, 2, -4), -0.73f, PxVec3(1.75f, 0.25f, 0.6f)),
            actor(context, field, PxVec3(-7, 2, 5), 0.0f, PxVec3(0.9f, 0.4f, 1.3f)),
            actor(context, ceiling, PxVec3(4, 6, 5), 0.21f, PxVec3(1.2f, 0.3f, 0.7f)),
        };
        PxShape* offset_shape = nullptr;
        actors[3]->getShapes(&offset_shape, 1);
        offset_shape->setLocalPose(PxTransform(PxVec3(0.25f, -1.0f, 0.5f), PxQuat(0.37f, PxVec3(1, 0, 0))));
        for (unsigned frame = 0; frame != 75; ++frame) {
            if (frame == 30) actors[0]->setGlobalPose(PxTransform(PxVec3(-9, 3, -6), PxQuat(0.87f, PxVec3(0, 1, 0))));
            if (frame == 45) {
                actors[2]->release();
                actors[2] = actor(context, ceiling, PxVec3(-4, 8, 7), -0.41f, PxVec3(1.5f, 0.6f, 0.8f));
            }
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"actors\":[";
            for (std::size_t index = 0; index < actors.size(); ++index) {
                if (index) truth << ',';
                write_actor(truth, actors[index]);
            }
            truth << "]}";
            context.step();
        }
        truth << "]}\n";
        if (!truth) throw std::runtime_error("truth write failed");
        for (auto* body : actors) body->release();
        field->release();
        ceiling->release();
        std::cout << "75 frames, 4 height field actors, SDK triangle truth written\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
