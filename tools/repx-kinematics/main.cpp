#include "../physx-features/feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>

template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"

struct ImportErrors : PxErrorCallback {
    std::vector<std::pair<int, std::string>> records;
    void reportError(PxErrorCode::Enum code, const char* message, const char*, int) override {
        records.emplace_back(int(code), message);
    }
};

static void pose(std::ostream& out, const PxTransform& value) {
    out << '[' << value.p.x << ',' << value.p.y << ',' << value.p.z << ','
        << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << ']';
}

static void vector(std::ostream& out, const PxVec3& value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void dump_actor(PxRigidDynamic& actor, PxSerialObjectId id, const std::string& path,
                       const ImportErrors& errors) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot create kinematic getter output");
    PxTransform target(PxIdentity);
    const bool valid = actor.getKinematicTarget(target);
    out << std::setprecision(9) << std::boolalpha << "{\"schema\":2,\"id\":" << id
        << ",\"flags\":" << PxU32(actor.getRigidBodyFlags()) << ",\"pose\":";
    pose(out, actor.getGlobalPose());
    out << ",\"cmass\":"; pose(out, actor.getCMassLocalPose());
    out << ",\"linear\":"; vector(out, actor.getLinearVelocity());
    out << ",\"angular\":"; vector(out, actor.getAngularVelocity());
    out << ",\"mass\":" << actor.getMass() << ",\"inertia\":";
    vector(out, actor.getMassSpaceInertiaTensor());
    PxU32 position_iterations, velocity_iterations;
    actor.getSolverIterationCounts(position_iterations, velocity_iterations);
    out << ",\"damping\":[" << actor.getLinearDamping() << ',' << actor.getAngularDamping()
        << "],\"max_angular_velocity\":" << actor.getMaxAngularVelocity()
        << ",\"ccd\":" << actor.getMinCCDAdvanceCoefficient()
        << ",\"depenetration\":" << actor.getMaxDepenetrationVelocity()
        << ",\"impulse\":" << actor.getMaxContactImpulse()
        << ",\"sleep_threshold\":" << actor.getSleepThreshold()
        << ",\"stabilization_threshold\":" << actor.getStabilizationThreshold()
        << ",\"wake\":" << actor.getWakeCounter()
        << ",\"contact_threshold\":" << actor.getContactReportThreshold()
        << ",\"iterations\":[" << position_iterations << ',' << velocity_iterations
        << "],\"actor_flags\":" << PxU32(actor.getActorFlags())
        << ",\"client_behavior\":" << PxU32(actor.getClientBehaviorFlags())
        << ",\"lock_flags\":" << PxU32(actor.getRigidDynamicLockFlags())
        << ",\"dominance\":" << PxU32(actor.getDominanceGroup())
        << ",\"owner_client\":" << PxU32(actor.getOwnerClient())
        << ",\"shapes\":" << actor.getNbShapes();
    out << ",\"target_valid\":" << valid << ",\"target\":";
    if (valid) pose(out, target); else out << "null";
    out << ",\"body_target\":";
    if (valid) pose(out, target * actor.getCMassLocalPose()); else out << "null";
    out << ",\"diagnostics\":[";
    for (std::size_t i = 0; i < errors.records.size(); ++i) {
        if (i) out << ',';
        out << "{\"code\":" << errors.records[i].first << ",\"message\":"
            << std::quoted(errors.records[i].second) << '}';
    }
    out << "]}\n";
    out.close();
    if (!out) throw std::runtime_error("Kinematic getter write failed");
}

static void snapshot(FeatureContext& context, PxRigidDynamic& actor, const std::string& prefix) {
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*context.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    resources.collection->add(actor);
    PxSerialization::complete(*resources.collection, *resources.registry);
    for (PxU32 i = 0; i < resources.collection->getNbObjects(); ++i) {
        auto& object = resources.collection->getObject(i);
        resources.collection->addId(object, reinterpret_cast<std::uintptr_t>(&object));
    }
    dump_actor(actor, resources.collection->getId(actor), prefix + "-truth.json", ImportErrors());
    PxDefaultFileOutputStream output((prefix + "-reference.xml").c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, *resources.collection, *resources.registry))
        throw std::runtime_error("Kinematic reference XML serialization failed");
}

