#include "../physx-features/feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>

template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"
#include "../repx-meshes/mesh_probe.h"

static PxRigidDynamic* body(FeatureContext& context, const char* name, float x, bool kinematic) {
    auto* actor = require(context.physics->createRigidDynamic(PxTransform(PxVec3(x, 3, 2), PxQuat(0.25f, PxVec3(0, 1, 0)))), "body");
    actor->setName(name);
    actor->setMass(2.5f);
    actor->setMassSpaceInertiaTensor(PxVec3(3, 4, 5));
    actor->setCMassLocalPose(PxTransform(PxVec3(0.2f, -0.3f, 0.4f)));
    actor->setClientBehaviorFlags(PxActorClientBehaviorFlag::eREPORT_TO_FOREIGN_CLIENTS_CONTACT_NOTIFY);
    if (kinematic) actor->setRigidBodyFlags(PxRigidBodyFlag::eKINEMATIC | PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES);
    return actor;
}

static void attach(PxRigidActor& actor, PxShape& shape) {
    const auto before = actor.getNbShapes();
    actor.attachShape(shape);
    if (actor.getNbShapes() != before + 1) throw std::runtime_error("SDK rejected shape attachment");
}

static void snapshot(FeatureContext& context, const std::string& directory) {
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*context.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    const auto types = PxActorTypeFlag::eRIGID_STATIC | PxActorTypeFlag::eRIGID_DYNAMIC;
    std::vector<PxActor*> actors(context.scene->getNbActors(types));
    context.scene->getActors(types, actors.data(), PxU32(actors.size()));
    if (actors.size() != 8) throw std::runtime_error("Unexpected geometry actor count");
    for (auto* actor : actors) resources.collection->add(*actor);
    PxSerialization::complete(*resources.collection, *resources.registry, nullptr, true);
    for (PxU32 index = 0; index < resources.collection->getNbObjects(); ++index) {
        auto& object = resources.collection->getObject(index);
        resources.collection->addId(object, reinterpret_cast<std::uintptr_t>(&object));
    }
    dump_collection(*resources.collection, (directory + "/sdk-truth.json").c_str());
    PxDefaultFileOutputStream output((directory + "/sdk-reference.xml").c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, *resources.collection, *resources.registry))
        throw std::runtime_error("Geometry reference XML serialization failed");
}

static void zero_flags_probe(const std::string& directory) {
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, sdk.errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *sdk.foundation, PxTolerancesScale()), "physics");
    sdk.extensions = PxInitExtensions(*sdk.physics, nullptr);
    if (!sdk.extensions) throw std::runtime_error("Extensions initialization failed");
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*sdk.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    auto* material = require(sdk.physics->createMaterial(0.4f, 0.3f, 0.2f), "probe material");
    auto* actor = require(sdk.physics->createRigidStatic(PxTransform(PxVec3(1, 2, 3))), "zero-flags actor");
    actor->setName("zero-flags-box");
    actor->setActorFlags(PxActorFlags());
    auto* shape = require(sdk.physics->createShape(PxBoxGeometry(0.5f, 0.7f, 0.9f), *material, true, PxShapeFlags()), "zero-flags shape");
    attach(*actor, *shape); shape->release();
    resources.collection->add(*actor);
    PxSerialization::complete(*resources.collection, *resources.registry);
    for (PxU32 index = 0; index < resources.collection->getNbObjects(); ++index) {
        auto& object = resources.collection->getObject(index);
        resources.collection->addId(object, reinterpret_cast<std::uintptr_t>(&object));
    }
    dump_collection(*resources.collection, (directory + "/zero-flags-truth.json").c_str());
    PxDefaultFileOutputStream output((directory + "/zero-flags-reference.xml").c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, *resources.collection, *resources.registry))
        throw std::runtime_error("Zero-flags reference XML serialization failed");
    actor->release(); material->release();
}

