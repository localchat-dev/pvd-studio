#include "../physx-features/feature_harness.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>

static std::uintptr_t id(const void* value) { return reinterpret_cast<std::uintptr_t>(value); }
static void vector(std::ostream& out, PxVec3 value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}
static void pose(std::ostream& out, PxTransform value) {
    out << "{\"position\":";
    vector(out, value.p);
    out << ",\"rotation\":[" << value.q.x << ',' << value.q.y << ',' << value.q.z << ',' << value.q.w << "]}";
}

static void write_actor(std::ostream& out, PxRigidActor& actor) {
    auto* link = actor.is<PxArticulationLink>();
    auto* dynamic = actor.is<PxRigidDynamic>();
    out << "{\"id\":" << id(&actor) << ",\"type\":\"" << actor.getConcreteTypeName()
        << "\",\"aggregate\":" << id(actor.getAggregate()) << ",\"pose\":";
    pose(out, actor.getGlobalPose());
    if (auto* body = actor.is<PxRigidBody>()) {
        out << ",\"linear_velocity\":";
        vector(out, body->getLinearVelocity());
        out << ",\"angular_velocity\":";
        vector(out, body->getAngularVelocity());
    }
    out << ",\"sleeping\":" << (link ? link->getArticulation().isSleeping() : dynamic ? dynamic->isSleeping() : false);
    if (link) {
        out << ",\"articulation\":" << id(&link->getArticulation())
            << ",\"joint\":" << id(link->getInboundJoint()) << ",\"children\":[";
        std::vector<PxArticulationLink*> children(link->getNbChildren());
        link->getChildren(children.data(), static_cast<PxU32>(children.size()));
        for (std::size_t i = 0; i < children.size(); ++i) {
            if (i) out << ',';
            out << id(children[i]);
        }
        out << ']';
        if (auto* joint = link->getInboundJoint()) {
            out << ",\"parent_pose\":";
            pose(out, joint->getParentPose());
            out << ",\"child_pose\":";
            pose(out, joint->getChildPose());
        }
    }
    out << ",\"shapes\":[";
    std::vector<PxShape*> shapes(actor.getNbShapes());
    actor.getShapes(shapes.data(), static_cast<PxU32>(shapes.size()));
    for (std::size_t i = 0; i < shapes.size(); ++i) {
        if (i) out << ',';
        auto& shape = *shapes[i];
        out << "{\"id\":" << id(&shape) << ",\"world_pose\":";
        pose(out, PxShapeExt::getGlobalPose(shape, actor));
        out << ",\"local_pose\":";
        pose(out, shape.getLocalPose());
        out << ",\"geometry\":" << shape.getGeometryType() << ",\"dimensions\":";
        PxBoxGeometry box;
        PxSphereGeometry sphere;
        PxCapsuleGeometry capsule;
        if (shape.getBoxGeometry(box)) vector(out, box.halfExtents);
        else if (shape.getSphereGeometry(sphere)) out << '[' << sphere.radius << ']';
        else if (shape.getCapsuleGeometry(capsule)) out << '[' << capsule.radius << ',' << capsule.halfHeight << ']';
        else throw std::runtime_error("unexpected fixture shape geometry");
        out << '}';
    }
    out << "]}";
}

static PxArticulationLink* add_link(FeatureContext& context, PxArticulation& articulation,
    PxArticulationLink* parent, PxTransform transform, const PxGeometry& geometry, const char* name) {
    auto* link = feature_require(articulation.createLink(parent, transform), "articulation link");
    link->setName(name);
    auto* shape = feature_require(PxRigidActorExt::createExclusiveShape(*link, geometry, *context.material), "link shape");
    shape->setLocalPose(PxTransform(PxVec3(0.07f, 0.11f, -0.05f), PxQuat(0.19f, PxVec3(0, 0, 1))));
    if (!PxRigidBodyExt::updateMassAndInertia(*link, 2.0f)) throw std::runtime_error("link mass update failed");
    if (auto* joint = link->getInboundJoint()) {
        joint->setParentPose(parent->getGlobalPose().transformInv(transform));
        joint->setChildPose(PxTransform(PxIdentity));
        joint->setStiffness(10.0f);
        joint->setDamping(3.0f);
    }
    return link;
}

