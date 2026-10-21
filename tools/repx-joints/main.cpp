#include "../physx-features/feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>
template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"

static void text(std::ostream& out, const char* value) {
    out << '"';
    if (value) for (const unsigned char* p = reinterpret_cast<const unsigned char*>(value); *p; ++p) {
        if (*p == '"' || *p == '\\') out << '\\' << *p;
        else if (*p < 32) out << "\\u00" << std::hex << std::setw(2) << std::setfill('0') << unsigned(*p) << std::dec;
        else out << *p;
    }
    out << '"';
}

static void pose(std::ostream& out, const PxTransform& value) {
    out << "{\"p\":[" << value.p.x << ',' << value.p.y << ',' << value.p.z
        << "],\"q\":[" << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << "]}";
}

static PxSerialObjectId actor_id(PxCollection& collection, PxRigidActor* actor) {
    if (!actor) return 0;
    const auto id = collection.getId(*actor);
    if (!id) throw std::runtime_error("Joint actor is missing a collection ID");
    return id;
}

static void vector(std::ostream& out, const PxVec3& value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void limit_parameters(std::ostream& out, const PxJointLimitParameters& limit) {
    out << "\"restitution\":" << limit.restitution << ",\"bounce_threshold\":" << limit.bounceThreshold
        << ",\"stiffness\":" << limit.stiffness << ",\"damping\":" << limit.damping
        << ",\"contact_distance\":" << limit.contactDistance;
}

template<typename T> static void pair_limit(std::ostream& out, const T& limit) {
    out << '{'; limit_parameters(out, limit);
    out << ",\"lower\":" << limit.lower << ",\"upper\":" << limit.upper << '}';
}

static void cone_limit(std::ostream& out, const PxJointLimitCone& limit) {
    out << '{'; limit_parameters(out, limit);
    out << ",\"y_angle\":" << limit.yAngle << ",\"z_angle\":" << limit.zAngle << '}';
}

template<typename T> static void projection(std::ostream& out, const T& joint) {
    out << ",\"projection_linear_tolerance\":" << joint.getProjectionLinearTolerance()
        << ",\"projection_angular_tolerance\":" << joint.getProjectionAngularTolerance();
}

static void joint_properties(std::ostream& out, PxJoint& joint) {
    if (auto* value = joint.is<PxSphericalJoint>()) {
        out << ",\"limit\":"; cone_limit(out, value->getLimitCone());
        out << ",\"spherical_flags\":" << unsigned(value->getSphericalJointFlags())
            << ",\"projection_linear_tolerance\":" << value->getProjectionLinearTolerance();
    } else if (auto* value = joint.is<PxFixedJoint>()) {
        projection(out, *value);
    } else if (auto* value = joint.is<PxDistanceJoint>()) {
        out << ",\"min_distance\":" << value->getMinDistance() << ",\"max_distance\":" << value->getMaxDistance()
            << ",\"tolerance\":" << value->getTolerance() << ",\"stiffness\":" << value->getStiffness()
            << ",\"damping\":" << value->getDamping() << ",\"distance_flags\":" << unsigned(value->getDistanceJointFlags());
    } else if (auto* value = joint.is<PxRevoluteJoint>()) {
        projection(out, *value);
        out << ",\"limit\":"; pair_limit(out, value->getLimit());
        out << ",\"drive_velocity\":" << value->getDriveVelocity() << ",\"drive_force_limit\":" << value->getDriveForceLimit()
            << ",\"drive_gear_ratio\":" << value->getDriveGearRatio() << ",\"revolute_flags\":" << unsigned(value->getRevoluteJointFlags());
    } else if (auto* value = joint.is<PxPrismaticJoint>()) {
        projection(out, *value);
        out << ",\"limit\":"; pair_limit(out, value->getLimit());
        out << ",\"prismatic_flags\":" << unsigned(value->getPrismaticJointFlags());
    } else if (auto* value = joint.is<PxD6Joint>()) {
        projection(out, *value);
        out << ",\"motion\":[";
        for (PxU32 axis = 0; axis < PxD6Axis::eCOUNT; ++axis) {
            if (axis) out << ',';
            out << unsigned(value->getMotion(PxD6Axis::Enum(axis)));
        }
        out << "],\"linear_limit\":{"; const auto linear = value->getLinearLimit();
        limit_parameters(out, linear); out << ",\"value\":" << linear.value << '}';
        out << ",\"twist_limit\":"; pair_limit(out, value->getTwistLimit());
        out << ",\"swing_limit\":"; cone_limit(out, value->getSwingLimit());
        out << ",\"drives\":[";
        for (PxU32 index = 0; index < PxD6Drive::eCOUNT; ++index) {
            if (index) out << ',';
            const auto drive = value->getDrive(PxD6Drive::Enum(index));
            out << "{\"stiffness\":" << drive.stiffness << ",\"damping\":" << drive.damping
                << ",\"force_limit\":" << drive.forceLimit << ",\"flags\":" << unsigned(drive.flags) << '}';
        }
        out << "],\"drive_position\":"; pose(out, value->getDrivePosition());
        PxVec3 linear_velocity, angular_velocity; value->getDriveVelocity(linear_velocity, angular_velocity);
        out << ",\"drive_velocity\":["; vector(out, linear_velocity); out << ','; vector(out, angular_velocity); out << ']';
    } else throw std::runtime_error("Getter probe encountered an unsupported joint type");
}

static void dump_collection(PxCollection& collection, const char* path) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot open joint getter output");
    out << std::setprecision(9) << "{\"actors\":[";
    bool first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* actor = collection.getObject(i).is<PxRigidActor>();
        if (!actor) continue;
        if (!first) out << ',';
        first = false;
        out << "{\"id\":" << collection.getId(*actor) << ",\"name\":";
        text(out, actor->getName());
        out << ",\"type\":";
        text(out, actor->getConcreteTypeName());
        out << ",\"pose\":";
        pose(out, actor->getGlobalPose());
        out << '}';
    }
    out << "],\"joints\":[";
    first = true;
    for (PxU32 i = 0; i < collection.getNbObjects(); ++i) {
        auto* joint = collection.getObject(i).is<PxJoint>();
        if (!joint) continue;
        if (!first) out << ',';
        first = false;
        PxRigidActor* actor0 = nullptr;
        PxRigidActor* actor1 = nullptr;
        joint->getActors(actor0, actor1);
        PxRigidActor* constraint0 = nullptr;
        PxRigidActor* constraint1 = nullptr;
        require(joint->getConstraint(), "joint constraint")->getActors(constraint0, constraint1);
        PxReal force, torque;
        joint->getBreakForce(force, torque);
        out << "{\"id\":" << collection.getId(*joint) << ",\"name\":";
        text(out, joint->getName());
        out << ",\"type\":"; text(out, joint->getConcreteTypeName());
        out << ",\"actors\":[" << actor_id(collection, actor0) << ',' << actor_id(collection, actor1)
            << "],\"constraint_actors\":[" << actor_id(collection, constraint0) << ',' << actor_id(collection, constraint1)
            << "],\"local_pose\":[";
        pose(out, joint->getLocalPose(PxJointActorIndex::eACTOR0));
        out << ',';
        pose(out, joint->getLocalPose(PxJointActorIndex::eACTOR1));
        out << "],\"break_force\":[" << force << ',' << torque
            << "],\"constraint_flags\":" << unsigned(joint->getConstraintFlags())
            << ",\"inv_mass_scale\":[" << joint->getInvMassScale0() << ',' << joint->getInvMassScale1()
            << "],\"inv_inertia_scale\":[" << joint->getInvInertiaScale0() << ',' << joint->getInvInertiaScale1()
            << ']';
        joint_properties(out, *joint);
        out << '}';
    }
    out << "]}\n";
    out.close();
    if (!out) throw std::runtime_error("Joint getter output failed");
}