static void probe_flags(const std::string& directory) {
    ImportErrors errors;
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *sdk.foundation, PxTolerancesScale()), "physics");
    RepxResources resources;
    resources.collection = require(PxCreateCollection(), "collection");
    resources.own_objects = true;
    auto* actor = require(sdk.physics->createRigidDynamic(PxTransform(PxIdentity)), "flag probe actor");
    resources.collection->add(*actor);
    const auto before = PxU32(actor->getRigidBodyFlags());
    actor->setRigidBodyFlags(PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES);
    const auto query_only = PxU32(actor->getRigidBodyFlags());
    const auto requested = PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES
        | PxRigidBodyFlag::eENABLE_CCD | PxRigidBodyFlag::eENABLE_SPECULATIVE_CCD;
    actor->setRigidBodyFlags(requested);
    const auto filtered = PxU32(actor->getRigidBodyFlags());
    actor->setRigidBodyFlags(PxRigidBodyFlags());
    const auto after_clear = PxU32(actor->getRigidBodyFlags());
    actor->setRigidBodyFlags(PxRigidBodyFlag::eKINEMATIC | PxRigidBodyFlag::eENABLE_CCD);
    const auto kinematic_ccd = PxU32(actor->getRigidBodyFlags());
    actor->setRigidBodyFlags(PxRigidBodyFlag::eKINEMATIC | PxRigidBodyFlag::eENABLE_SPECULATIVE_CCD);
    const auto kinematic_speculative = PxU32(actor->getRigidBodyFlags());
    actor->setRigidBodyFlags(PxRigidBodyFlags());
    std::ofstream out(directory + "/flag-probe.json");
    out << "{\"before\":" << before << ",\"query_only\":" << query_only
        << ",\"requested_conflict\":" << PxU32(requested) << ",\"filtered\":" << filtered
        << ",\"after_clear\":" << after_clear
        << ",\"kinematic_ccd_requested\":5,\"kinematic_ccd_filtered\":" << kinematic_ccd
        << ",\"kinematic_speculative\":" << kinematic_speculative
        << ",\"kinematic_after_clear\":" << PxU32(actor->getRigidBodyFlags()) << ",\"diagnostics\":[";
    for (std::size_t i = 0; i < errors.records.size(); ++i) {
        if (i) out << ',';
        out << "{\"code\":" << errors.records[i].first << ",\"message\":"
            << std::quoted(errors.records[i].second) << '}';
    }
    out << "]}\n";
    out.close();
    if (!out) throw std::runtime_error("Flag probe output failed");
}

static void captured_frame(FeatureContext& context, PxRigidDynamic& actor, const std::string& prefix,
                           bool capture_poststep = false) {
    snapshot(context, actor, prefix + "-prestep");
    if (!capture_poststep) snapshot(context, actor, prefix);
    context.flush_and_require_connected();
    const auto before = [&] { std::lock_guard<PxPvdTransport> lock(*context.transport); return context.transport->getWrittenDataSize(); }();
    context.step();
    context.flush_and_require_connected();
    const auto after = [&] { std::lock_guard<PxPvdTransport> lock(*context.transport); return context.transport->getWrittenDataSize(); }();
    snapshot(context, actor, prefix + "-poststep");
    if (capture_poststep) snapshot(context, actor, prefix);
    std::ofstream boundary(prefix + "-boundary.json");
    if (!boundary) throw std::runtime_error("Cannot create frame boundary output");
    boundary << "{\"schema\":2,\"prestep_bytes\":" << before << ",\"poststep_bytes\":" << after << "}\n";
    boundary.close();
    if (!boundary) throw std::runtime_error("Frame boundary write failed");
}

