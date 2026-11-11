#include "../physx-features/feature_harness.h"
#include <algorithm>
#include <cstdint>
#include <cstring>
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

static void dump_rigid(PxCollection& collection, const std::string& path) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot open aggregate getter output");
    out << std::setprecision(9) << std::boolalpha << "{\"actors\":[";
    bool first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* actor = collection.getObject(i).is<PxRigidActor>();
        if (!actor || actor->is<PxArticulationLink>()) continue;
        if (!first) out << ',';
        first = false;
        out << "{\"id\":" << id(collection, actor) << ",\"name\":\"" << actor->getName()
            << "\",\"type\":\"" << actor->getConcreteTypeName() << "\",\"aggregate\":"
            << id(collection, actor->getAggregate()) << ",\"pose\":";
        pose(out, actor->getGlobalPose());
        out << ",\"client_behavior\":" << PxU32(actor->getClientBehaviorFlags())
            << ",\"shapes\":" << actor->getNbShapes() << '}';
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

static void vec(std::ostream& out, PxVec3 v) { out << '[' << v.x << ',' << v.y << ',' << v.z << ']'; }
static void quat(std::ostream& out, PxQuat v) { out << '[' << v.x << ',' << v.y << ',' << v.z << ',' << v.w << ']'; }

static void target_bits(std::ostream& out, PxQuat target) {
    const float values[] = {target.x, target.y, target.z, target.w};
    out << '[';
    for (unsigned index = 0; index != 4; ++index) {
        PxU32 bits;
        static_assert(sizeof(bits) == sizeof(values[index]), "f32 bit width");
        std::memcpy(&bits, &values[index], sizeof(bits));
        if (index) out << ',';
        out << bits;
    }
    out << ']';
}

static void dump_articulations(PxCollection& collection, const std::string& path, bool drive_probe = false) {
    std::ofstream out(path);
    out << std::setprecision(9) << std::boolalpha << "{\"articulations\":[";
    bool first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* art = collection.getObject(i).is<PxArticulation>();
        if (!art) continue;
        if (!first) out << ',';
        first = false;
        PxU32 pi, vi; art->getSolverIterationCounts(pi, vi);
        out << "{\"id\":" << id(collection, art) << ",\"name\":\"" << art->getName() << "\",\"aggregate\":" << id(collection, art->getAggregate())
            << ",\"projection\":" << art->getMaxProjectionIterations() << ",\"separation\":" << art->getSeparationTolerance()
            << ",\"internal\":" << art->getInternalDriveIterations() << ",\"external\":" << art->getExternalDriveIterations()
            << ",\"iterations\":[" << pi << ',' << vi << "],\"sleep\":" << art->getSleepThreshold()
            << ",\"stabilization\":" << art->getStabilizationThreshold() << ",\"wake\":" << art->getWakeCounter() << ",\"links\":[";
        std::vector<PxArticulationLink*> links(art->getNbLinks()); art->getLinks(links.data(), PxU32(links.size()));
        for (size_t n = 0; n < links.size(); ++n) {
            if (n) out << ',';
            auto* link = links[n]; auto* joint = link->getInboundJoint();
            out << "{\"id\":" << id(collection, link) << ",\"name\":\"" << link->getName() << "\",\"pose\":"; pose(out, link->getGlobalPose());
            out << ",\"cmass\":"; pose(out, link->getCMassLocalPose());
            out << ",\"mass\":" << link->getMass() << ",\"inertia\":"; vec(out, link->getMassSpaceInertiaTensor());
            out << ",\"linear\":"; vec(out, link->getLinearVelocity()); out << ",\"angular\":"; vec(out, link->getAngularVelocity());
            out << ",\"flags\":" << PxU32(link->getRigidBodyFlags()) << ",\"actor_flags\":" << PxU32(link->getActorFlags())
                << ",\"client_behavior\":" << PxU32(link->getClientBehaviorFlags())
                << ",\"dominance\":" << PxU32(link->getDominanceGroup()) << ",\"owner_client\":" << PxU32(link->getOwnerClient())
                << ",\"ccd\":" << link->getMinCCDAdvanceCoefficient() << ",\"depenetration\":" << link->getMaxDepenetrationVelocity()
                << ",\"impulse\":" << link->getMaxContactImpulse() << ",\"shapes\":" << link->getNbShapes() << ",\"children\":[";
            std::vector<PxArticulationLink*> children(link->getNbChildren()); link->getChildren(children.data(), PxU32(children.size()));
            for (size_t j = 0; j < children.size(); ++j) { if (j) out << ','; out << id(collection, children[j]); }
            out << "]";
            if (joint) {
                PxReal y,z,lo,hi; joint->getSwingLimit(y,z); joint->getTwistLimit(lo,hi);
                out << ",\"joint\":{\"parent_pose\":"; pose(out, joint->getParentPose()); out << ",\"child_pose\":"; pose(out, joint->getChildPose());
                out << ",\"target\":"; quat(out,joint->getTargetOrientation()); out << ",\"velocity\":"; vec(out,joint->getTargetVelocity());
                if (drive_probe) {
                    const auto target = joint->getTargetOrientation();
                    out << ",\"target_bits\":"; target_bits(out, target);
                    out << ",\"target_is_unit\":" << target.isUnit();
                }
                out << ",\"drive\":" << PxU32(joint->getDriveType()) << ",\"stiffness\":" << joint->getStiffness() << ",\"damping\":" << joint->getDamping()
                    << ",\"internal\":" << joint->getInternalCompliance() << ",\"external\":" << joint->getExternalCompliance()
                    << ",\"swing\":[" << y << ',' << z << "],\"twist\":[" << lo << ',' << hi << "]"
                    << ",\"tangent_stiffness\":" << joint->getTangentialStiffness() << ",\"tangent_damping\":" << joint->getTangentialDamping()
                    << ",\"swing_contact\":" << joint->getSwingLimitContactDistance() << ",\"twist_contact\":" << joint->getTwistLimitContactDistance()
                    << ",\"swing_enabled\":" << joint->getSwingLimitEnabled() << ",\"twist_enabled\":" << joint->getTwistLimitEnabled() << '}';
            }
            out << '}';
        }
        out << "]}";
    }
    out << "],\"rigid\":";
    const auto rigid_path = path + ".rigid.json";
    dump_rigid(collection, rigid_path);
    std::ifstream input(rigid_path); out << input.rdbuf() << "}\n";
    out.close();
    if (!out) throw std::runtime_error("Articulation getter output failed");
}