static void import_collection(const char* input, const char* truth, const char* roundtrip) {
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION, sdk.allocator, sdk.errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION, *sdk.foundation, PxTolerancesScale()), "physics");
    sdk.extensions = PxInitExtensions(*sdk.physics, nullptr);
    if (!sdk.extensions) throw std::runtime_error("Cannot initialize serialization extensions");
    sdk.cooking = require(PxCreateCooking(PX_PHYSICS_VERSION, *sdk.foundation, PxCookingParams(sdk.physics->getTolerancesScale())), "cooking");
    sdk.strings = &PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*sdk.physics), "registry");
    PxDefaultFileInputData stream(input);
    if (!stream.isValid()) throw std::runtime_error("Cannot open RepX input");
    PxSerialization::PxXmlMiscParameter parameters;
    resources.collection = require(PxSerialization::createCollectionFromXml(stream, *sdk.cooking, *resources.registry, nullptr, sdk.strings, &parameters), "RepX joint collection");
    resources.own_objects = true;
    dump_collection(*resources.collection, truth);
    PxDefaultFileOutputStream output(roundtrip);
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output, *resources.collection, *resources.registry, nullptr, nullptr, &parameters)) {
        throw std::runtime_error("RepX joint roundtrip failed");
    }
}

static void snapshot(FeatureContext& context, const std::string& output, int frame) {
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*context.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    const auto types = PxActorTypeFlag::eRIGID_STATIC | PxActorTypeFlag::eRIGID_DYNAMIC;
    std::vector<PxActor*> actors(context.scene->getNbActors(types));
    context.scene->getActors(types, actors.data(), static_cast<PxU32>(actors.size()));
    for (auto* actor : actors) resources.collection->add(*actor);
    PxSerialization::complete(*resources.collection, *resources.registry, nullptr, true);
    for (PxU32 i = 0; i < resources.collection->getNbObjects(); ++i) {
        auto& object = resources.collection->getObject(i);
        resources.collection->addId(object, reinterpret_cast<std::uintptr_t>(&object));
    }
    const auto prefix = output + "/frame-" + std::to_string(frame);
    dump_collection(*resources.collection, (prefix + "-truth.json").c_str());
    PxDefaultFileOutputStream reference((prefix + "-reference.xml").c_str());
    if (!reference.isValid() || !PxSerialization::serializeCollectionToXml(reference, *resources.collection, *resources.registry)) {
        throw std::runtime_error("Official joint reference XML failed");
    }
}

