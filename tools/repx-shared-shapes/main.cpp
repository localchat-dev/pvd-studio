#include "../physx-features/feature_harness.h"
#include <algorithm>
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <map>
#include <set>
#include <vector>

template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"

static void text(std::ostream& out, const char* value) {
    out << '"';
    if (value) for (const auto* p=value; *p; ++p) {
        if (*p=='"' || *p=='\\') out << '\\';
        if (static_cast<unsigned char>(*p)<32) throw std::runtime_error("Unexpected control in fixture string");
        out << *p;
    }
    out << '"';
}
static void vec(std::ostream& out, PxVec3 v) { out << '[' << v.x << ',' << v.y << ',' << v.z << ']'; }
static void pose(std::ostream& out, PxTransform p) { out << '[' << p.q.x << ',' << p.q.y << ',' << p.q.z << ',' << p.q.w << ',' << p.p.x << ',' << p.p.y << ',' << p.p.z << ']'; }
static void filter(std::ostream& out, PxFilterData f) { out << '[' << f.word0 << ',' << f.word1 << ',' << f.word2 << ',' << f.word3 << ']'; }
static PxSerialObjectId id(PxCollection& collection, PxBase& object) {
    const auto result=collection.getId(object);
    if (result==PX_SERIAL_OBJECT_ID_INVALID) throw std::runtime_error("Missing serial identity");
    return result;
}

static void shape_json(std::ostream& out, PxShape& shape, PxCollection& collection) {
    out << "{\"id\":" << (shape.isExclusive()?0:id(collection,shape))
        << ",\"exclusive\":" << (shape.isExclusive()?"true":"false") << ",\"actor\":";
    auto* actor=shape.getActor();
    if (!shape.isExclusive() && actor) throw std::runtime_error("Shared SDK Shape returned an owner");
    out << (actor?id(collection,*actor):0) << ",\"name\":";
    text(out,shape.getName());
    out << ",\"pose\":"; pose(out,shape.getLocalPose());
    out << ",\"contact\":" << shape.getContactOffset() << ",\"rest\":" << shape.getRestOffset()
        << ",\"flags\":" << unsigned(shape.getFlags()) << ",\"simulation_filter\":";
    filter(out,shape.getSimulationFilterData());
    out << ",\"query_filter\":"; filter(out,shape.getQueryFilterData());
    const auto geometry=shape.getGeometry();
    out << ",\"geometry\":{\"type\":" << unsigned(geometry.getType()) << ",\"dimensions\":";
    switch (geometry.getType()) {
    case PxGeometryType::eBOX: vec(out,geometry.box().halfExtents); break;
    case PxGeometryType::eSPHERE: out << '[' << geometry.sphere().radius << ']'; break;
    case PxGeometryType::eCAPSULE: out << '[' << geometry.capsule().radius << ',' << geometry.capsule().halfHeight << ']'; break;
    default: throw std::runtime_error("Unimplemented fixture geometry");
    }
    out << "},\"materials\":[";
    std::vector<PxMaterial*> materials(shape.getNbMaterials());
    shape.getMaterials(materials.data(),static_cast<PxU32>(materials.size()));
    for (std::size_t i=0;i<materials.size();++i) {
        if(i) out << ',';
        auto& material=*materials[i];
        out << "{\"id\":" << id(collection,material) << ",\"static\":" << material.getStaticFriction()
            << ",\"dynamic\":" << material.getDynamicFriction() << ",\"restitution\":" << material.getRestitution()
            << ",\"flags\":" << unsigned(material.getFlags()) << ",\"friction_mode\":" << material.getFrictionCombineMode()
            << ",\"restitution_mode\":" << material.getRestitutionCombineMode() << '}';
    }
    out << "]}";
}