static void produce(const char* capture, const std::string& directory) {
    probe_flags(directory);
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    auto* actor = require(context.physics->createRigidDynamic(PxTransform(PxVec3(1, 2, 3))), "actor");
    actor->setName("kinematic-target");
    actor->setMass(2.5f);
    actor->setMassSpaceInertiaTensor(PxVec3(3.0f, 4.0f, 5.0f));
    actor->setLinearDamping(0.17f); actor->setAngularDamping(0.23f);
    actor->setMaxAngularVelocity(12.0f); actor->setMinCCDAdvanceCoefficient(0.21f);
    actor->setMaxDepenetrationVelocity(8.0f); actor->setMaxContactImpulse(9.0f);
    actor->setSleepThreshold(0.0005f); actor->setStabilizationThreshold(0.003f);
    actor->setSolverIterationCounts(9, 2); actor->setContactReportThreshold(13.0f);
    actor->setClientBehaviorFlags(PxActorClientBehaviorFlag::eREPORT_TO_FOREIGN_CLIENTS_CONTACT_NOTIFY);
    actor->setRigidBodyFlags(PxRigidBodyFlag::eKINEMATIC | PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES);
    actor->setCMassLocalPose(PxTransform(PxVec3(0.25f, 0.5f, 0.75f)));
    require(PxRigidActorExt::createExclusiveShape(*actor, PxBoxGeometry(PxVec3(0.5f)), *context.material), "shape");
    context.scene->addActor(*actor);
    captured_frame(context, *actor, directory + "/frame-0");
    const PxTransform targets[] = {
        PxTransform(PxVec3(4, 5, 6)), PxTransform(PxVec3(7, 8, 9), PxQuat(0, 0, 1, 0)),
    };
    for (unsigned i = 0; i < 2; ++i) {
        actor->setKinematicTarget(targets[i]);
        const auto label = std::to_string(i + 1);
        snapshot(context, *actor, directory + "/pending-" + label);
        captured_frame(context, *actor, directory + "/frame-" + label);
    }
    captured_frame(context, *actor, directory + "/frame-3");
    actor->setRigidBodyFlag(PxRigidBodyFlag::eKINEMATIC, false);
    actor->setLinearVelocity(PxVec3(0.25f, -0.5f, 0.75f));
    actor->setAngularVelocity(PxVec3(0.1f, 0.2f, 0.3f));
    actor->setWakeCounter(0.9f);
    captured_frame(context, *actor, directory + "/frame-4");
    actor->setRigidBodyFlag(PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES, false);
    actor->setLinearVelocity(PxVec3(-0.2f, 0.4f, 0.6f));
    actor->setAngularVelocity(PxVec3(-0.3f, 0.2f, 0.1f));
    actor->setWakeCounter(0.7f);
    captured_frame(context, *actor, directory + "/frame-5");
    actor->setRigidBodyFlag(PxRigidBodyFlag::eKINEMATIC, true);
    actor->setKinematicTarget(PxTransform(PxVec3(-2, 3, 4), PxQuat(PxPi / 2, PxVec3(0, 1, 0))));
    snapshot(context, *actor, directory + "/pending-3");
    captured_frame(context, *actor, directory + "/frame-6", true);
    captured_frame(context, *actor, directory + "/frame-7");
    actor->release();
}

static void read_reference(const char* path, const char* truth) {
    ImportErrors errors;
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *sdk.foundation, PxTolerancesScale()), "physics");
    sdk.extensions = PxInitExtensions(*sdk.physics, nullptr);
    if (!sdk.extensions) throw std::runtime_error("Extensions initialization failed");
    sdk.cooking = require(PxCreateCooking(PX_PHYSICS_VERSION, *sdk.foundation, PxCookingParams(sdk.physics->getTolerancesScale())), "cooking");
    sdk.strings = &PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*sdk.physics), "registry");
    PxDefaultFileInputData input(path);
    if (!input.isValid()) throw std::runtime_error("Cannot open kinematic XML");
    resources.collection = require(PxSerialization::createCollectionFromXml(input, *sdk.cooking, *resources.registry, nullptr, sdk.strings), "collection");
    resources.own_objects = true;
    PxRigidDynamic* actor = nullptr;
    for (PxU32 i = 0; i < resources.collection->getNbObjects(); ++i) {
        if (auto* value = resources.collection->getObject(i).is<PxRigidDynamic>()) {
            if (actor) throw std::runtime_error("Expected one imported kinematic actor");
            actor = value;
        }
    }
    require(actor, "imported actor");
    dump_actor(*actor, resources.collection->getId(*actor), truth, errors);
}

int main(int argc, char** argv) {
    try {
        if (argc == 4 && std::string(argv[1]) == "--import") read_reference(argv[2], argv[3]);
        else if (argc == 3) produce(argv[1], argv[2]);
        else throw std::runtime_error("Usage: pvd-repx-kinematics capture.pvdstream output-directory | --import input.xml truth.json");
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