int main(int argc, char** argv) {
    try {
        if (argc != 3) throw std::runtime_error("Usage: pvd-objects capture.pvdstream truth.objects.json");
        std::ofstream truth(argv[2], std::ios::trunc);
        if (!truth) throw std::runtime_error("cannot open truth output");
        truth << std::setprecision(9) << std::boolalpha << "{\"sdk\":\"3.4.2\",\"frames\":[";
        FeatureContext context(argv[1]);
        context.scene->setGravity(PxVec3(0));
        auto* aggregate = feature_require(context.physics->createAggregate(16, false), "aggregate");
        auto* fixed = feature_require(context.physics->createRigidStatic(PxTransform(PxVec3(-3, 1, 0))), "static actor");
        fixed->setName("Aggregate static box");
        feature_require(PxRigidActorExt::createExclusiveShape(*fixed, PxBoxGeometry(0.3f, 0.4f, 0.5f), *context.material), "static shape");
        auto* moving = feature_require(context.physics->createRigidDynamic(PxTransform(PxVec3(-3, 3, 0))), "dynamic actor");
        moving->setName("Aggregate movable member");
        feature_require(PxRigidActorExt::createExclusiveShape(*moving, PxBoxGeometry(0.2f, 0.35f, 0.45f), *context.material), "dynamic shape");
        if (!PxRigidBodyExt::updateMassAndInertia(*moving, 1.0f)) throw std::runtime_error("dynamic mass update failed");
        moving->setLinearVelocity(PxVec3(0.2f, 0.1f, -0.15f));
        moving->setLinearDamping(1.0f);
        auto* articulation = feature_require(context.physics->createArticulation(), "articulation");
        articulation->setName("Branched articulation");
        articulation->setSleepThreshold(0.5f);
        auto* root = add_link(context, *articulation, nullptr, PxTransform(PxVec3(1, 2, 0), PxQuat(0.31f, PxVec3(0, 1, 0))), PxBoxGeometry(0.25f, 0.35f, 0.2f), "Root link");
        auto* child = add_link(context, *articulation, root, PxTransform(PxVec3(1, 3, 0)), PxCapsuleGeometry(0.2f, 0.3f), "Capsule child");
        add_link(context, *articulation, child, PxTransform(PxVec3(1, 4, 0)), PxSphereGeometry(0.3f), "Sphere grandchild");
        add_link(context, *articulation, root, PxTransform(PxVec3(2, 2, 0)), PxBoxGeometry(0.35f, 0.2f, 0.15f), "Second branch");
        if (!aggregate->addActor(*fixed) || !aggregate->addActor(*moving) || !aggregate->addArticulation(*articulation)) throw std::runtime_error("aggregate insertion failed");
        context.scene->addAggregate(*aggregate);
        moving->setWakeCounter(0.4f);
        root->addForce(PxVec3(0.03f, 0.01f, 0), PxForceMode::eIMPULSE);
        for (unsigned frame = 0; frame != 210; ++frame) {
            if (frame == 60 && !aggregate->removeActor(*moving)) throw std::runtime_error("aggregate actor removal failed");
            if (frame == 90) {
                context.scene->removeActor(*moving);
                if (!aggregate->addActor(*moving)) throw std::runtime_error("aggregate actor reattachment failed");
                moving->setWakeCounter(0.4f);
            }
            if (frame == 120 && !aggregate->removeArticulation(*articulation)) throw std::runtime_error("aggregate articulation removal failed");
            if (frame == 150) {
                context.scene->removeArticulation(*articulation);
                if (!aggregate->addArticulation(*articulation)) throw std::runtime_error("aggregate articulation reattachment failed");
            }
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"scene\":" << id(context.scene)
                << ",\"aggregate\":{\"id\":" << id(aggregate) << ",\"capacity\":" << aggregate->getMaxNbActors()
                << ",\"self_collision\":" << aggregate->getSelfCollision() << ",\"actors\":[";
            std::vector<PxActor*> members(aggregate->getNbActors());
            aggregate->getActors(members.data(), static_cast<PxU32>(members.size()));
            for (std::size_t i = 0; i < members.size(); ++i) {
                if (i) truth << ',';
                truth << id(members[i]);
            }
            truth << "]},\"articulation\":{\"id\":" << id(articulation)
                << ",\"aggregate\":" << id(articulation->getAggregate()) << ",\"sleeping\":" << articulation->isSleeping()
                << ",\"links\":[";
            std::vector<PxArticulationLink*> links(articulation->getNbLinks());
            articulation->getLinks(links.data(), static_cast<PxU32>(links.size()));
            for (std::size_t i = 0; i < links.size(); ++i) {
                if (i) truth << ',';
                truth << id(links[i]);
            }
            truth << "]},\"actors\":[";
            write_actor(truth, *fixed);
            truth << ',';
            write_actor(truth, *moving);
            for (auto* link : links) {
                truth << ',';
                write_actor(truth, *link);
            }
            truth << ']';
            context.step();
            truth << ",\"articulation_sleeping_at_end\":" << articulation->isSleeping() << '}';
        }
        truth << "]}\n";
        if (!truth) throw std::runtime_error("truth write failed");
        articulation->release();
        moving->release();
        fixed->release();
        aggregate->release();
        std::cout << "210 frames, aggregate with 2 rigid actors and 4 branched articulation links, getter truth written\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