static void produce(const char* capture, const std::string& directory) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    auto* alternate = require(context.physics->createMaterial(0.8f, 0.6f, 0.45f), "alternate material");
    alternate->setFrictionCombineMode(PxCombineMode::eMAX);
    auto* triangle = cook_probe_triangle(context);
    auto* height_field = cook_probe_height_field(context);
    PxMaterial* table[] = {context.material, alternate};
    const PxShapeFlags simulation = PxShapeFlag::eSIMULATION_SHAPE | PxShapeFlag::eSCENE_QUERY_SHAPE | PxShapeFlag::eVISUALIZATION;
    const PxShapeFlags query_only = PxShapeFlag::eSCENE_QUERY_SHAPE | PxShapeFlag::eVISUALIZATION;
    const PxTransform local(PxVec3(0.1f, -0.2f, 0.3f), PxQuat(-0.2f, PxVec3(1, 0, 0)));
    const PxMeshScale scale(PxVec3(-1.3f, 0.8f, 1.7f), PxQuat(0.3f, PxVec3(0, 1, 0)));

    auto* plane_actor = body(context, "kin-plane", 0, true);
    auto* plane = require(context.physics->createShape(PxPlaneGeometry(), *context.material, true, simulation), "kinematic plane");
    plane->setLocalPose(local); attach(*plane_actor, *plane); plane->release(); context.scene->addActor(*plane_actor);

    auto* triangle_actor = body(context, "kin-triangle-exclusive", 10, true);
    auto* mesh = require(context.physics->createShape(PxTriangleMeshGeometry(triangle, scale, PxMeshGeometryFlag::eDOUBLE_SIDED), table, 2, true, simulation), "kinematic triangle");
    mesh->setLocalPose(local); attach(*triangle_actor, *mesh); mesh->release(); context.scene->addActor(*triangle_actor);

    auto* field_actor = body(context, "kin-heightfield", 20, true);
    auto* field = require(context.physics->createShape(PxHeightFieldGeometry(height_field, PxMeshGeometryFlag::eDOUBLE_SIDED, 0.25f, 1.2f, 0.8f), table, 2, true, simulation), "kinematic height field");
    field->setLocalPose(local); attach(*field_actor, *field); field->release(); context.scene->addActor(*field_actor);

    auto* query_actor = body(context, "query-plane", 30, false);
    auto* query = require(context.physics->createShape(PxPlaneGeometry(), *context.material, true, query_only), "query-only plane");
    query->setLocalPose(local); attach(*query_actor, *query); query->release(); context.scene->addActor(*query_actor);
    query_actor->setWakeCounter(0);

    auto* first = body(context, "kin-shared-a", 40, true);
    auto* second = body(context, "kin-shared-b", 50, true);
    auto* fixed = require(context.physics->createRigidStatic(PxTransform(PxVec3(60, 3, 2))), "shared static owner");
    fixed->setName("static-shared");
    auto* shared = require(context.physics->createShape(PxTriangleMeshGeometry(triangle, scale, PxMeshGeometryFlag::eDOUBLE_SIDED), table, 2, false, simulation), "shared triangle");
    shared->setLocalPose(local); attach(*first, *shared); attach(*second, *shared); attach(*fixed, *shared); shared->release();
    context.scene->addActor(*first); context.scene->addActor(*second); context.scene->addActor(*fixed);
    auto* zero_actor = require(context.physics->createRigidStatic(PxTransform(PxVec3(70, 3, 2))), "zero-flags actor");
    zero_actor->setName("zero-flags-box"); zero_actor->setActorFlags(PxActorFlags());
    auto* zero_shape = require(context.physics->createShape(PxBoxGeometry(0.5f, 0.7f, 0.9f), *context.material, true, PxShapeFlags()), "zero-flags box");
    attach(*zero_actor, *zero_shape); zero_shape->release(); context.scene->addActor(*zero_actor);
    triangle->release(); height_field->release(); alternate->release();
    context.step();
    snapshot(context, directory);
    std::ofstream manifest(directory + "/fixture.json");
    manifest << "{\"schema\":1,\"sdk_version\":[" << PX_PHYSICS_VERSION_MAJOR << ',' << PX_PHYSICS_VERSION_MINOR << ',' << PX_PHYSICS_VERSION_BUGFIX
        << "],\"frames\":1,\"actors\":8,\"kinematic_actors\":5}\n";
    manifest.close();
    if (!manifest) throw std::runtime_error("Geometry fixture manifest write failed");
}

int main(int argc, char** argv) {
    try {
        if (argc == 5 && std::string(argv[1]) == "--import") import_collection(argv[2], argv[3], argv[4]);
        else if (argc == 3 && std::string(argv[1]) == "--zero-flags-probe") zero_flags_probe(argv[2]);
        else if (argc == 3) produce(argv[1], argv[2]);
        else throw std::runtime_error("Usage: pvd-repx-kinematic-shapes capture.pvdstream output-directory | --import input.xml truth.json roundtrip.xml | --zero-flags-probe output-directory");
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