struct JointResources {
    std::vector<PxJoint*> joints;
    ~JointResources() { for (auto* joint : joints) if (joint) joint->release(); }
};

static void configure_extra(FeatureContext& context, JointResources& resources, int revision) {
    for (std::size_t i = 0; i < resources.joints.size(); ++i) {
        auto* joint = resources.joints[i];
        const float change = float(revision);
        joint->setBreakForce(7.0e12f + float(i) * 1.0e9f, 8.0e12f + change * 1.0e10f);
        joint->setConstraintFlags(PxConstraintFlag::eVISUALIZATION | PxConstraintFlag::eCOLLISION_ENABLED
            | (revision ? PxConstraintFlag::eDISABLE_PREPROCESSING : PxConstraintFlag::ePROJECT_TO_ACTOR1));
        joint->setInvMassScale0(0.7f + change * 0.1f);
        joint->setInvMassScale1(1.3f + change * 0.2f);
        joint->setInvInertiaScale0(0.8f + float(i) * 0.1f);
        joint->setInvInertiaScale1(1.6f + change * 0.3f);
        joint->setLocalPose(PxJointActorIndex::eACTOR1,
            PxTransform(PxVec3(-1.7f + change * 0.1f, 0.1f, -0.2f), PxQuat(0.2f + change * 0.1f, PxVec3(0, 0, 1))));
        if (auto* value = joint->is<PxFixedJoint>()) {
            value->setProjectionLinearTolerance(0.15f + change * 0.05f);
            value->setProjectionAngularTolerance(0.17f + change * 0.07f);
        } else if (auto* value = joint->is<PxDistanceJoint>()) {
            value->setMinDistance(0.5f + change * 0.1f); value->setMaxDistance(4.5f + change * 0.2f);
            value->setTolerance(0.08f + change * 0.01f); value->setStiffness(1.25f + change); value->setDamping(0.4f + change * 0.2f);
            value->setDistanceJointFlags(PxDistanceJointFlag::eMAX_DISTANCE_ENABLED | PxDistanceJointFlag::eSPRING_ENABLED
                | (revision ? PxDistanceJointFlags() : PxDistanceJointFlag::eMIN_DISTANCE_ENABLED));
        } else if (auto* value = joint->is<PxRevoluteJoint>()) {
            PxJointAngularLimitPair limit(-0.9f - change * 0.1f, 1.2f + change * 0.1f, 0.07f);
            limit.restitution = 0.25f; limit.bounceThreshold = 0.35f; limit.stiffness = 2.0f + change; limit.damping = 0.4f;
            value->setLimit(limit); value->setDriveVelocity(-0.3f + change * 0.7f);
            value->setDriveForceLimit(1.8f + change); value->setDriveGearRatio(1.2f + change * 0.3f);
            value->setRevoluteJointFlags(PxRevoluteJointFlag::eLIMIT_ENABLED | PxRevoluteJointFlag::eDRIVE_ENABLED
                | (revision ? PxRevoluteJointFlag::eDRIVE_FREESPIN : PxRevoluteJointFlags()));
            value->setProjectionLinearTolerance(0.16f + change * 0.03f); value->setProjectionAngularTolerance(0.21f + change * 0.02f);
        } else if (auto* value = joint->is<PxPrismaticJoint>()) {
            PxJointLinearLimitPair limit(context.physics->getTolerancesScale(), -2.0f - change * 0.1f, 3.0f + change * 0.2f, 0.06f);
            limit.restitution = 0.3f; limit.bounceThreshold = 0.45f; limit.stiffness = 1.5f + change; limit.damping = 0.6f;
            value->setLimit(limit); value->setPrismaticJointFlag(PxPrismaticJointFlag::eLIMIT_ENABLED, revision == 0);
            value->setProjectionLinearTolerance(0.12f + change * 0.04f); value->setProjectionAngularTolerance(0.23f + change * 0.03f);
        } else if (auto* value = joint->is<PxD6Joint>()) {
            for (PxU32 axis = 0; axis < PxD6Axis::eCOUNT; ++axis)
                value->setMotion(PxD6Axis::Enum(axis), PxD6Motion::Enum((axis + revision) % 3));
            PxJointLinearLimit linear(context.physics->getTolerancesScale(), 2.5f + change, 0.04f);
            linear.restitution = 0.15f; linear.bounceThreshold = 0.8f; linear.stiffness = 2.2f; linear.damping = 0.3f;
            value->setLinearLimit(linear);
            PxJointAngularLimitPair twist(revision ? 3.3f : -1.3f, revision ? 4.7f : 1.1f, 0.05f);
            twist.restitution = 0.2f; twist.bounceThreshold = 0.6f; twist.stiffness = 1.7f; twist.damping = 0.9f;
            value->setTwistLimit(twist);
            PxJointLimitCone swing(0.8f + change * 0.2f, 1.2f + change * 0.1f, 0.03f);
            swing.restitution = 0.35f; swing.bounceThreshold = 0.7f; swing.stiffness = 1.9f; swing.damping = 0.5f;
            value->setSwingLimit(swing);
            for (PxU32 index = 0; index < PxD6Drive::eCOUNT; ++index)
                value->setDrive(PxD6Drive::Enum(index), PxD6JointDrive(1.2f + index + change, 0.15f + index * 0.1f,
                    2.5f + index + change, (index + revision) % 2 == 0));
            value->setDrivePosition(PxTransform(PxVec3(0.1f + change * 0.1f, -0.2f, 0.3f), PxQuat(0.15f + change * 0.1f, PxVec3(1, 0, 0))));
            value->setDriveVelocity(PxVec3(0.2f + change, -0.3f, 0.4f), PxVec3(-0.1f, 0.2f + change * 0.1f, -0.3f));
            value->setProjectionLinearTolerance(0.17f + change * 0.02f); value->setProjectionAngularTolerance(0.19f + change * 0.05f);
        }
    }
}