static void serialize(PxCollection& collection, PxSerializationRegistry& registry, const std::string& path) {
    PxDefaultFileOutputStream output(path.c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, collection, registry)) {
        throw std::runtime_error("Aggregate XML serialization failed");
    }
}

static void snapshot(FeatureContext& context, const std::string& directory, unsigned frame, bool drive_probe = false) {
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
    std::vector<PxArticulation*> articulations(context.scene->getNbArticulations());
    context.scene->getArticulations(articulations.data(), PxU32(articulations.size()));
    for (auto* articulation : articulations) resources.collection->add(*articulation);
    PxSerialization::complete(*resources.collection, *resources.registry, nullptr, true);
    for (PxU32 i = 0; i < resources.collection->getNbObjects(); ++i) {
        auto& object = resources.collection->getObject(i);
        resources.collection->addId(object, reinterpret_cast<std::uintptr_t>(&object));
    }
    const auto prefix = directory + "/frame-" + std::to_string(frame);
    dump_articulations(*resources.collection, prefix + "-truth.json", drive_probe);
    serialize(*resources.collection, *resources.registry, prefix + "-reference.xml");
}

struct DriveErrors : PxErrorCallback {
    std::vector<std::pair<PxErrorCode::Enum, std::string>> records;
    void reportError(PxErrorCode::Enum code, const char* message, const char*, int) override {
        records.emplace_back(code, message ? message : "");
    }
};

static void import_collection(const char* path, const char* truth, const char* roundtrip, bool drive_probe = false) {
    DriveErrors errors;
    {
        RepxImportEnvironment sdk;
        PxErrorCallback& callback = drive_probe ? static_cast<PxErrorCallback&>(errors) : static_cast<PxErrorCallback&>(sdk.errors);
        sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, callback), "foundation");
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
        dump_articulations(*resources.collection, truth, drive_probe);
        serialize(*resources.collection, *resources.registry, roundtrip);
    }
    if (drive_probe) {
        std::ofstream diagnostics(std::string(truth) + ".diagnostics.json");
        diagnostics << '[';
        for (std::size_t index = 0; index < errors.records.size(); ++index) {
            if (index) diagnostics << ',';
            diagnostics << "{\"code\":" << errors.records[index].first << ",\"message\":" << std::quoted(errors.records[index].second) << '}';
        }
        diagnostics << "]\n"; diagnostics.close();
        if (!diagnostics) throw std::runtime_error("Cannot write drive diagnostics");
    }
}

