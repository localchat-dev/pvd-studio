#include "../physx-features/feature_harness.h"
#include <algorithm>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>

template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"

static PxSerialObjectId id(PxCollection& collection, PxBase* value) {
    if (!value) return 0;
    const auto result = collection.getId(*value);
    if (!result) throw std::runtime_error("Missing collection dependency ID");
    return result;
}

static void pose(std::ostream& out, const PxTransform& value) {
    out << "[" << value.p.x << ',' << value.p.y << ',' << value.p.z << ','
        << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << ']';
}

static void dump(PxCollection& collection, const std::string& path) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot open aggregate getter output");
    out << std::setprecision(9) << std::boolalpha << "{\"actors\":[";
    bool first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* actor = collection.getObject(i).is<PxRigidActor>();
        if (!actor) continue;
        if (!first) out << ',';
        first = false;
        out << "{\"id\":" << id(collection, actor) << ",\"name\":\"" << actor->getName()
            << "\",\"type\":\"" << actor->getConcreteTypeName() << "\",\"aggregate\":"
            << id(collection, actor->getAggregate()) << ",\"pose\":";
        pose(out, actor->getGlobalPose());
        out << ",\"shapes\":" << actor->getNbShapes() << '}';
    }
    out << "],\"aggregates\":[";
    first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* aggregate = collection.getObject(i).is<PxAggregate>();
        if (!aggregate) continue;
        if (!first) out << ',';
        first = false;
        out << "{\"id\":" << id(collection, aggregate) << ",\"capacity\":" << aggregate->getMaxNbActors()
            << ",\"self_collision\":" << aggregate->getSelfCollision() << ",\"members\":[";
        std::vector<PxActor*> actors(aggregate->getNbActors());
        aggregate->getActors(actors.data(), static_cast<PxU32>(actors.size()));
        for (std::size_t member = 0; member < actors.size(); ++member) {
            if (member) out << ',';
            out << id(collection, actors[member]);
        }
        out << "]}";
    }
    out << "],\"joints\":[";
    first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* joint = collection.getObject(i).is<PxJoint>();
        if (!joint) continue;
        if (!first) out << ',';
        first = false;
        PxRigidActor *a = nullptr, *b = nullptr;
        joint->getActors(a, b);
        out << "{\"id\":" << id(collection, joint) << ",\"actors\":[" << id(collection, a) << ','
            << id(collection, b) << "],\"type\":\"" << joint->getConcreteTypeName() << "\"}";
    }
    out << "]}\n";
    out.close();
    if (!out) throw std::runtime_error("Aggregate getter output failed");
}

static void serialize(PxCollection& collection, PxSerializationRegistry& registry, const std::string& path) {
    PxDefaultFileOutputStream output(path.c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, collection, registry)) {
        throw std::runtime_error("Aggregate XML serialization failed");
    }
}

static void snapshot(FeatureContext& context, const std::string& directory, unsigned frame) {
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*context.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    const auto types = PxActorTypeFlag::eRIGID_STATIC | PxActorTypeFlag::eRIGID_DYNAMIC;
    std::vector<PxActor*> actors(context.scene->getNbActors(types));
    context.scene->getActors(types, actors.data(), static_cast<PxU32>(actors.size()));
    for (auto* actor : actors) resources.collection->add(*actor);
    std::vector<PxAggregate*> aggregates(context.scene->getNbAggregates());
    context.scene->getAggregates(aggregates.data(), static_cast<PxU32>(aggregates.size()));
    for (auto* aggregate : aggregates) resources.collection->add(*aggregate);
    PxSerialization::complete(*resources.collection, *resources.registry, nullptr, true);
    for (PxU32 i = 0; i < resources.collection->getNbObjects(); ++i) {
        auto& object = resources.collection->getObject(i);
        resources.collection->addId(object, reinterpret_cast<std::uintptr_t>(&object));
    }
    const auto prefix = directory + "/frame-" + std::to_string(frame);
    dump(*resources.collection, prefix + "-truth.json");
    serialize(*resources.collection, *resources.registry, prefix + "-reference.xml");
}

