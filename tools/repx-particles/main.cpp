#include "../physx-features/feature_harness.h"
#include <algorithm>
#include <cstdint>
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

static void vector_json(std::ostream& out, PxVec3 v) { out << '[' << v.x << ',' << v.y << ',' << v.z << ']'; }

static void dump(PxCollection& collection, const std::string& path) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot create particle truth");
    out << std::setprecision(9) << std::boolalpha
        << "{\"schema\":2,\"sdk_version\":[" << PX_PHYSICS_VERSION_MAJOR << ',' << PX_PHYSICS_VERSION_MINOR << ',' << PX_PHYSICS_VERSION_BUGFIX
        << "],\"systems\":[";
    bool first = true;
    for (PxU32 i=0; i<collection.getNbObjects(); ++i) {
        auto* actor=collection.getObject(i).is<PxParticleBase>(); if (!actor) continue;
        if (!first) out << ','; first=false;
        auto* data=require(actor->lockParticleReadData(),"particle read");
        out << "{\"id\":" << id(collection,actor) << ",\"name\":\"" << actor->getName() << "\",\"type\":\"" << actor->getConcreteTypeName()
            << "\",\"capacity\":" << actor->getMaxParticles() << ",\"range\":" << data->validParticleRange << ",\"count\":" << data->nbValidParticles
            << ",\"actor_flags\":" << PxU32(actor->getActorFlags()) << ",\"dominance\":" << PxU32(actor->getDominanceGroup()) << ",\"owner_client\":" << PxU32(actor->getOwnerClient())
            << ",\"client_flags\":" << PxU32(actor->getClientBehaviorFlags())
            << ",\"base_flags\":" << PxU32(actor->getParticleBaseFlags()) << ",\"read_flags\":" << PxU32(actor->getParticleReadDataFlags())
            << ",\"damping\":" << actor->getDamping() << ",\"mass\":" << actor->getParticleMass() << ",\"restitution\":" << actor->getRestitution()
            << ",\"static_friction\":" << actor->getStaticFriction() << ",\"dynamic_friction\":" << actor->getDynamicFriction()
            << ",\"max_motion\":" << actor->getMaxMotionDistance() << ",\"rest\":" << actor->getRestOffset() << ",\"contact\":" << actor->getContactOffset()
            << ",\"grid\":" << actor->getGridSize() << ",\"acceleration\":"; vector_json(out,actor->getExternalAcceleration());
        PxVec3 normal; PxReal distance; actor->getProjectionPlane(normal,distance);
        out << ",\"plane_normal\":"; vector_json(out,normal); out << ",\"plane_distance\":" << distance;
        const auto filter=actor->getSimulationFilterData(); out << ",\"filter\":[" << filter.word0 << ',' << filter.word1 << ',' << filter.word2 << ',' << filter.word3 << ']';
        if (auto* fluid=actor->is<PxParticleFluid>()) {
            out << ",\"stiffness\":" << fluid->getStiffness() << ",\"viscosity\":" << fluid->getViscosity() << ",\"spacing\":" << fluid->getRestParticleDistance();
        }
        out << ",\"particles\":[";
        bool particle_first=true;
        for (PxU32 index=0; index<data->validParticleRange; ++index) {
            if (!(data->validParticleBitmap[index>>5] & (1u << (index&31)))) continue;
            if (!particle_first) out << ','; particle_first=false;
            out << "{\"index\":" << index << ",\"position\":"; vector_json(out,data->positionBuffer[index]);
            out << ",\"velocity\":"; vector_json(out,data->velocityBuffer[index]);
            out << ",\"flags\":" << PxU32(data->flagsBuffer[index]);
            if (data->restOffsetBuffer.ptr()) out << ",\"rest\":" << data->restOffsetBuffer[index];
            out << '}';
        }
        data->unlock(); out << "]}";
    }
    out << "]}\n"; out.close(); if (!out) throw std::runtime_error("Particle truth write failed");
}

static void serialize(PxCollection& collection, PxSerializationRegistry& registry, const std::string& path) {
    PxDefaultFileOutputStream output(path.c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output,collection,registry)) throw std::runtime_error("Particle XML serialization failed");
}

static void snapshot(FeatureContext& context, const std::string& directory, unsigned frame) {
    RepxResources resources;
    resources.registry=require(PxSerialization::createSerializationRegistry(*context.physics),"registry");
    resources.collection=require(PxCreateCollection(),"collection");
    const auto flags=PxActorTypeFlag::ePARTICLE_SYSTEM | PxActorTypeFlag::ePARTICLE_FLUID;
    std::vector<PxActor*> actors(context.scene->getNbActors(flags));
    context.scene->getActors(flags,actors.data(),PxU32(actors.size()));
    for (auto* actor:actors) resources.collection->add(*actor,reinterpret_cast<std::uintptr_t>(actor));
    const auto prefix=directory+"/frame-"+std::to_string(frame);
    dump(*resources.collection,prefix+"-truth.json"); serialize(*resources.collection,*resources.registry,prefix+"-reference.xml");
}

