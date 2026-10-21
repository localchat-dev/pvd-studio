#include "feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <memory>
#include <vector>

static unsigned number(const char* text, unsigned minimum, unsigned maximum) {
    const std::string value(text);
    if (value.empty() || value.find_first_not_of("0123456789") != std::string::npos) {
        throw std::runtime_error("Expected an unsigned decimal argument");
    }
    const auto parsed = std::stoul(value);
    if (parsed < minimum || parsed > maximum) throw std::runtime_error("Numeric argument outside allowed range");
    return static_cast<unsigned>(parsed);
}

static void vector_json(std::ostream& out, const PxVec3& value) {
    out << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void controller_json(std::ostream& out, PxController& controller) {
    auto* actor = controller.getActor();
    PxShape* shape = nullptr;
    if (actor->getShapes(&shape, 1) != 1) throw std::runtime_error("CCT proxy must have one shape");
    const PxTransform pose = actor->getGlobalPose() * shape->getLocalPose();
    out << "{\"actor\":" << reinterpret_cast<std::uintptr_t>(actor)
        << ",\"shape\":" << reinterpret_cast<std::uintptr_t>(shape) << ",\"position\":";
    vector_json(out, pose.p);
    out << ",\"rotation\":[" << pose.q.x << ',' << pose.q.y << ',' << pose.q.z << ',' << pose.q.w << ']';
    PxCapsuleGeometry capsule;
    PxBoxGeometry box;
    if (shape->getCapsuleGeometry(capsule)) {
        out << ",\"geometry\":\"capsule\",\"radius\":" << capsule.radius << ",\"half_height\":" << capsule.halfHeight;
    } else if (shape->getBoxGeometry(box)) {
        out << ",\"geometry\":\"box\",\"half_extents\":";
        vector_json(out, box.halfExtents);
    } else throw std::runtime_error("Unexpected CCT proxy geometry");
    out << '}';
}

static PxRigidStatic* static_box(FeatureContext& context, const PxVec3& position, const PxVec3& half_extents) {
    auto* actor = feature_require(context.physics->createRigidStatic(PxTransform(position)), "static actor");
    feature_require(PxRigidActorExt::createExclusiveShape(*actor, PxBoxGeometry(half_extents), *context.material), "static shape");
    context.scene->addActor(*actor);
    return actor;
}

int main(int argc, char** argv) {
    try {
        if (argc != 3 && argc != 5) {
            throw std::runtime_error("Usage: pvd-cct capture.pvdstream truth.json [frames static-boxes]\nDefaults: 1000 frames, 5000 static boxes");
        }
        const unsigned frame_count = argc == 5 ? number(argv[3], 1, 100000) : 1000;
        const unsigned static_count = argc == 5 ? number(argv[4], 0, 50000) : 5000;
        FeatureContext context(argv[1], PxPvdInstrumentationFlag::eDEBUG);
        auto release_manager = [](PxControllerManager* manager) { if (manager) manager->release(); };
        std::unique_ptr<PxControllerManager, decltype(release_manager)> manager(
            feature_require(PxCreateControllerManager(*context.scene), "controller manager"), release_manager);
        std::vector<PxRigidStatic*> environment;
        environment.push_back(static_box(context, PxVec3(0, -0.25f, 0), PxVec3(10, 0.25f, 5)));
        for (unsigned index = 0; index < static_count; ++index) {
            environment.push_back(static_box(context,
                PxVec3(20.0f + float(index % 100) * 1.2f, 0.5f, float(index / 100) * 1.2f), PxVec3(0.5f)));
        }
        PxCapsuleControllerDesc capsule;
        capsule.radius = 0.5f;
        capsule.height = 1.0f;
        capsule.stepOffset = 0.2f;
        capsule.position = PxExtendedVec3(-3, 1.1, -1.5);
        capsule.material = context.material;
        auto* capsule_controller = feature_require(manager->createController(capsule), "capsule controller");
        capsule_controller->getActor()->setName("CCT capsule proxy");
        PxBoxControllerDesc box;
        box.halfHeight = 0.8f;
        box.halfSideExtent = 0.4f;
        box.halfForwardExtent = 0.3f;
        box.stepOffset = 0.2f;
        box.position = PxExtendedVec3(-3, 0.9, 1.5);
        box.material = context.material;
        auto* box_controller = feature_require(manager->createController(box), "box controller");
        box_controller->getActor()->setName("CCT box proxy");
        std::ofstream truth(argv[2]);
        if (!truth) throw std::runtime_error("Cannot create CCT truth JSON");
        truth << std::setprecision(9) << "{\"sdk\":\"3.4.2\",\"instrumentation\":\"debug\",\"static_boxes\":"
            << static_count << ",\"scale_coeff\":0.8,\"frames\":[";
        for (unsigned frame = 0; frame < frame_count; ++frame) {
            const float direction = (frame / 100) % 2 == 0 ? 1.0f : -1.0f;
            // A stationary first frame separates SDK initialization from subsequent frame-boundary poses.
            const PxVec3 displacement = frame == 0 ? PxVec3(0) : PxVec3(0.06f * direction, -0.2f, 0);
            capsule_controller->move(displacement, 0.001f, 1.0f / 60.0f, PxControllerFilters());
            box_controller->move(displacement, 0.001f, 1.0f / 60.0f, PxControllerFilters());
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"controllers\":[";
            controller_json(truth, *capsule_controller);
            truth << ',';
            controller_json(truth, *box_controller);
            truth << "]}";
            context.step();
        }
        truth << "]}\n";
        truth.close();
        if (!truth) throw std::runtime_error("CCT truth write failed");
        manager.reset();
        for (auto* actor : environment) actor->release();
        std::cout << frame_count << " frames, " << static_count << " static boxes and two moving CCT proxies\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
