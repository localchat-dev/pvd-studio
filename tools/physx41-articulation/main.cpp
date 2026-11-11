#include "feature_harness.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <memory>
#include <vector>

static std::uintptr_t id(const void* value) { return reinterpret_cast<std::uintptr_t>(value); }

static void vector_json(std::ostream& out, const PxVec3& value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void pose_json(std::ostream& out, const PxTransform& value) {
    out << "{\"position\":";
    vector_json(out, value.p);
    out << ",\"rotation\":[" << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << "]}";
}

struct Options {
    FeatureConnection connection;
    const char* truth;
};

static Options options(int argc, char** argv) {
    if (argc == 3) return { FeatureConnection::file(argv[1]), argv[2] };
    if (argc == 6 && std::string(argv[1]) == "--host" && std::string(argv[3]) == "--port") {
        const std::string value(argv[4]);
        if (value.empty() || value.find_first_not_of("0123456789") != std::string::npos) {
            throw std::runtime_error("Port must be an integer between 1 and 65535");
        }
        const auto port = std::stoul(value);
        if (!port || port > 65535) throw std::runtime_error("Port must be between 1 and 65535");
        return { FeatureConnection::socket(argv[2], static_cast<unsigned>(port)), argv[5] };
    }
    throw std::runtime_error("Usage: pvd-physx41-articulation capture.pvdstream truth.articulation.json | --host ADDRESS --port PORT truth.articulation.json");
}

static std::vector<PxArticulationLink*> links(PxArticulationBase& articulation) {
    std::vector<PxArticulationLink*> result(articulation.getNbLinks());
    if (articulation.getLinks(result.data(), static_cast<PxU32>(result.size())) != result.size()) {
        throw std::runtime_error("Incomplete SDK link enumeration");
    }
    return result;
}

static std::vector<PxArticulationLink*> children(PxArticulationLink& link) {
    std::vector<PxArticulationLink*> result(link.getNbChildren());
    if (link.getChildren(result.data(), static_cast<PxU32>(result.size())) != result.size()) {
        throw std::runtime_error("Incomplete SDK child enumeration");
    }
    return result;
}

static std::uintptr_t parent_id(PxArticulationBase& articulation,
    PxArticulationLink& link, const std::vector<PxArticulationLink*>& all) {
    std::uintptr_t parent = 0;
    for (auto* candidate : all) {
        for (auto* child : children(*candidate)) {
            if (child == &link) {
                if (parent) throw std::runtime_error("SDK link has more than one parent");
                parent = id(candidate);
            }
        }
    }
    if (link.getInboundJoint()) {
        if (!parent) throw std::runtime_error("SDK child has no parent");
        return parent;
    }
    if (parent) throw std::runtime_error("SDK root has a link parent");
    return id(&articulation);
}

static PxArticulationLink* add_link(FeatureContext& context,
    PxArticulationReducedCoordinate& articulation, PxArticulationLink* parent,
    const PxTransform& transform, const PxGeometry& geometry, const char* name) {
    auto* link = feature_require(articulation.createLink(parent, transform), "RC link");
    link->setName(name);
    auto* shape = feature_require(
        PxRigidActorExt::createExclusiveShape(*link, geometry, *context.material), "RC shape");
    shape->setLocalPose(PxTransform(PxVec3(0.07f, 0.11f, -0.05f), PxQuat(0.19f, PxVec3(0, 0, 1))));
    if (!PxRigidBodyExt::updateMassAndInertia(*link, 2.0f)) {
        throw std::runtime_error("RC link mass update failed");
    }
    return link;
}

static void shape_json(std::ostream& out, PxShape& shape, PxArticulationLink& link) {
    out << "{\"id\":" << id(&shape) << ",\"local_pose\":";
    pose_json(out, shape.getLocalPose());
    out << ",\"world_pose\":";
    pose_json(out, PxShapeExt::getGlobalPose(shape, link));
    out << ",\"geometry_type\":" << shape.getGeometryType() << ",\"dimensions\":";
    PxBoxGeometry box;
    PxSphereGeometry sphere;
    PxCapsuleGeometry capsule;
    if (shape.getBoxGeometry(box)) vector_json(out, box.halfExtents);
    else if (shape.getSphereGeometry(sphere)) out << '[' << sphere.radius << ']';
    else if (shape.getCapsuleGeometry(capsule)) out << '[' << capsule.radius << ',' << capsule.halfHeight << ']';
    else throw std::runtime_error("Unexpected RC fixture shape");
    out << '}';
}

static void link_json(std::ostream& out, PxArticulationBase& articulation,
    PxArticulationLink& link, const std::vector<PxArticulationLink*>& all) {
    out << "{\"id\":" << id(&link) << ",\"name\":\"" << link.getName()
        << "\",\"parent\":" << parent_id(articulation, link, all)
        << ",\"articulation\":" << id(&link.getArticulation()) << ",\"pose\":";
    pose_json(out, link.getGlobalPose());
    out << ",\"linear_velocity\":";
    vector_json(out, link.getLinearVelocity());
    out << ",\"angular_velocity\":";
    vector_json(out, link.getAngularVelocity());
    out << ",\"children\":[";
    const auto descendants = children(link);
    for (std::size_t i = 0; i < descendants.size(); ++i) {
        if (i) out << ',';
        out << id(descendants[i]);
    }
    out << "],\"joint\":";
    if (auto* base = link.getInboundJoint()) {
        auto* joint = static_cast<PxArticulationJointReducedCoordinate*>(base);
        out << "{\"id\":" << id(base) << ",\"concrete_type\":\"" << joint->getConcreteTypeName()
            << "\",\"joint_type\":" << joint->getJointType()
            << ",\"twist_motion\":" << joint->getMotion(PxArticulationAxis::eTWIST)
            << ",\"parent_pose\":";
        pose_json(out, joint->getParentPose());
        out << ",\"child_pose\":";
        pose_json(out, joint->getChildPose());
        out << '}';
    } else {
        out << "null";
    }
    out << ",\"shapes\":[";
    std::vector<PxShape*> shapes(link.getNbShapes());
    if (link.getShapes(shapes.data(), static_cast<PxU32>(shapes.size())) != shapes.size()) {
        throw std::runtime_error("Incomplete SDK shape enumeration");
    }
    for (std::size_t i = 0; i < shapes.size(); ++i) {
        if (i) out << ',';
        shape_json(out, *shapes[i], link);
    }
    out << "]}";
}

static void articulation_json(std::ostream& out, PxArticulationReducedCoordinate& articulation) {
    PxU32 position_iterations = 0, velocity_iterations = 0;
    articulation.getSolverIterationCounts(position_iterations, velocity_iterations);
    out << "{\"id\":" << id(static_cast<PxArticulationBase*>(&articulation))
        << ",\"concrete_type\":\"" << articulation.getConcreteTypeName()
        << "\",\"name\":\"" << articulation.getName()
        << "\",\"sleeping\":" << articulation.isSleeping()
        << ",\"flags\":" << static_cast<unsigned>(static_cast<PxU8>(articulation.getArticulationFlags()))
        << ",\"dofs\":" << articulation.getDofs()
        << ",\"sleep_threshold\":" << articulation.getSleepThreshold()
        << ",\"solver_iterations\":[" << position_iterations << ',' << velocity_iterations << "],\"links\":[";
    const auto all = links(articulation);
    for (std::size_t i = 0; i < all.size(); ++i) {
        if (i) out << ',';
        link_json(out, articulation, *all[i], all);
    }
    out << "]}";
}

struct ReleaseArticulation {
    void operator()(PxArticulationReducedCoordinate* articulation) const { articulation->release(); }
};

int main(int argc, char** argv) {
    try {
        const auto configuration = options(argc, argv);
        std::ofstream truth(configuration.truth, std::ios::trunc);
        if (!truth) throw std::runtime_error("Cannot open articulation truth output");
        FeatureContext context(configuration.connection);
        if (feature_sdk_version() != "4.1.2") throw std::runtime_error("This fixture requires PhysX 4.1.2");
        context.scene->setGravity(PxVec3(0));
        std::unique_ptr<PxArticulationReducedCoordinate, ReleaseArticulation> articulation(
            feature_require(context.physics->createArticulationReducedCoordinate(), "RC articulation"));
        articulation->setName("Reduced-coordinate chain");
        articulation->setArticulationFlag(PxArticulationFlag::eFIX_BASE, true);
        articulation->setSolverIterationCounts(12, 2);
        articulation->setSleepThreshold(0.005f);
        auto* root = add_link(context, *articulation, nullptr, PxTransform(PxVec3(1, 3, -0.25f)),
            PxBoxGeometry(0.25f, 0.2f, 0.3f), "RC root");
        auto* middle = add_link(context, *articulation, root, PxTransform(PxVec3(1, 4, -0.25f)),
            PxSphereGeometry(0.22f), "RC middle");
        auto* tip = add_link(context, *articulation, middle, PxTransform(PxVec3(1, 5, -0.25f)),
            PxCapsuleGeometry(0.16f, 0.3f), "RC tip");
        auto* revolute = static_cast<PxArticulationJointReducedCoordinate*>(
            feature_require(middle->getInboundJoint(), "RC revolute joint"));
        revolute->setJointType(PxArticulationJointType::eREVOLUTE);
        revolute->setMotion(PxArticulationAxis::eTWIST, PxArticulationMotion::eLIMITED);
        revolute->setLimit(PxArticulationAxis::eTWIST, -0.7f, 0.7f);
        revolute->setParentPose(PxTransform(PxVec3(0, 0.5f, 0)));
        revolute->setChildPose(PxTransform(PxVec3(0, -0.5f, 0)));
        auto* fixed = static_cast<PxArticulationJointReducedCoordinate*>(
            feature_require(tip->getInboundJoint(), "RC fixed joint"));
        fixed->setJointType(PxArticulationJointType::eFIX);
        fixed->setParentPose(PxTransform(PxVec3(0, 0.5f, 0)));
        fixed->setChildPose(PxTransform(PxVec3(0, -0.5f, 0)));
        articulation->setWakeCounter(0.0f);
        context.scene->addArticulation(*articulation);

        const bool sleep_at_frame[] = { true, true, false, false, false, true, true, false, false, true };
        truth << std::setprecision(9) << std::boolalpha << "{\"sdk\":\"" << feature_sdk_version()
            << "\",\"scene\":" << id(context.scene) << ",\"destroy_frame\":10,\"initial\":";
        articulation_json(truth, *articulation);
        truth << ",\"frames\":[";
        for (unsigned frame = 0; frame < 12; ++frame) {
            if (frame == 2 || frame == 7) {
                articulation->wakeUp();
                middle->addTorque(PxVec3(frame == 2 ? 4.0f : -3.0f, 0, 0), PxForceMode::eFORCE);
            }
            if (frame == 5 || frame == 9) articulation->putToSleep();
            if (frame == 10) articulation.reset();
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"scene_articulations\":[";
            std::vector<PxArticulationBase*> members(context.scene->getNbArticulations());
            if (context.scene->getArticulations(members.data(), static_cast<PxU32>(members.size())) != members.size()) {
                throw std::runtime_error("Incomplete SDK scene articulation enumeration");
            }
            for (std::size_t i = 0; i < members.size(); ++i) {
                if (i) truth << ',';
                truth << id(members[i]);
            }
            truth << "],\"before_simulate\":";
            if (articulation) articulation_json(truth, *articulation);
            else truth << "null";
            context.step();
            if (articulation && articulation->isSleeping() != sleep_at_frame[frame]) {
                throw std::runtime_error("SDK articulation did not retain the scheduled sleep state");
            }
            truth << ",\"after_fetch\":";
            if (articulation) articulation_json(truth, *articulation);
            else truth << "null";
            truth << '}';
        }
        truth << "]}\n";
        truth.flush();
        if (!truth) throw std::runtime_error("Articulation truth write failed");
        context.flush_and_require_connected();
        std::cout << "PhysX " << feature_sdk_version()
            << ": 12 frames, 3 RC links, revolute/fixed joints, sleep/wake/destroy getter truth\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
