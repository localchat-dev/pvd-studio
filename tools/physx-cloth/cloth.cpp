#include "../physx-features/feature_harness.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <string>
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
template <typename T> static void array(std::ostream& out, const std::vector<T>& values) {
    out << '[';
    for (std::size_t i = 0; i < values.size(); ++i) {
        if (i) out << ',';
        out << values[i];
    }
    out << ']';
}

struct FabricInput {
    std::vector<PxClothParticle> particles;
    std::vector<PxU32> triangles;
    PxClothFabric* fabric;

    FabricInput(FeatureContext& context, PxU32 rows, PxU32 columns) {
        for (PxU32 row = 0; row < rows; ++row) {
            for (PxU32 column = 0; column < columns; ++column) {
                PxClothParticle particle;
                particle.pos = PxVec3(column * 0.51f + row * 0.03f, 2.5f - row * 0.48f, 0.025f * column * row);
                particle.invWeight = row == 0 ? 0.0f : 0.5f + column * 0.1f;
                particles.push_back(particle);
            }
        }
        for (PxU32 row = 0; row + 1 < rows; ++row) {
            for (PxU32 column = 0; column + 1 < columns; ++column) {
                PxU32 a = row * columns + column, b = a + 1, c = a + columns, d = c + 1;
                for (PxU32 index : {a, c, b, b, c, d}) triangles.push_back(index);
            }
        }
        PxClothMeshDesc description;
        description.points.count = static_cast<PxU32>(particles.size());
        description.points.stride = sizeof(PxClothParticle);
        description.points.data = particles.data();
        description.invMasses.count = description.points.count;
        description.invMasses.stride = sizeof(PxClothParticle);
        description.invMasses.data = &particles[0].invWeight;
        description.triangles.count = static_cast<PxU32>(triangles.size() / 3);
        description.triangles.stride = sizeof(PxU32) * 3;
        description.triangles.data = triangles.data();
        fabric = feature_require(PxClothFabricCreate(*context.physics, description, PxVec3(0, -1, 0)), "cloth fabric");
    }
};

static PxCloth* cloth(FeatureContext& context, FabricInput& input, PxVec3 position, float angle) {
    auto* value = feature_require(context.physics->createCloth(
        PxTransform(position, PxQuat(angle, PxVec3(0, 1, 0))), *input.fabric, input.particles.data(), PxClothFlags(0)), "cloth actor");
    value->setName("SDK CPU cloth");
    value->setSolverFrequency(120.0f);
    value->setDampingCoefficient(PxVec3(0.15f));
    value->setExternalAcceleration(PxVec3(0.15f, 0.0f, 0.7f));
    context.scene->addActor(*value);
    return value;
}

static void write_fabric(std::ostream& out, PxClothFabric& fabric) {
    std::vector<PxU32> indices(fabric.getNbParticleIndices()), sets(fabric.getNbSets());
    std::vector<PxReal> rest(fabric.getNbRestvalues());
    std::vector<PxClothFabricPhase> phases(fabric.getNbPhases());
    fabric.getParticleIndices(indices.data(), static_cast<PxU32>(indices.size()));
    fabric.getSets(sets.data(), static_cast<PxU32>(sets.size()));
    fabric.getRestvalues(rest.data(), static_cast<PxU32>(rest.size()));
    fabric.getPhases(phases.data(), static_cast<PxU32>(phases.size()));
    out << "{\"id\":" << id(&fabric) << ",\"particles\":" << fabric.getNbParticles() << ",\"indices\":";
    array(out, indices);
    out << ",\"sets\":";
    array(out, sets);
    out << ",\"rest\":";
    array(out, rest);
    out << ",\"phases\":[";
    for (std::size_t i = 0; i < phases.size(); ++i) {
        if (i) out << ',';
        out << '[' << phases[i].phaseType << ',' << phases[i].setIndex << ']';
    }
    out << "]}";
}

static void write_cloth(std::ostream& out, PxCloth& cloth) {
    out << "{\"id\":" << id(&cloth) << ",\"fabric\":" << id(cloth.getFabric())
        << ",\"sleeping\":" << cloth.isSleeping() << ",\"previous_time_step\":" << cloth.getPreviousTimeStep() << ",\"pose\":";
    pose(out, cloth.getGlobalPose());
    auto* data = feature_require(cloth.lockParticleData(), "cloth read lock");
    out << ",\"particles\":[";
    for (PxU32 i = 0; i < cloth.getNbParticles(); ++i) {
        if (i) out << ',';
        out << "{\"local\":";
        vector(out, data->particles[i].pos);
        out << ",\"world\":";
        vector(out, cloth.getGlobalPose().transform(data->particles[i].pos));
        out << ",\"inverse_mass\":" << data->particles[i].invWeight << ",\"previous_local\":";
        vector(out, data->previousParticles[i].pos);
        out << '}';
    }
    data->unlock();
    out << "]}";
}

int main(int argc, char** argv) {
    try {
        if (argc < 3 || argc > 4 || (argc == 4 && std::string(argv[3]) != "--reuse-fabric-address"))
            throw std::runtime_error("Usage: pvd-cloth capture.pvdstream truth.cloth.json [--reuse-fabric-address]");
        const bool reuse_address = argc == 4;
        std::ofstream truth(argv[2], std::ios::trunc);
        if (!truth) throw std::runtime_error("cannot open truth output");
        truth << std::setprecision(9) << std::boolalpha << "{\"sdk\":\"3.4.2\",\"frames\":[";
        FeatureContext context(argv[1]);
        FabricInput input(context, 4, 5);
        PxCloth* first = cloth(context, input, PxVec3(-3, 1, 0), 0.31f);
        PxCloth* second = cloth(context, input, PxVec3(2, 0.5f, -1), -0.53f);
        for (unsigned frame = 0; frame != 75; ++frame) {
            if (frame == 20) first->setGlobalPose(PxTransform(PxVec3(-2, 2, 1), PxQuat(0.47f, PxVec3(1, 0, 0))));
            if (frame == 30) {
                context.scene->removeActor(*first);
                context.scene->addActor(*first);
            }
            if (frame == 45) {
                PxClothFabric* old_fabric = input.fabric;
                const auto old_address = id(old_fabric);
                if (!reuse_address) input = FabricInput(context, 3, 4);
                first->release();
                second->release();
                old_fabric->release();
                if (reuse_address) {
                    input = FabricInput(context, 3, 4);
                    if (id(input.fabric) != old_address) throw std::runtime_error("SDK allocator did not reuse the Fabric address; probe condition absent");
                }
                first = cloth(context, input, PxVec3(-1, 2, 1), 0.71f);
                second = cloth(context, input, PxVec3(3, 1, 0), -0.23f);
            }
            if (frame == 60) first->putToSleep();
            if (frame == 68) first->wakeUp();
            context.step();
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"fabric\":";
            write_fabric(truth, *input.fabric);
            truth << ",\"cloths\":[";
            write_cloth(truth, *first);
            truth << ',';
            write_cloth(truth, *second);
            truth << "]}";
        }
        truth << "]}\n";
        if (!truth) throw std::runtime_error("truth write failed");
        first->release();
        second->release();
        input.fabric->release();
        std::cout << "75 frames, 2 CPU cloths, shared/replaced fabric, direct particle and constraint getters written\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
