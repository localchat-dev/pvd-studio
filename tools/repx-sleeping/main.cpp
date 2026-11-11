#include "feature_harness.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <memory>
#include <vector>

template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "repx_probe.h"

static std::uintptr_t id(const void* value) { return reinterpret_cast<std::uintptr_t>(value); }

static void vector_json(std::ostream& out, const PxVec3& value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void pose_json(std::ostream& out, const PxTransform& value) {
    out << "{\"position\":";
    vector_json(out, value.p);
    out << ",\"rotation\":[" << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << "]}";
}

static void actor_json(std::ostream& out, PxRigidDynamic& actor) {
    PxTransform target(PxIdentity);
    const bool valid = actor.getKinematicTarget(target);
    out << "{\"id\":" << id(&actor) << ",\"scene\":" << id(actor.getScene())
        << ",\"flags\":" << static_cast<PxU32>(actor.getRigidBodyFlags())
        << ",\"actor_flags\":" << static_cast<PxU32>(actor.getActorFlags())
        << ",\"is_sleeping\":";
    if (actor.getScene()) out << actor.isSleeping(); else out << "null";
    out << ",\"pose\":";
    pose_json(out, actor.getGlobalPose());
    out << ",\"cmass\":";
    pose_json(out, actor.getCMassLocalPose());
    out << ",\"linear\":";
    vector_json(out, actor.getLinearVelocity());
    out << ",\"angular\":";
    vector_json(out, actor.getAngularVelocity());
    out << ",\"wake\":" << actor.getWakeCounter()
        << ",\"target_valid\":" << valid << ",\"target\":";
    if (valid) pose_json(out, target); else out << "null";
    out << ",\"body_target\":";
    if (valid) pose_json(out, target * actor.getCMassLocalPose()); else out << "null";
    out << '}';
}

static void reference_xml(FeatureContext& context, PxRigidDynamic& actor, const std::string& path) {
    if (std::ifstream(path, std::ios::binary).good()) throw std::runtime_error("Reference XML must not already exist");
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*context.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    resources.collection->add(actor);
    PxSerialization::complete(*resources.collection, *resources.registry);
    for (PxU32 index = 0; index < resources.collection->getNbObjects(); ++index) {
        auto& object = resources.collection->getObject(index);
        resources.collection->addId(object, id(&object));
    }
    PxDefaultFileOutputStream output(path.c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, *resources.collection, *resources.registry)) {
        throw std::runtime_error("Cannot serialize sleeping oracle reference XML");
    }
}

struct ReleaseBody {
    void operator()(PxRigidDynamic* actor) const { actor->release(); }
};

struct Simulation {
    PxScene& scene;
    bool active = false;
    explicit Simulation(PxScene& value) : scene(value) {}
    void start() {
        scene.simulate(1.0f / 60.0f);
        active = true;
    }
    void finish() {
        if (!scene.fetchResults(true)) throw std::runtime_error("SDK fetchResults failed");
        active = false;
    }
    void step() { start(); finish(); }
    ~Simulation() { if (active) scene.fetchResults(true); }
};

struct Oracle {
    FeatureContext& context;
    const std::string directory;
    std::ofstream out;
    bool first = true;

    Oracle(FeatureContext& value, const std::string& destination, bool initial_buffered)
        : context(value), directory(destination), out(destination + "/oracle.json") {
        if (!out) throw std::runtime_error("Cannot create oracle JSON; create the output directory first");
        out << std::setprecision(9) << std::boolalpha << "{\"sdk\":\"" << feature_sdk_version()
            << "\",\"initial_buffered\":" << initial_buffered << ",\"scene\":" << id(context.scene) << ",\"phases\":[";
    }

    void phase(const char* name, unsigned completed, PxRigidDynamic* actor, bool save_reference = true) {
        if (!first) out << ',';
        first = false;
        out << "{\"name\":" << std::quoted(name) << ",\"completed_steps\":" << completed << ",\"actor\":";
        if (actor) actor_json(out, *actor); else out << "null";
        if (save_reference && actor && actor->getScene()) {
            const auto reference = std::string(name) + "-reference.xml";
            reference_xml(context, *actor, directory + "/" + reference);
            out << ",\"reference\":" << std::quoted(reference);
        }
        {
            std::lock_guard<PxPvdTransport> lock(*context.transport);
            context.transport->flush();
            out << ",\"wire_bytes\":" << context.transport->getWrittenDataSize();
        }
        out << '}';
        out.flush();
        if (!out) throw std::runtime_error("Sleeping oracle phase write failed");
    }

    void finish(unsigned completed) {
        if (completed != 10) throw std::runtime_error("Unexpected SDK oracle frame count");
        out << "],\"frames\":" << completed << "}\n";
        out.close();
        if (!out) throw std::runtime_error("Sleeping oracle JSON write failed");
    }
};