static void dump(PxCollection& collection, const std::string& path) {
    std::vector<PxRigidActor*> actors;
    std::set<PxShape*> unique;
    std::map<PxSerialObjectId,std::pair<PxShape*,std::vector<PxSerialObjectId>>> shared;
    for(PxU32 i=0;i<collection.getNbObjects();++i) if(auto* actor=collection.getObject(i).is<PxRigidActor>()) actors.push_back(actor);
    std::sort(actors.begin(),actors.end(),[&](PxRigidActor* a,PxRigidActor* b){return id(collection,*a)<id(collection,*b);});
    std::ofstream out(path);
    if(!out) throw std::runtime_error("Cannot open getter truth");
    out << std::setprecision(9) << "{\"actors\":[";
    std::size_t attachments=0;
    for(std::size_t i=0;i<actors.size();++i) {
        if(i) out << ',';
        auto& actor=*actors[i];
        out << "{\"id\":" << id(collection,actor) << ",\"name\":"; text(out,actor.getName());
        out << ",\"dynamic\":" << (actor.is<PxRigidDynamic>()?"true":"false") << ",\"pose\":";
        pose(out,actor.getGlobalPose());
        out << ",\"actor_flags\":" << unsigned(actor.getActorFlags()) << ",\"dominance\":" << unsigned(actor.getDominanceGroup())
            << ",\"owner\":" << unsigned(actor.getOwnerClient()) << ",\"shapes\":[";
        std::vector<PxShape*> shapes(actor.getNbShapes());
        actor.getShapes(shapes.data(),static_cast<PxU32>(shapes.size()));
        attachments+=shapes.size();
        for(std::size_t j=0;j<shapes.size();++j) {
            if(j) out << ',';
            auto* shape=shapes[j]; unique.insert(shape);
            if(!shape->isExclusive()) {
                const auto key=id(collection,*shape);
                auto found=shared.find(key);
                if(found!=shared.end() && found->second.first!=shape) throw std::runtime_error("One serial ID resolves to different Shape pointers");
                shared[key].first=shape;
                shared[key].second.push_back(id(collection,actor));
            }
            shape_json(out,*shape,collection);
        }
        out << "]}";
    }
    out << "],\"unique_shapes\":" << unique.size() << ",\"attachments\":" << attachments << ",\"shared\":[";
    bool first=true;
    for(auto& entry:shared) {
        if(!first) out << ','; first=false;
        out << "{\"id\":" << entry.first << ",\"same_pointer\":true,\"actors\":[";
        for(std::size_t i=0;i<entry.second.second.size();++i) { if(i) out << ','; out << entry.second.second[i]; }
        out << "]}";
    }
    out << "]}\n";
    out.close();
    if(!out) throw std::runtime_error("Getter truth write failed");
}

static void save(FeatureContext& context,const std::string& base,unsigned frame) {
    RepxResources resources;
    resources.registry=require(PxSerialization::createSerializationRegistry(*context.physics),"registry");
    resources.collection=require(PxCreateCollection(),"collection");
    const auto types=PxActorTypeFlag::eRIGID_STATIC|PxActorTypeFlag::eRIGID_DYNAMIC;
    std::vector<PxActor*> actors(context.scene->getNbActors(types));
    context.scene->getActors(types,actors.data(),static_cast<PxU32>(actors.size()));
    for(auto* actor:actors) resources.collection->add(*actor);
    PxSerialization::complete(*resources.collection,*resources.registry,nullptr,true);
    for(PxU32 i=0;i<resources.collection->getNbObjects();++i) {
        auto& object=resources.collection->getObject(i);
        resources.collection->addId(object,reinterpret_cast<std::uintptr_t>(&object));
    }
    const auto prefix=base+".frame-"+std::to_string(frame);
    dump(*resources.collection,prefix+".json");
    PxDefaultFileOutputStream output((prefix+".xml").c_str());
    if(!output.isValid() || !PxSerialization::serializeCollectionToXml(output,*resources.collection,*resources.registry)) throw std::runtime_error("Official shared Shape XML failed");
    context.step();
    context.flush_and_require_connected();
}

static void configure(PxShape& shape, bool changed) {
    shape.setName(changed?"SharedAfterDetach":"SharedFirst");
    shape.setLocalPose(PxTransform(PxVec3(changed?0.35f:0.2f,0.3f,-0.1f),PxQuat(0.3f,PxVec3(0,1,0))));
    shape.setContactOffset(changed?0.12f:0.08f);
    shape.setRestOffset(changed?-0.01f:0.02f);
    shape.setSimulationFilterData(PxFilterData(1,2,3,4));
    shape.setQueryFilterData(PxFilterData(7,8,9,changed?11:10));
    shape.setFlags(PxShapeFlag::eSIMULATION_SHAPE|PxShapeFlag::eSCENE_QUERY_SHAPE|PxShapeFlag::eVISUALIZATION);
}

