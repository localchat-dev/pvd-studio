#include "../physx-features/feature_harness.h"

#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <memory>
#include <vector>

template <typename T> struct Release {
    void operator()(T* value) const { if (value) value->release(); }
};

struct ReadGuard {
    PxParticleReadData* data;
    ~ReadGuard() { data->unlock(); }
};

static void vector_json(std::ostream& output, PxVec3 value) {
    output << '[' << value.x << ',' << value.y << ',' << value.z << ']';
}

static void configure(PxParticleBase& actor) {
    actor.setRestOffset(0.08f);
    actor.setContactOffset(0.12f);
    actor.setMaxMotionDistance(0.3f);
    actor.setParticleBaseFlag(PxParticleBaseFlag::eGPU, false);
    for (auto flag : {PxParticleReadDataFlag::ePOSITION_BUFFER, PxParticleReadDataFlag::eVELOCITY_BUFFER,
                     PxParticleReadDataFlag::eREST_OFFSET_BUFFER, PxParticleReadDataFlag::eFLAGS_BUFFER,
                     PxParticleReadDataFlag::eCOLLISION_NORMAL_BUFFER}) {
        actor.setParticleReadDataFlag(flag, true);
    }
}

static void create(PxParticleBase& actor, std::vector<PxU32> indices, float x, float y) {
    std::vector<PxVec3> positions, velocities;
    std::vector<PxReal> offsets;
    for (size_t i = 0; i < indices.size(); ++i) {
        positions.emplace_back(x + 0.11f * static_cast<float>(i), y + 0.015f * static_cast<float>(i), 0.08f * static_cast<float>(i % 2));
        velocities.emplace_back(0.05f * static_cast<float>(i), -0.4f, 0.03f);
        offsets.push_back(0.03f + 0.004f * static_cast<float>(i));
    }
    PxParticleCreationData data;
    data.numParticles = static_cast<PxU32>(indices.size());
    data.indexBuffer = PxStrideIterator<const PxU32>(indices.data());
    data.positionBuffer = PxStrideIterator<const PxVec3>(positions.data());
    data.velocityBuffer = PxStrideIterator<const PxVec3>(velocities.data());
    data.restOffsetBuffer = PxStrideIterator<const PxF32>(offsets.data());
    if (!actor.createParticles(data)) throw std::runtime_error("SDK createParticles failed");
}

static void erase(PxParticleBase& actor, std::vector<PxU32> indices) {
    actor.releaseParticles(static_cast<PxU32>(indices.size()), PxStrideIterator<const PxU32>(indices.data()));
}

static void write_actor(std::ostream& output, PxParticleBase& actor, PxParticleFluid* fluid) {
    PxParticleFluidReadData* fluid_data = fluid ? fluid->lockParticleFluidReadData() : nullptr;
    PxParticleReadData* data = fluid ? fluid_data : actor.lockParticleReadData();
    feature_require(data, "particle read data");
    ReadGuard guard{data};
    output << "{\"actor\":" << reinterpret_cast<uintptr_t>(&actor)
           << ",\"kind\":\"" << (fluid ? "Fluid" : "System") << "\",\"scene\":" << reinterpret_cast<uintptr_t>(actor.getScene())
           << ",\"max_particles\":" << actor.getMaxParticles() << ",\"valid_range\":" << data->validParticleRange
           << ",\"count\":" << data->nbValidParticles << ",\"rest_offset\":" << actor.getRestOffset()
           << ",\"base_flags\":" << static_cast<PxU16>(actor.getParticleBaseFlags())
           << ",\"read_flags\":" << static_cast<PxU16>(actor.getParticleReadDataFlags())
           << ",\"rest_particle_distance\":";
    if (fluid) output << fluid->getRestParticleDistance(); else output << "null";
    output << ",\"particles\":[";
    bool comma = false;
    for (PxU32 index = 0; index < data->validParticleRange; ++index) {
        if (!(data->validParticleBitmap[index >> 5] & (1u << (index & 31)))) continue;
        if (comma) output << ',';
        comma = true;
        output << "{\"index\":" << index << ",\"position\":";
        vector_json(output, data->positionBuffer[index]);
        output << ",\"velocity\":";
        vector_json(output, data->velocityBuffer[index]);
        output << ",\"rest_offset\":" << data->restOffsetBuffer[index]
               << ",\"flags\":" << static_cast<PxU16>(data->flagsBuffer[index]) << ",\"collision_normal\":";
        if (data->collisionNormalBuffer.ptr()) vector_json(output, data->collisionNormalBuffer[index]); else output << "null";
        output << ",\"density\":";
        if (fluid_data && fluid_data->densityBuffer.ptr()) output << fluid_data->densityBuffer[index]; else output << "null";
        output << '}';
    }
    output << "]}";
}

int main(int argc, char** argv) {
    try {
        if (argc != 3) throw std::runtime_error("usage: pvd-particles capture.pvdstream truth.particles.json");
        std::ofstream truth(argv[2]);
        truth.exceptions(std::ios::badbit | std::ios::failbit);
        truth << std::setprecision(9);
        FeatureContext context(argv[1]);
        std::unique_ptr<PxRigidStatic, Release<PxRigidStatic>> floor(feature_require(PxCreatePlane(*context.physics, PxPlane(0, 1, 0, 0), *context.material), "floor"));
        context.scene->addActor(*floor);
        std::unique_ptr<PxParticleSystem, Release<PxParticleSystem>> system(feature_require(context.physics->createParticleSystem(128, true), "CPU particle system"));
        std::unique_ptr<PxParticleFluid, Release<PxParticleFluid>> fluid(feature_require(context.physics->createParticleFluid(128, true), "CPU particle fluid"));
        configure(*system);
        configure(*fluid);
        system->setName("Sparse particle system");
        fluid->setName("Sparse particle fluid");
        fluid->setRestParticleDistance(0.15f);
        fluid->setParticleReadDataFlag(PxParticleReadDataFlag::eDENSITY_BUFFER, true);
        create(*system, {0, 7, 31, 32, 63, 95}, -1.0f, 0.045f);
        create(*fluid, {1, 8, 30, 33, 64, 96}, 1.0f, 0.06f);
        context.scene->addActor(*system);
        context.scene->addActor(*fluid);
        truth << "{\"sdk\":\"3.4.2\",\"frames\":[";
        for (int frame = 0; frame < 10; ++frame) {
            if (frame == 2) { erase(*system, {7, 31}); erase(*fluid, {8, 33}); }
            if (frame == 3) { create(*system, {7}, -1.3f, 0.25f); create(*fluid, {8}, 1.3f, 0.3f); }
            if (frame == 4) system->releaseParticles();
            if (frame == 5) { create(*system, {31}, -1.1f, 0.5f); fluid->releaseParticles(); }
            if (frame == 6) { create(*system, {32, 63}, -1.4f, 0.2f); create(*fluid, {0, 31, 32}, 1.2f, 0.25f); }
            if (frame == 7) { erase(*system, {31}); erase(*fluid, {0}); }
            context.step();
            if (frame) truth << ',';
            truth << "{\"frame\":" << frame << ",\"systems\":[";
            write_actor(truth, *system, nullptr);
            truth << ',';
            write_actor(truth, *fluid, fluid.get());
            truth << "]}";
        }
        truth << "]}\n";
        std::cout << "PhysX 3.4.2 CPU particle system/fluid capture and SDK readback: 10 frames\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