static void wait_for_sleep(Simulation& simulation, Oracle& oracle, PxRigidDynamic& actor,
    unsigned& completed, const char* prefix) {
    unsigned idle = 0;
    while (!actor.isSleeping() && idle < 4) {
        simulation.step();
        ++completed;
        const auto name = std::string(prefix) + "-idle-" + std::to_string(++idle);
        oracle.phase(name.c_str(), completed, &actor);
    }
    if (!actor.isSleeping()) throw std::runtime_error("Kinematic did not sleep within four idle steps");
    const auto name = std::string(prefix) + "-sleep-confirmed";
    oracle.phase(name.c_str(), completed, &actor);
}

static bool exists(const std::string& path) {
    return std::ifstream(path, std::ios::binary).good();
}

int main(int argc, char** argv) {
    try {
        const bool initial_buffered = argc == 4 && std::string(argv[3]) == "--initial-buffered";
        if (argc != 3 && !initial_buffered) {
            throw std::runtime_error("Usage: pvd-repx-sleeping capture.pvdstream output-directory [--initial-buffered]");
        }
        if (exists(argv[1]) || exists(std::string(argv[2]) + "/oracle.json")) {
            throw std::runtime_error("Capture and oracle outputs must not already exist");
        }
        FeatureContext context(argv[1], PxPvdInstrumentationFlag::eDEBUG);
        if (feature_sdk_version() != "3.4.2") throw std::runtime_error("This oracle requires PhysX 3.4.2");
        context.scene->setGravity(PxVec3(0));
        std::unique_ptr<PxRigidDynamic, ReleaseBody> actor(
            require(context.physics->createRigidDynamic(PxTransform(PxVec3(1, 2, 3))), "oracle actor"));
        actor->setName("sleeping-kinematic-oracle");
        actor->setMass(2.5f);
        actor->setMassSpaceInertiaTensor(PxVec3(3, 4, 5));
        actor->setRigidBodyFlags(PxRigidBodyFlag::eKINEMATIC | PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES);
        actor->setCMassLocalPose(PxTransform(PxVec3(0.25f, 0.5f, 0.75f), PxQuat(0.17f, PxVec3(0, 0, 1))));
        require(PxRigidActorExt::createExclusiveShape(*actor, PxBoxGeometry(PxVec3(0.5f)), *context.material), "oracle shape");
        context.scene->addActor(*actor);
        Simulation simulation(*context.scene);
        Oracle oracle(context, argv[2], initial_buffered);
        unsigned completed = 0;
        oracle.phase("created", completed, actor.get());
        const PxTransform first_target(PxVec3(4, 5, 6), PxQuat(0.31f, PxVec3(0, 1, 0)));
        if (initial_buffered) {
            simulation.start();
            oracle.phase("initial-sim-before-target", completed, actor.get(), false);
            actor->setKinematicTarget(first_target);
            oracle.phase("initial-buffered-pending", completed, actor.get(), false);
            simulation.finish();
            oracle.phase("initial-buffered-after-fetch", ++completed, actor.get());
            simulation.step();
            oracle.phase("initial-buffered-executed", ++completed, actor.get());
        } else {
            simulation.step();
            oracle.phase("sleeping-initial", ++completed, actor.get());
            actor->setKinematicTarget(first_target);
            oracle.phase("immediate-pending", completed, actor.get());
            simulation.step();
            oracle.phase("immediate-executed", ++completed, actor.get());
        }
        wait_for_sleep(simulation, oracle, *actor, completed, "first");

        simulation.start();
        oracle.phase("sim-before-buffered-target", completed, actor.get(), false);
        actor->setKinematicTarget(PxTransform(PxVec3(-2, 3, 4), PxQuat(0.43f, PxVec3(1, 0, 0))));
        oracle.phase("buffered-pending", completed, actor.get(), false);
        simulation.finish();
        oracle.phase("buffered-after-fetch", ++completed, actor.get());
        simulation.step();
        oracle.phase("buffered-executed", ++completed, actor.get());
        wait_for_sleep(simulation, oracle, *actor, completed, "second");

        actor->setActorFlag(PxActorFlag::eDISABLE_SIMULATION, true);
        oracle.phase("simulation-disabled-tail", completed, actor.get());
        simulation.step();
        oracle.phase("simulation-disabled-frame", ++completed, actor.get());
        actor->setActorFlag(PxActorFlag::eDISABLE_SIMULATION, false);
        context.scene->removeActor(*actor);
        oracle.phase("off-scene-tail", completed, actor.get(), false);
        actor->setRigidBodyFlag(PxRigidBodyFlag::eUSE_KINEMATIC_TARGET_FOR_SCENE_QUERIES, false);
        context.scene->addActor(*actor);
        simulation.step();
        oracle.phase("readded-sleeping-no-query", ++completed, actor.get());
        actor.reset();
        oracle.phase("released-tail", completed, nullptr, false);
        oracle.finish(completed);
        context.flush_and_require_connected();
        std::cout << "PhysX " << feature_sdk_version() << ": " << completed
            << " steps, one body, SDK phase getters and exact wire cuts\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