static void produce(const char* capture,const std::string& base) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    auto* alternate=require(context.physics->createMaterial(0.7f,0.4f,0.3f),"alternate material");
    alternate->setFrictionCombineMode(PxCombineMode::eMAX);
    auto* shape=require(context.physics->createShape(PxBoxGeometry(0.4f,0.6f,0.8f),*context.material,false),"shared Shape");
    configure(*shape,false);
    auto* a=require(context.physics->createRigidStatic(PxTransform(PxVec3(-8,2,0))),"A"); a->setName("OwnerA");
    auto* b=require(context.physics->createRigidDynamic(PxTransform(PxVec3(8,2,0))),"B"); b->setName("OwnerB");
    a->attachShape(*shape); b->attachShape(*shape);
    auto* exclusive=require(PxRigidActorExt::createExclusiveShape(*a,PxSphereGeometry(0.31f),*context.material),"exclusive A");
    exclusive->setName("ExclusiveA"); exclusive->setLocalPose(PxTransform(PxVec3(0,2,0)));
    PxRigidBodyExt::updateMassAndInertia(*b,2.0f);
    context.scene->addActor(*a); context.scene->addActor(*b);
    save(context,base,0);
    a->detachShape(*shape); a->release();
    save(context,base,1);
    auto* c=require(context.physics->createRigidStatic(PxTransform(PxVec3(-12,3,1))),"C"); c->setName("OwnerC");
    c->attachShape(*shape); context.scene->addActor(*c);
    b->setGlobalPose(PxTransform(PxVec3(5,4,1)));
    save(context,base,2);
    b->detachShape(*shape); c->detachShape(*shape);
    shape->setGeometry(PxBoxGeometry(0.5f,0.7f,0.9f));
    shape->setMaterials(&alternate,1); configure(*shape,true);
    b->attachShape(*shape); c->attachShape(*shape);
    save(context,base,3);
    auto* exclusive_b=require(PxRigidActorExt::createExclusiveShape(*b,PxCapsuleGeometry(0.2f,0.6f),*context.material),"exclusive B");
    exclusive_b->setName("ExclusiveB"); exclusive_b->setLocalPose(PxTransform(PxVec3(0,3,0)));
    save(context,base,4);
    shape->release(); alternate->release();
    std::cout << "frames=5 shared_shape_id=" << reinterpret_cast<std::uintptr_t>(shape) << '\n';
}

static void import(const char* input,const char* truth,const char* roundtrip) {
    RepxImportEnvironment sdk;
    sdk.foundation=require(PxCreateFoundation(PX_FOUNDATION_VERSION,sdk.allocator,sdk.errors),"foundation");
    sdk.physics=require(PxCreatePhysics(PX_PHYSICS_VERSION,*sdk.foundation,PxTolerancesScale()),"physics");
    sdk.extensions=PxInitExtensions(*sdk.physics,nullptr);
    if(!sdk.extensions) throw std::runtime_error("extensions");
    sdk.cooking=require(PxCreateCooking(PX_PHYSICS_VERSION,*sdk.foundation,PxCookingParams(sdk.physics->getTolerancesScale())),"cooking");
    sdk.strings=&PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry=require(PxSerialization::createSerializationRegistry(*sdk.physics),"registry");
    PxDefaultFileInputData stream(input);
    if(!stream.isValid()) throw std::runtime_error("RepX input missing");
    resources.collection=require(PxSerialization::createCollectionFromXml(stream,*sdk.cooking,*resources.registry,nullptr,sdk.strings),"import");
    resources.own_objects=true;
    dump(*resources.collection,truth);
    PxDefaultFileOutputStream output(roundtrip);
    if(!output.isValid() || !PxSerialization::serializeCollectionToXml(output,*resources.collection,*resources.registry)) throw std::runtime_error("SDK reserialization failed");
}

int main(int argc,char** argv) {
    try {
        if(argc==5 && std::string(argv[1])=="--import") import(argv[2],argv[3],argv[4]);
        else if(argc==3) produce(argv[1],argv[2]);
        else throw std::runtime_error("Usage: producer capture truth-prefix | --import XML truth.json roundtrip.xml");
        return 0;
    } catch(const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