static PxArticulation* tree(FeatureContext& context, const char* name, float x) {
    auto* art = require(context.physics->createArticulation(), "articulation");
    art->setName(name); art->setMaxProjectionIterations(7); art->setSeparationTolerance(0.03f);
    art->setInternalDriveIterations(5); art->setExternalDriveIterations(6); art->setSolverIterationCounts(8,3);
    art->setSleepThreshold(0.02f); art->setStabilizationThreshold(0.03f);
    PxArticulationLink* links[4]{};
    const char* names[] = {"root", "left", "right", "leaf"};
    for (unsigned i = 0; i != 4; ++i) {
        auto* parent = i ? links[i == 3 ? 1 : 0] : nullptr;
        auto* link = require(art->createLink(parent, PxTransform(PxVec3(x + float(i), 4, 0))), "link");
        links[i] = link; link->setName(names[i]); link->setActorFlag(PxActorFlag::eDISABLE_GRAVITY, true);
        require(PxRigidActorExt::createExclusiveShape(*link, PxBoxGeometry(0.2f, 0.2f, 0.2f), *context.material), "shape");
        if (!PxRigidBodyExt::updateMassAndInertia(*link, 2.0f)) throw std::runtime_error("Link mass update failed");
        link->setMinCCDAdvanceCoefficient(0.17f); link->setMaxDepenetrationVelocity(12); link->setMaxContactImpulse(20);
        const PxRigidBodyFlags flags[] = {
            PxRigidBodyFlag::eENABLE_CCD | PxRigidBodyFlag::eENABLE_CCD_FRICTION | PxRigidBodyFlag::eENABLE_CCD_MAX_CONTACT_IMPULSE,
            PxRigidBodyFlag::eENABLE_SPECULATIVE_CCD | PxRigidBodyFlag::eENABLE_POSE_INTEGRATION_PREVIEW,
            PxRigidBodyFlags(),
            PxRigidBodyFlag::eENABLE_CCD_FRICTION | PxRigidBodyFlag::eENABLE_POSE_INTEGRATION_PREVIEW,
        };
        link->setRigidBodyFlags(flags[i]);
        if (auto* joint = link->getInboundJoint()) {
            joint->setParentPose(parent->getGlobalPose().transformInv(link->getGlobalPose()));
            joint->setChildPose(PxTransform(PxIdentity)); joint->setTargetOrientation(PxQuat(PxIdentity));
            joint->setStiffness(0); joint->setDamping(2); joint->setInternalCompliance(0.4f); joint->setExternalCompliance(0.6f);
            joint->setSwingLimit(0.5f, 0.6f); joint->setTwistLimit(-0.4f,0.3f);
            joint->setSwingLimitContactDistance(0.02f); joint->setTwistLimitContactDistance(0.03f);
            joint->setTangentialStiffness(0.7f); joint->setTangentialDamping(0.8f);
            joint->setSwingLimitEnabled(true); joint->setTwistLimitEnabled(true);
        }
    }
    return art;
}