static void import_collection(const char* path,const char* truth,const char* roundtrip) {
    RepxImportEnvironment sdk;
    sdk.foundation=require(PxCreateFoundation(PX_FOUNDATION_VERSION,sdk.allocator,sdk.errors),"foundation");
    sdk.physics=require(PxCreatePhysics(PX_PHYSICS_VERSION,*sdk.foundation,PxTolerancesScale()),"physics");
    sdk.extensions=PxInitExtensions(*sdk.physics,nullptr); if (!sdk.extensions) throw std::runtime_error("Extensions failed");
    sdk.cooking=require(PxCreateCooking(PX_PHYSICS_VERSION,*sdk.foundation,PxCookingParams(sdk.physics->getTolerancesScale())),"cooking");
    sdk.strings=&PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources; resources.registry=require(PxSerialization::createSerializationRegistry(*sdk.physics),"registry");
    PxDefaultFileInputData input(path); if (!input.isValid()) throw std::runtime_error("Cannot read XML");
    resources.collection=require(PxSerialization::createCollectionFromXml(input,*sdk.cooking,*resources.registry,nullptr,sdk.strings),"particle collection");
    resources.own_objects=true; dump(*resources.collection,truth); serialize(*resources.collection,*resources.registry,roundtrip);
}

static void configure(PxParticleBase& actor,bool per_rest,const char* name,unsigned count,float x) {
    actor.setName(name); actor.setParticleBaseFlag(PxParticleBaseFlag::eGPU,false);
    actor.setRestOffset(0.04f); actor.setContactOffset(0.1f); actor.setMaxMotionDistance(0.25f); actor.setGridSize(1.5f);
    actor.setDamping(0.2f); actor.setParticleMass(0.7f); actor.setRestitution(0.3f); actor.setStaticFriction(0.4f); actor.setDynamicFriction(0.2f);
    actor.setExternalAcceleration(PxVec3(0.05f,0,0)); actor.setProjectionPlane(PxVec3(0,1,0),3);
    actor.setSimulationFilterData(PxFilterData(17,19,23,29));
    for (PxU32 i=0;i!=(actor.is<PxParticleFluid>() ? 7u : 6u);++i) actor.setParticleReadDataFlag(PxParticleReadDataFlag::Enum(1<<i),i==0 || i==1 || i==3 || (i==2 && per_rest));
    std::vector<PxU32> indices; std::vector<PxVec3> positions,velocities; std::vector<PxReal> rests;
    for (unsigned i=0;i<count;++i) { indices.push_back(i); positions.push_back(PxVec3(x+float(i),4,0)); velocities.push_back(PxVec3(0.1f*float(i),0,0)); rests.push_back(0.015f+0.002f*float(i)); }
    PxParticleCreationData data; data.numParticles=count; data.indexBuffer=PxStrideIterator<const PxU32>(indices.data());
    data.positionBuffer=PxStrideIterator<const PxVec3>(positions.data()); data.velocityBuffer=PxStrideIterator<const PxVec3>(velocities.data());
    if (per_rest) data.restOffsetBuffer=PxStrideIterator<const PxF32>(rests.data());
    if (!actor.createParticles(data)) throw std::runtime_error("Particle creation failed");
}

static void add_particles(PxParticleBase& actor, bool per_rest, const std::vector<PxU32>& indices, float x) {
    std::vector<PxVec3> positions, velocities;
    std::vector<PxF32> rests;
    for (auto index : indices) {
        positions.push_back(PxVec3(x + float(index), 7, 1));
        velocities.push_back(PxVec3(0.01f * float(index), 0.1f, 0));
        rests.push_back(0.01f + 0.0001f * float(index));
    }
    PxParticleCreationData data;
    data.numParticles = PxU32(indices.size());
    data.indexBuffer = PxStrideIterator<const PxU32>(indices.data());
    data.positionBuffer = PxStrideIterator<const PxVec3>(positions.data());
    data.velocityBuffer = PxStrideIterator<const PxVec3>(velocities.data());
    if (per_rest) data.restOffsetBuffer = PxStrideIterator<const PxF32>(rests.data());
    if (!actor.createParticles(data)) throw std::runtime_error("Sparse particle creation failed");
}

