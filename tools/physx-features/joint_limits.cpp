#include "feature_harness.h"
#include "CmVisualization.h"
#include "PxPvdRenderBuffer.h"
#include <chrono>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <thread>

struct Options {
    std::string capture;
    std::string truth;
    std::string host;
    unsigned port = 5425;
    unsigned interval_ms = 16;
};

static Options options(int argc, char** argv) {
    const char* usage = "Usage: pvd-joint-limits capture.pvdstream truth.json\n"
        "   or: pvd-joint-limits --host ADDRESS [--port 5425] [--interval-ms 16] truth.json";
    Options result;
    if (argc == 3 && std::string(argv[1]).find("--") != 0) {
        result.capture = argv[1];
        result.truth = argv[2];
        result.interval_ms = 0;
        return result;
    }
    const auto number = [](const std::string& value, unsigned minimum, unsigned maximum) {
        if (value.empty() || value.find_first_not_of("0123456789") != std::string::npos) {
            throw std::runtime_error("Expected an unsigned decimal argument");
        }
        const auto parsed = std::stoul(value);
        if (parsed < minimum || parsed > maximum) throw std::runtime_error("Numeric argument outside allowed range");
        return static_cast<unsigned>(parsed);
    };
    for (int i = 1; i < argc; ++i) {
        const std::string key = argv[i];
        if (key == "--host" || key == "--port" || key == "--interval-ms") {
            if (++i == argc) throw std::runtime_error(usage);
            const std::string value = argv[i];
            if (key == "--host") result.host = value;
            else if (key == "--port") result.port = number(value, 1, 65535);
            else result.interval_ms = number(value, 0, 10000);
        } else if (key.find("--") == 0 || !result.truth.empty()) {
            throw std::runtime_error(usage);
        } else result.truth = key;
    }
    if (result.host.empty() || result.truth.empty()) throw std::runtime_error(usage);
    return result;
}