static void produce(const char* capture, const std::string& directory) {
    FeatureContext context(capture); context.scene->setGravity(PxVec3(0));
    auto* group = require(context.physics->createAggregate(16, false), "aggregate");
    auto* rigid = require(context.physics->createRigidStatic(PxTransform(PxVec3(-3,0,0))), "rigid"); rigid->setName("group-static");
    require(PxRigidActorExt::createExclusiveShape(*rigid, PxBoxGeometry(0.2f, 0.2f, 0.2f), *context.material), "shape");
    auto* dynamic = require(context.physics->createRigidDynamic(PxTransform(PxVec3(-5, 0, 0))), "dynamic");
    dynamic->setName("separate-dynamic");
    require(PxRigidActorExt::createExclusiveShape(*dynamic, PxBoxGeometry(0.2f, 0.2f, 0.2f), *context.material), "shape");
    context.scene->addActor(*dynamic);
    auto* a = tree(context, "group-tree", 0); auto* b = tree(context, "separate-tree", 10);
    if (!group->addActor(*rigid) || !group->addArticulation(*a)) throw std::runtime_error("Group insertion failed");
    context.scene->addAggregate(*group); context.scene->addArticulation(*b);
    auto connect = [&]() {
        PxArticulationLink *al[4], *bl[4]; a->getLinks(al,4); b->getLinks(bl,4);
        return require(PxFixedJointCreate(*context.physics, al[2], al[2]->getGlobalPose().transformInv(bl[0]->getGlobalPose()), bl[0], PxTransform(PxIdentity)), "articulation bridge");
    };
    auto* bridge = connect();
    for (unsigned frame = 0; frame != 4; ++frame) {
        if (frame == 1) {
            bridge->release(); bridge = nullptr;
            a->setMaxProjectionIterations(9); a->setSeparationTolerance(0.07f); a->setSolverIterationCounts(11,4);
            PxArticulationLink* links[4]; a->getLinks(links,4); links[1]->setMass(0.8f);
            links[0]->setRigidBodyFlags(PxRigidBodyFlags());
            links[2]->setRigidBodyFlags(PxRigidBodyFlags(PxRigidBodyFlag::eENABLE_CCD));
            auto* joint = links[1]->getInboundJoint(); joint->setStiffness(1.1f); joint->setDamping(3.2f);
            joint->setSwingLimitEnabled(false); joint->setTwistLimitEnabled(false);
            if (!group->removeArticulation(*a)) throw std::runtime_error("Group removal failed");
        } else if (frame == 2) {
            context.scene->removeArticulation(*a);
            if (!group->addArticulation(*a)) throw std::runtime_error("Group reattachment failed");
            b->release(); b = nullptr;
        } else if (frame == 3) {
            b = tree(context, "recreated-tree", 20); context.scene->addArticulation(*b); bridge = connect();
        }
        for (auto* art : {a, b}) {
            if (!art) continue;
            PxArticulationLink* links[4]; art->getLinks(links, 4);
            for (PxU8 index = 0; index < 4; ++index)
                links[index]->setClientBehaviorFlags(PxActorClientBehaviorFlags(PxU8(frame * 4 + index)));
        }
        rigid->setClientBehaviorFlags(PxActorClientBehaviorFlags(PxU8(frame == 0 ? 15 : 0)));
        const PxU8 dynamic_flags[] = {8, 3, 0, 15};
        dynamic->setClientBehaviorFlags(PxActorClientBehaviorFlags(dynamic_flags[frame]));
        a->setWakeCounter(0.5f); if (b) b->setWakeCounter(0.5f);
        context.step(); snapshot(context, directory, frame);
    }
    if (bridge) bridge->release();
    if (b) b->release(); a->release(); rigid->release(); dynamic->release(); group->release();
}

struct DriveCase {
    const char* name;
    PxArticulationJointDriveType::Enum mode;
    PxQuat target;
    bool exportable;
};