static void produce(const char* capture,const std::string& directory) {
    FeatureContext context(capture); context.scene->setGravity(PxVec3(0));
    auto* system=require(context.physics->createParticleSystem(96,false),"system"); configure(*system,false,"dense-system",32,0);
    system->setParticleBaseFlag(PxParticleBaseFlag::eCOLLISION_TWOWAY,true);
    system->setParticleBaseFlag(PxParticleBaseFlag::ePROJECT_TO_PLANE,true);
    auto make_fluid=[&](const char* name,unsigned count) {
        auto* actor=require(context.physics->createParticleFluid(64,true),"fluid"); configure(*actor,true,name,count,10);
        actor->setRestParticleDistance(0.04f); actor->setStiffness(13); actor->setViscosity(1.3f); context.scene->addActor(*actor); return actor;
    };
    context.scene->addActor(*system); auto* fluid=make_fluid("dense-fluid",5);
    for (unsigned frame=0;frame!=18;++frame) {
        if (frame==1) { system->setDamping(0.35f); system->setExternalAcceleration(PxVec3(0.1f,0.2f,0)); fluid->setViscosity(1.7f); }
        if (frame==2) { fluid->release(); fluid=make_fluid("recreated-fluid",3); }
        if (frame==3) { PxU32 index=31; system->releaseParticles(1,PxStrideIterator<const PxU32>(&index)); }
        if (frame==4) {
            const PxU32 indices[] = {0,2,15,30};
            system->releaseParticles(4, PxStrideIterator<const PxU32>(indices));
            add_particles(*system, false, {63}, 20);
            const PxU32 fluid_indices[] = {0,2};
            fluid->releaseParticles(2, PxStrideIterator<const PxU32>(fluid_indices));
        }
        if (frame==5) {
            add_particles(*system, false, {0,2,32,64}, 40);
            add_particles(*fluid, true, {0}, 10);
        }
        if (frame==6) { system->releaseParticles(); fluid->releaseParticles(); }
        if (frame==7) {
            add_particles(*system, false, {1,31,63}, 60);
            add_particles(*fluid, true, {2,35}, 20);
        }
        const auto client_flags = PxActorClientBehaviorFlags(PxU8(frame < 7 ? frame : frame == 17 ? 0 : frame - 1));
        system->setClientBehaviorFlags(client_flags);
        fluid->setClientBehaviorFlags(client_flags);
        context.step(); snapshot(context,directory,frame);
    }
    system->release(); fluid->release();
}

static void produce_empty(const char* capture, const std::string& directory) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    const char* names[] = {
        "cycle-system", "cycle-system-rest", "cycle-fluid", "cycle-fluid-rest",
        "empty-system-default", "empty-system-rest-read", "empty-fluid-density", "empty-fluid-rest-read"
    };
    std::vector<PxParticleBase*> actors;
    for (unsigned index = 0; index != 8; ++index) {
        const bool fluid = (index % 4) >= 2;
        const bool per_rest = (index % 2) != 0;
        PxParticleBase* actor = fluid
            ? static_cast<PxParticleBase*>(require(context.physics->createParticleFluid(64, per_rest), "empty fluid"))
            : static_cast<PxParticleBase*>(require(context.physics->createParticleSystem(64, per_rest), "empty system"));
        configure(*actor, per_rest, names[index], 0, 0);
        if (auto* value = actor->is<PxParticleFluid>()) {
            value->setRestParticleDistance(0.04f);
            value->setStiffness(13);
            value->setViscosity(1.3f);
        }
        if (index >= 4) {
            const PxU32 masks[] = {9, 63, 73, 127};
            for (PxU32 bit = 0; bit != (fluid ? 7u : 6u); ++bit) {
                actor->setParticleReadDataFlag(PxParticleReadDataFlag::Enum(1u << bit), (masks[index - 4] & (1u << bit)) != 0);
            }
        }
        actor->setParticleBaseFlag(PxParticleBaseFlag::eCOLLISION_TWOWAY, index % 2 != 0);
        actor->setParticleBaseFlag(PxParticleBaseFlag::ePROJECT_TO_PLANE, index % 4 >= 2);
        context.scene->addActor(*actor);
        actors.push_back(actor);
    }
    for (unsigned frame = 0; frame != 4; ++frame) {
        for (unsigned index = 0; index != actors.size(); ++index) {
            auto* actor = actors[index];
            if (index < 4) {
                if (frame == 1) add_particles(*actor, index % 2 != 0, {0, 31, 63}, float(index) * 100);
                if (frame == 2) actor->releaseParticles();
                if (frame == 3) add_particles(*actor, index % 2 != 0, {1, 32, 63}, float(index) * 100);
            }
            actor->setClientBehaviorFlags(PxActorClientBehaviorFlags(PxU8(frame * 4 + index % 4)));
            actor->setActorFlag(PxActorFlag::eVISUALIZATION, frame % 2 != 0);
        }
        context.step();
        snapshot(context, directory, frame);
    }
    for (auto* actor : actors) actor->release();
}

int main(int argc,char** argv) {
    try {
        if (argc==5 && std::string(argv[1])=="--import") import_collection(argv[2],argv[3],argv[4]);
        else if (argc==4 && std::string(argv[1])=="--empty") produce_empty(argv[2],argv[3]);
        else if (argc==3) produce(argv[1],argv[2]);
        else throw std::runtime_error("Usage: pvd-repx-particles [--empty] capture.pvdstream output-directory | --import input.xml truth.json roundtrip.xml");
        return 0;
    } catch(const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
