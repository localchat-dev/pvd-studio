#pragma once
#include "extensions/PxCollectionExt.h"

struct RepxResources {
    PxSerializationRegistry* registry = nullptr;
    PxCollection* collection = nullptr;
    bool own_objects = false;

    ~RepxResources() {
        if (collection) {
            if (own_objects) PxCollectionExt::releaseObjects(*collection);
            collection->release();
        }
        if (registry) registry->release();
    }
};

void export_repx(PxPhysics& physics, PxScene& scene, const std::string& path) {
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(physics), "serialization registry");
    resources.collection = require(PxCreateCollection(), "serialization collection");
    const auto types = PxActorTypeFlag::eRIGID_STATIC | PxActorTypeFlag::eRIGID_DYNAMIC;
    std::vector<PxActor*> actors(scene.getNbActors(types));
    scene.getActors(types, actors.data(), static_cast<PxU32>(actors.size()));
    for (auto* actor : actors) resources.collection->add(*actor);
    PxSerialization::complete(*resources.collection, *resources.registry, nullptr, true);
    PxSerialization::createSerialObjectIds(*resources.collection, 1);
    PxDefaultFileOutputStream output(path.c_str());
    if (!output.isValid()) throw std::runtime_error("Cannot create RepX: " + path);
    PxVec3 up(0, 1, 0);
    PxSerialization::PxXmlMiscParameter parameters(up, physics.getTolerancesScale());
    if (!PxSerialization::serializeCollectionToXml(output, *resources.collection, *resources.registry, nullptr, nullptr, &parameters)) {
        throw std::runtime_error("SDK RepX serialization failed.");
    }
    std::cout << "repx_export_objects=" << resources.collection->getNbObjects() << '\n';
}

struct RepxImportEnvironment {
    PxDefaultAllocator allocator;
    PxDefaultErrorCallback errors;
    PxFoundation* foundation = nullptr;
    PxPhysics* physics = nullptr;
    PxCooking* cooking = nullptr;
    PxStringTable* strings = nullptr;
    bool extensions = false;

    ~RepxImportEnvironment() {
        if (strings) strings->release();
        if (cooking) cooking->release();
        if (extensions) PxCloseExtensions();
        if (physics) physics->release();
        if (foundation) foundation->release();
    }
};

void import_repx(const std::string& input_path, const std::string& output_path) {
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, sdk.errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *sdk.foundation, PxTolerancesScale()), "physics");
    sdk.extensions = PxInitExtensions(*sdk.physics, nullptr);
    if (!sdk.extensions) throw std::runtime_error("Cannot initialize serialization extensions.");
    sdk.cooking = require(PxCreateCooking(PX_PHYSICS_VERSION, *sdk.foundation, PxCookingParams(sdk.physics->getTolerancesScale())), "cooking");
    sdk.strings = &PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*sdk.physics), "serialization registry");
    PxDefaultFileInputData input(input_path.c_str());
    if (!input.isValid()) throw std::runtime_error("Cannot open RepX: " + input_path);
    PxSerialization::PxXmlMiscParameter parameters;
    resources.collection = require(PxSerialization::createCollectionFromXml(input, *sdk.cooking, *resources.registry, nullptr, sdk.strings, &parameters), "RepX collection");
    resources.own_objects = true;
    unsigned actors = 0, shapes = 0, materials = 0;
    for (PxU32 index = 0; index < resources.collection->getNbObjects(); ++index) {
        const auto& object = resources.collection->getObject(index);
        actors += object.is<PxRigidActor>() != nullptr;
        shapes += object.is<PxShape>() != nullptr;
        materials += object.is<PxMaterial>() != nullptr;
    }
    if (!output_path.empty()) {
        PxDefaultFileOutputStream output(output_path.c_str());
        if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, *resources.collection,
            *resources.registry, nullptr, nullptr, &parameters)) {
            throw std::runtime_error("SDK RepX roundtrip serialization failed.");
        }
    }
    std::cout << "repx_import_objects=" << resources.collection->getNbObjects()
        << " actors=" << actors << " shapes=" << shapes << " materials=" << materials << '\n';
}