static void produce_error_drive(const char* capture, const std::string& directory) {
    FeatureContext context(capture); context.scene->setGravity(PxVec3(0));
    auto* art = require(context.physics->createArticulation(), "drive articulation");
    art->setName("error-drive-tree");
    auto* root = require(art->createLink(nullptr, PxTransform(PxIdentity)), "root link");
    auto* child = require(art->createLink(root, PxTransform(PxVec3(0, 1, 0))), "child link");
    root->setName("drive-root"); child->setName("drive-child");
    for (auto* link : {root, child}) {
        link->setMass(1); link->setMassSpaceInertiaTensor(PxVec3(1));
        link->setActorFlag(PxActorFlag::eDISABLE_GRAVITY, true);
        require(PxRigidActorExt::createExclusiveShape(*link, PxBoxGeometry(PxVec3(0.1f)), *context.material), "drive box");
    }
    auto* joint = require(child->getInboundJoint(), "drive joint");
    joint->setParentPose(PxTransform(PxVec3(0, 1, 0))); joint->setChildPose(PxTransform(PxIdentity));
    joint->setStiffness(0); joint->setDamping(0); joint->setTargetVelocity(PxVec3(0));
    joint->setSwingLimitEnabled(false); joint->setTwistLimitEnabled(false);
    context.scene->addArticulation(*art);
    using Mode = PxArticulationJointDriveType;
    const DriveCase cases[] = {
        {"target-initial", Mode::eTARGET, PxQuat(PxIdentity), true},
        {"error-axis", Mode::eERROR, PxQuat(1, 0, 0, 0), true},
        {"error-oblique", Mode::eERROR, PxQuat(0.36f, 0.48f, 0.8f, 0), true},
        {"error-upper-unit", Mode::eERROR, PxQuat(1.00005f, 0, 0, 0), true},
        {"error-lower-unit", Mode::eERROR, PxQuat(0.99995f, 0, 0, 0), true},
        {"error-negative-zero", Mode::eERROR, PxQuat(-0.0f, 0, -1, -0.0f), true},
        {"error-margin-upper-in", Mode::eERROR, PxQuat(1.0000899f, 0, 0, 0), true},
        {"error-margin-upper-out", Mode::eERROR, PxQuat(1.0000901f, 0, 0, 0), false},
        {"error-margin-lower-in", Mode::eERROR, PxQuat(0.9999101f, 0, 0, 0), true},
        {"error-margin-lower-out", Mode::eERROR, PxQuat(0.9999099f, 0, 0, 0), false},
        {"error-sdk-upper-in", Mode::eERROR, PxQuat(1.0000998f, 0, 0, 0), false},
        {"error-sdk-upper-out", Mode::eERROR, PxQuat(1.0001002f, 0, 0, 0), false},
        {"error-sdk-lower-in", Mode::eERROR, PxQuat(0.9999002f, 0, 0, 0), false},
        {"error-sdk-lower-out", Mode::eERROR, PxQuat(0.9998998f, 0, 0, 0), false},
        {"error-zero", Mode::eERROR, PxQuat(0, 0, 0, 0), false},
        {"error-half", Mode::eERROR, PxQuat(0.5f, 0, 0, 0), false},
        {"error-double", Mode::eERROR, PxQuat(2, 0, 0, 0), false},
        {"target-restored", Mode::eTARGET, PxQuat(PxIdentity), true},
    };
    std::ofstream oracle(directory + "/drive-oracle.json");
    oracle << std::boolalpha << std::setprecision(9) << "{\"schema\":1,\"sdk_version\":[" << PX_PHYSICS_VERSION_MAJOR << ',' << PX_PHYSICS_VERSION_MINOR << ',' << PX_PHYSICS_VERSION_BUGFIX
        << "],\"actor\":" << reinterpret_cast<std::uintptr_t>(child) << ",\"joint\":" << reinterpret_cast<std::uintptr_t>(joint)
        << ",\"scene\":" << reinterpret_cast<std::uintptr_t>(context.scene) << ",\"frames\":[";
    unsigned frame = 0;
    for (const auto& item : cases) {
        joint->setDriveType(item.mode); joint->setTargetOrientation(item.target);
        const auto before = joint->getTargetOrientation();
        context.flush_and_require_connected();
        const auto prefix = [&] { std::lock_guard<PxPvdTransport> lock(*context.transport); return context.transport->getWrittenDataSize(); }();
        context.step();
        context.flush_and_require_connected();
        const auto end = [&] { std::lock_guard<PxPvdTransport> lock(*context.transport); return context.transport->getWrittenDataSize(); }();
        snapshot(context, directory, frame, true);
        if (frame) oracle << ',';
        oracle << "{\"name\":" << std::quoted(item.name) << ",\"drive\":" << PxU32(joint->getDriveType())
            << ",\"target\":"; quat(oracle, before);
        oracle << ",\"target_bits\":"; target_bits(oracle, before);
        oracle << ",\"poststep_target_bits\":"; target_bits(oracle, joint->getTargetOrientation());
        oracle << ",\"sdk_is_unit\":" << before.isUnit() << ",\"exportable\":" << item.exportable
            << ",\"prestep_bytes\":" << prefix << ",\"poststep_bytes\":" << end << '}';
        ++frame;
    }
    oracle << "]}\n"; oracle.close();
    if (!oracle) throw std::runtime_error("Cannot write drive oracle");
    art->release();
}

int main(int argc, char** argv) {
    try {
        if (argc == 5 && std::string(argv[1]) == "--import") import_collection(argv[2], argv[3], argv[4]);
        else if (argc == 5 && std::string(argv[1]) == "--import-drive") import_collection(argv[2], argv[3], argv[4], true);
        else if (argc == 4 && std::string(argv[1]) == "--error-drive") produce_error_drive(argv[2], argv[3]);
        else if (argc == 3) produce(argv[1], argv[2]);
        else throw std::runtime_error("Usage: pvd-repx-articulations capture.pvdstream output-directory | --error-drive capture.pvdstream output-directory | --import input.xml truth.json roundtrip.xml | --import-drive input.xml truth.json roundtrip.xml");
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