static void vector_json(std::ostream& output, PxVec3 value) {
    output << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void pose_json(std::ostream& output, const PxTransform& pose) {
    output << "{\"position\":";
    vector_json(output, pose.p);
    output << ",\"rotation\":[" << pose.q.x << ',' << pose.q.y << ',' << pose.q.z << ',' << pose.q.w << "]}";
}

static void geometry_json(std::ostream& output, const PxRenderBuffer& buffer) {
    output << '[';
    bool first = true;
    const auto line = [&](PxVec3 a, PxVec3 b, PxU32 color) {
        if (!first) output << ',';
        first = false;
        output << "{\"positions\":[";
        vector_json(output, a);
        output << ',';
        vector_json(output, b);
        output << "],\"color\":" << color << '}';
    };
    for (PxU32 i = 0; i < buffer.getNbLines(); ++i) {
        const auto& item = buffer.getLines()[i];
        line(item.pos0, item.pos1, item.color0);
    }
    for (PxU32 i = 0; i < buffer.getNbTriangles(); ++i) {
        const auto& item = buffer.getTriangles()[i];
        line(item.pos0, item.pos1, item.color0);
        line(item.pos1, item.pos2, item.color1);
        line(item.pos2, item.pos0, item.color2);
    }
    output << ']';
}

static void reference_cases(std::ostream& output) {
    output << "\"reference_cases\":[";
    bool first = true;
    for (int variant = 0; variant < 2; ++variant) {
        const PxTransform pose(PxVec3(variant ? -3.0f : 1.0f, 2.0f, 0.5f),
            PxQuat(variant ? -0.83f : 0.45f, PxVec3(0.3f, 0.6f, 0.4f).getNormalized()));
        const bool active = variant != 0;
        for (int kind = 0; kind < 4; ++kind) {
            if (!first) output << ',';
            first = false;
            Cm::RenderBuffer buffer;
            Cm::RenderOutput renderer(buffer);
            output << "{\"kind\":" << kind << ",\"active\":" << (active ? "true" : "false") << ",\"pose\":";
            pose_json(output, pose);
            if (kind == 0) {
                const float limit = variant ? -2.5f : 1.25f;
                output << ",\"value\":" << limit;
                Cm::visualizeLinearLimit(renderer, 1.0f, pose, PxTransform(PxIdentity), limit, active);
            } else if (kind == 1) {
                output << ",\"lower\":-0.65,\"upper\":1.1";
                Cm::visualizeAngularLimit(renderer, 1.0f, pose, -0.65f, 1.1f, active);
            } else if (kind == 2) {
                output << ",\"y_swing\":0.21,\"z_swing\":0.13";
                Cm::visualizeLimitCone(renderer, 1.0f, pose, 0.21f, 0.13f, active);
            } else {
                output << ",\"angle\":0.47";
                Cm::visualizeDoubleCone(renderer, 1.0f, pose, 0.47f, active);
            }
            output << ",\"lines\":";
            geometry_json(output, buffer);
            output << '}';
        }
    }
    output << ']';
}

int main(int argc, char** argv) {
    try {
        const auto output = options(argc, argv);
        FeatureContext context(output.host.empty()
            ? FeatureConnection::file(output.capture.c_str())
            : FeatureConnection::socket(output.host.c_str(), output.port));
        context.scene->setGravity(PxVec3(0));
        context.scene->setVisualizationParameter(PxVisualizationParameter::eJOINT_LOCAL_FRAMES, 0.0f);
        std::ofstream truth(output.truth);
        if (!truth) throw std::runtime_error("Cannot open joint truth output");
        truth << std::setprecision(9) << "{\"sdk\":\"3.4.2\",";
        reference_cases(truth);
        truth << ",\"joints\":[";
        for (int variant = 0; variant < 2; ++variant) {
            for (int kind = 0; kind < 4; ++kind) {
                const PxTransform parent(PxVec3(float(kind * 4), float(variant * 4), 0.0f), PxQuat(0.3f, PxVec3(0, 0, 1)));
                PxTransform initial = parent;
                if (variant) {
                    if (kind == 0) initial.p += parent.q.rotate(PxVec3(1.2f, 0, 0));
                    else initial.q = parent.q * PxQuat(0.9f, kind == 1 ? PxVec3(1, 0, 0) : PxVec3(0, 1, 0));
                }
                auto* actor = feature_require(PxCreateDynamic(*context.physics, initial, PxBoxGeometry(0.25f, 0.25f, 0.25f), *context.material, 1.0f), "joint actor");
                actor->setName("joint-limit probe");
                actor->setSleepThreshold(0.0f);
                context.scene->addActor(*actor);
                PxJoint* joint = nullptr;
                if (kind == 0) {
                    auto* typed = feature_require(PxPrismaticJointCreate(*context.physics, nullptr, parent, actor, PxTransform(PxIdentity)), "prismatic joint");
                    typed->setLimit(PxJointLinearLimitPair(context.physics->getTolerancesScale(), -0.6f, 0.9f, 0.05f));
                    typed->setPrismaticJointFlag(PxPrismaticJointFlag::eLIMIT_ENABLED, true);
                    joint = typed;
                } else if (kind == 1) {
                    auto* typed = feature_require(PxRevoluteJointCreate(*context.physics, nullptr, parent, actor, PxTransform(PxIdentity)), "revolute joint");
                    typed->setLimit(PxJointAngularLimitPair(-0.4f, 0.4f, 0.05f));
                    typed->setRevoluteJointFlag(PxRevoluteJointFlag::eLIMIT_ENABLED, true);
                    joint = typed;
                } else if (kind == 2) {
                    auto* typed = feature_require(PxSphericalJointCreate(*context.physics, nullptr, parent, actor, PxTransform(PxIdentity)), "spherical joint");
                    typed->setLimitCone(PxJointLimitCone(0.45f, 0.35f, 0.05f));
                    typed->setSphericalJointFlag(PxSphericalJointFlag::eLIMIT_ENABLED, true);
                    joint = typed;
                } else {
                    auto* typed = feature_require(PxD6JointCreate(*context.physics, nullptr, parent, actor, PxTransform(PxIdentity)), "D6 joint");
                    typed->setMotion(PxD6Axis::eSWING1, PxD6Motion::eLIMITED);
                    typed->setMotion(PxD6Axis::eSWING2, PxD6Motion::eFREE);
                    typed->setMotion(PxD6Axis::eTWIST, PxD6Motion::eFREE);
                    typed->setSwingLimit(PxJointLimitCone(0.4f, 0.55f, 0.05f));
                    joint = typed;
                }
                joint->setConstraintFlag(PxConstraintFlag::eVISUALIZATION, true);
                if (variant || kind) truth << ',';
                truth << "{\"id\":" << reinterpret_cast<std::uintptr_t>(joint) << ",\"kind\":" << kind << '}';
            }
        }
        pvdsdk::PvdDebugTriangle triangle(PxVec3(2, 1, -1), PxVec3(3, 1, -1), PxVec3(2.5f, 2, -1), 0xffff0000);
        triangle.color1 = 0xff00ff00;
        triangle.color2 = 0xff0000ff;
        truth << "],\"debug_triangle\":{\"positions\":[";
        vector_json(truth, triangle.pos0);
        truth << ',';
        vector_json(truth, triangle.pos1);
        truth << ',';
        vector_json(truth, triangle.pos2);
        truth << "],\"colors\":[" << triangle.color0 << ',' << triangle.color1 << ',' << triangle.color2 << "]},\"frames\":[";
        for (int frame = 0; frame < 6; ++frame) {
            context.scene->getScenePvdClient()->drawTriangles(&triangle, 1);
            context.step();
            if (!output.host.empty()) context.flush_and_require_connected();
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"lines\":";
            geometry_json(truth, context.scene->getRenderBuffer());
            truth << '}';
            if (output.interval_ms) std::this_thread::sleep_for(std::chrono::milliseconds(output.interval_ms));
        }
        if (!output.host.empty()) context.flush_and_require_connected();
        truth << "]}\n";
        truth.close();
        if (!truth) throw std::runtime_error("Joint truth write failed");
        std::cout << "SDK 3.4.2 joint limits: eight helper references and six simulation frames\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