static void import_collection(const char* path, const char* truth, const char* roundtrip) {
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, sdk.errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *sdk.foundation, PxTolerancesScale()), "physics");
    sdk.extensions = PxInitExtensions(*sdk.physics, nullptr);
    if (!sdk.extensions) throw std::runtime_error("Extensions initialization failed");
    sdk.cooking = require(PxCreateCooking(PX_PHYSICS_VERSION, *sdk.foundation, PxCookingParams(sdk.physics->getTolerancesScale())), "cooking");
    sdk.strings = &PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*sdk.physics), "registry");
    PxDefaultFileInputData input(path);
    if (!input.isValid()) throw std::runtime_error("Cannot open aggregate XML");
    resources.collection = require(PxSerialization::createCollectionFromXml(input, *sdk.cooking, *resources.registry, nullptr, sdk.strings), "aggregate collection");
    resources.own_objects = true;
    dump(*resources.collection, truth);
    serialize(*resources.collection, *resources.registry, roundtrip);
}

static void produce(const char* capture, const std::string& directory) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    std::vector<PxRigidActor*> actors;
    const char* names[] = {"aggregate-a", "aggregate-b", "aggregate-c", "aggregate-d", "unrelated"};
    for (unsigned i = 0; i != 5; ++i) {
        const PxTransform transform(PxVec3(float(i) * 3.0f, 5.0f, 0));
        PxRigidActor* actor = i == 3
            ? static_cast<PxRigidActor*>(context.physics->createRigidStatic(transform))
            : static_cast<PxRigidActor*>(context.physics->createRigidDynamic(transform));
        require(actor, "actor");
        actor->setName(names[i]);
        require(PxRigidActorExt::createExclusiveShape(*actor, PxBoxGeometry(0.2f, 0.3f, 0.4f), *context.material), "shape");
        if (auto* body = actor->is<PxRigidBody>()) {
            if (!PxRigidBodyExt::updateMassAndInertia(*body, 1.0f)) throw std::runtime_error("Mass update failed");
        }
        actors.push_back(actor);
    }
    auto* first = require(context.physics->createAggregate(4, false), "aggregate one");
    auto* second = require(context.physics->createAggregate(5, true), "aggregate two");
    auto* empty = require(context.physics->createAggregate(0, false), "empty aggregate");
    if (!first->addActor(*actors[0]) || !first->addActor(*actors[1])
        || !second->addActor(*actors[2]) || !second->addActor(*actors[3])) throw std::runtime_error("Aggregate insertion failed");
    context.scene->addAggregate(*first);
    context.scene->addAggregate(*second);
    context.scene->addAggregate(*empty);
    context.scene->addActor(*actors[4]);
    auto joint_between = [&](unsigned a, unsigned b) {
        const auto relative = actors[a]->getGlobalPose().transformInv(actors[b]->getGlobalPose());
        auto* joint = require(PxFixedJointCreate(*context.physics, actors[a], relative, actors[b], PxTransform(PxIdentity)), "fixed joint");
        joint->setName("aggregate-bridge");
        return joint;
    };
    auto* joint = joint_between(1, 2);
    for (unsigned frame = 0; frame != 4; ++frame) {
        if (frame == 1) {
            joint->release(); joint = nullptr;
            if (!first->removeActor(*actors[1])) throw std::runtime_error("Aggregate removal failed");
        } else if (frame == 2) {
            context.scene->removeActor(*actors[1]);
            if (!second->addActor(*actors[1])) throw std::runtime_error("Aggregate transfer failed");
            joint = joint_between(0, 1);
        } else if (frame == 3) {
            joint->release(); joint = nullptr;
            if (!first->removeActor(*actors[0])) throw std::runtime_error("Aggregate emptying failed");
        }
        snapshot(context, directory, frame);
        context.step();
    }
    for (auto* actor : actors) actor->release();
    first->release(); second->release(); empty->release();
}

int main(int argc, char** argv) {
    try {
        if (argc == 5 && std::string(argv[1]) == "--import") import_collection(argv[2], argv[3], argv[4]);
        else if (argc == 3) produce(argv[1], argv[2]);
        else throw std::runtime_error("Usage: pvd-repx-aggregates capture.pvdstream output-directory | --import input.xml truth.json roundtrip.xml");
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