static void produce(const char* capture, const std::string& output, bool expanded = false) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    PxRigidDynamic* bodies[3];
    const char* names[] = {"world-body", "chain-body", "anchor-body"};
    for (int i = 0; i < 3; ++i) {
        bodies[i] = require(PxCreateDynamic(*context.physics,
            PxTransform(PxVec3(float(i * 3), 2, 0), PxQuat(0.12f * (i + 1), PxVec3(0, 1, 0))),
            PxSphereGeometry(0.3f), *context.material, 2.0f), "dynamic actor");
        bodies[i]->setName(names[i]);
        context.scene->addActor(*bodies[i]);
    }
    auto* anchor = require(PxCreateStatic(*context.physics, PxTransform(PxVec3(6, 4, 0)), PxBoxGeometry(0.2f, 0.3f, 0.4f), *context.material), "static anchor");
    anchor->setName("static-anchor");
    context.scene->addActor(*anchor);
    JointResources resources;
    resources.joints.push_back(require(PxSphericalJointCreate(*context.physics, nullptr,
        PxTransform(PxVec3(0, 3, 0), PxQuat(0.25f, PxVec3(1, 0, 0))), bodies[0],
        PxTransform(PxVec3(0, 1, 0), PxQuat(-0.15f, PxVec3(0, 0, 1)))), "world joint"));
    resources.joints.push_back(require(PxSphericalJointCreate(*context.physics, bodies[0],
        PxTransform(PxVec3(1, 0, 0), PxQuat(0.35f, PxVec3(0, 1, 0))), bodies[1],
        PxTransform(PxVec3(-2, 0, 0), PxQuat(-0.2f, PxVec3(1, 0, 0)))), "body joint"));
    resources.joints.push_back(require(PxSphericalJointCreate(*context.physics, anchor,
        PxTransform(PxVec3(0, -1, 0), PxQuat(-0.3f, PxVec3(0, 0, 1))), bodies[2],
        PxTransform(PxVec3(0, 1, 0), PxQuat(0.18f, PxVec3(0, 1, 0)))), "static joint"));
    const char* joint_names[] = {"world-joint", "body-joint", "static-joint"};
    for (int i = 0; i < 3; ++i) {
        auto* joint = resources.joints[i]->is<PxSphericalJoint>();
        joint->setName(joint_names[i]);
        joint->setBreakForce(1.0e10f + float(i) * 1.0e9f, 2.0e10f + float(i) * 1.0e9f);
        joint->setConstraintFlags(PxConstraintFlag::eVISUALIZATION | PxConstraintFlag::eCOLLISION_ENABLED
            | PxConstraintFlag::eDRIVE_LIMITS_ARE_FORCES | (i == 1 ? PxConstraintFlag::eDISABLE_PREPROCESSING : PxConstraintFlag::ePROJECT_TO_ACTOR0));
        joint->setInvMassScale0(0.25f + i * 0.25f);
        joint->setInvMassScale1(1.2f + i * 0.2f);
        joint->setInvInertiaScale0(0.8f + i * 0.3f);
        joint->setInvInertiaScale1(1.5f + i * 0.1f);
        PxJointLimitCone limit(0.7f + 0.1f * i, 0.9f + 0.1f * i, 0.03f + 0.01f * i);
        limit.restitution = 0.2f + 0.1f * i;
        limit.bounceThreshold = 0.6f + 0.2f * i;
        limit.stiffness = 7.5f + i;
        limit.damping = 1.25f + i * 0.5f;
        joint->setLimitCone(limit);
        joint->setSphericalJointFlag(PxSphericalJointFlag::eLIMIT_ENABLED, i != 2);
        joint->setProjectionLinearTolerance(0.15f + i * 0.05f);
    }
    JointResources extra;
    if (expanded) {
        const PxTransform local0(PxVec3(1, 0.1f, 0.2f), PxQuat(0.12f, PxVec3(1, 0, 0)));
        const PxTransform local1(PxVec3(-1.5f, 0.2f, 0.1f), PxQuat(0.16f, PxVec3(0, 1, 0)));
        extra.joints.push_back(require(PxFixedJointCreate(*context.physics, nullptr, local0, bodies[0], local1), "fixed joint"));
        extra.joints.push_back(require(PxDistanceJointCreate(*context.physics, bodies[0], local0, bodies[1], local1), "distance joint"));
        extra.joints.push_back(require(PxRevoluteJointCreate(*context.physics, anchor, local0, bodies[2], local1), "revolute joint"));
        extra.joints.push_back(require(PxPrismaticJointCreate(*context.physics, bodies[1], local0, bodies[2], local1), "prismatic joint"));
        extra.joints.push_back(require(PxD6JointCreate(*context.physics, nullptr, local0, bodies[1], local1), "d6 joint"));
        const char* extra_names[] = {"fixed-joint", "distance-joint", "revolute-joint", "prismatic-joint", "d6-joint"};
        for (std::size_t i = 0; i < extra.joints.size(); ++i) extra.joints[i]->setName(extra_names[i]);
        configure_extra(context, extra, 0);
    }
    snapshot(context, output, 0);
    context.step();
    auto* changed = resources.joints[1]->is<PxSphericalJoint>();
    changed->setName("body-joint-updated");
    changed->setLocalPose(PxJointActorIndex::eACTOR1, PxTransform(PxVec3(-1.8f, 0.1f, -0.2f), PxQuat(0.4f, PxVec3(0, 0, 1))));
    changed->setConstraintFlag(PxConstraintFlag::eIMPROVED_SLERP, true);
    changed->setInvMassScale1(2.25f);
    changed->setInvInertiaScale0(0.45f);
    auto limit = changed->getLimitCone();
    limit.yAngle = 1.05f;
    limit.contactDistance = 0.08f;
    changed->setLimitCone(limit);
    changed->setSphericalJointFlag(PxSphericalJointFlag::eLIMIT_ENABLED, false);
    changed->setProjectionLinearTolerance(0.35f);
    configure_extra(context, extra, 1);
    snapshot(context, output, 1);
    context.step();
    resources.joints[2]->release();
    resources.joints[2] = nullptr;
    for (auto*& joint : extra.joints) { joint->release(); joint = nullptr; }
    snapshot(context, output, 2);
    context.step();
}

int main(int argc, char** argv) {
    try {
        if (argc == 5 && std::string(argv[1]) == "--import") import_collection(argv[2], argv[3], argv[4]);
        else if (argc == 3) produce(argv[1], argv[2]);
        else if (argc == 4 && std::string(argv[1]) == "--expanded") produce(argv[2], argv[3], true);
        else throw std::runtime_error("Usage: pvd-repx-joints [--expanded] capture.pvdstream output-directory | --import input.xml truth.json roundtrip.xml");
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
