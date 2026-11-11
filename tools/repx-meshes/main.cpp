#include "../physx-features/feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>
template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"

#include "mesh_probe.h"

static void produce(const char* capture, const char* reference, const char* truth) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    auto* alternate=require(context.physics->createMaterial(0.8f,0.6f,0.45f),"alternate material");
    alternate->setFrictionCombineMode(PxCombineMode::eMAX);
    auto* triangle=cook_probe_triangle(context);
    const PxVec3 convex_points[]={PxVec3(-1,-0.5f,-0.75f),PxVec3(1,-0.5f,-0.75f),PxVec3(1,-0.5f,0.75f),PxVec3(-1,-0.5f,0.75f),PxVec3(0,1.25f,0)};
    PxConvexMeshDesc convex_desc;
    convex_desc.points.count=5;
    convex_desc.points.stride=sizeof(PxVec3);
    convex_desc.points.data=convex_points;
    convex_desc.flags=PxConvexFlag::eCOMPUTE_CONVEX;
    PxDefaultMemoryOutputStream convex_cooked;
    if(!context.cooking->cookConvexMesh(convex_desc,convex_cooked)) throw std::runtime_error("Cannot cook convex mesh");
    PxDefaultMemoryInputData convex_input(convex_cooked.getData(),convex_cooked.getSize());
    auto* convex=require(context.physics->createConvexMesh(convex_input),"convex mesh");
    for(int i=0;i<4;++i) {
        const PxTransform transform(PxVec3(float(i*6),1,0),PxQuat(0.25f,PxVec3(0,1,0)));
        auto* actor=require(context.physics->createRigidStatic(transform),"mesh actor");
        actor->setName(i==0?"triangle-positive":i==1?"triangle-negative":i==2?"convex-a":"convex-b");
        PxShape* shape=nullptr;
        if(i<2) {
            const PxMeshScale scale(PxVec3(i==1?-1.3f:1.2f,0.8f,1.7f),PxQuat(0.3f,PxVec3(0,1,0)));
            PxMaterial* table[]={i==0?context.material:alternate,i==0?alternate:context.material};
            shape=require(context.physics->createShape(PxTriangleMeshGeometry(triangle,scale,PxMeshGeometryFlag::eDOUBLE_SIDED),table,2,true),"triangle shape");
        } else {
            const PxMeshScale scale(PxVec3(0.8f,1.4f,i==2?1.2f:1.7f),PxQuat(0.45f,PxVec3(0,0,1)));
            shape=require(context.physics->createShape(PxConvexMeshGeometry(convex,scale,PxConvexMeshGeometryFlag::eTIGHT_BOUNDS,0.03f),*context.material,true),"convex shape");
        }
        shape->setLocalPose(PxTransform(PxVec3(0.1f,-0.2f,0.3f),PxQuat(-0.2f,PxVec3(1,0,0))));
        actor->attachShape(*shape);
        shape->release();
        context.scene->addActor(*actor);
    }
    auto* field=cook_probe_height_field(context);
    for(int i=0;i<2;++i) {
        auto* actor=require(context.physics->createRigidStatic(PxTransform(PxVec3(25+float(i*8),2,-3),PxQuat(0.35f,PxVec3(0,1,0)))),"height field actor");
        actor->setName(i==0?"heightfield-a":"heightfield-b");
        PxMaterial* table[]={context.material,alternate};
        auto* shape=require(context.physics->createShape(PxHeightFieldGeometry(field,PxMeshGeometryFlag::eDOUBLE_SIDED,0.25f,1.2f+i*0.3f,0.8f),table,2,true),"height field shape");
        shape->setLocalPose(PxTransform(PxVec3(0.1f,0.2f,0.3f),PxQuat(-0.3f,PxVec3(1,0,0))));
        actor->attachShape(*shape); shape->release(); context.scene->addActor(*actor);
    }
    triangle->release(); convex->release(); field->release(); alternate->release();
    context.step();
    RepxResources resources;
    resources.registry=require(PxSerialization::createSerializationRegistry(*context.physics),"registry");
    resources.collection=require(PxCreateCollection(),"collection");
    std::vector<PxActor*> actors(context.scene->getNbActors(PxActorTypeFlag::eRIGID_STATIC));
    context.scene->getActors(PxActorTypeFlag::eRIGID_STATIC,actors.data(),static_cast<PxU32>(actors.size()));
    for(auto* actor:actors) resources.collection->add(*actor);
    PxSerialization::complete(*resources.collection,*resources.registry,nullptr,true);
    for(PxU32 i=0;i<resources.collection->getNbObjects();++i) {
        auto& object=resources.collection->getObject(i);
        resources.collection->addId(object,reinterpret_cast<std::uintptr_t>(&object));
    }
    dump_collection(*resources.collection,truth);
    PxDefaultFileOutputStream output(reference);
    if(!output.isValid() || !PxSerialization::serializeCollectionToXml(output,*resources.collection,*resources.registry)) throw std::runtime_error("Official reference XML failed");
}

int main(int argc,char** argv) {
    try {
        if(argc==5 && std::string(argv[1])=="--import") import_collection(argv[2],argv[3],argv[4]);
        else if(argc==4) produce(argv[1],argv[2],argv[3]);
        else throw std::runtime_error("Usage: pvd-repx-meshes capture reference.xml truth.json | --import input.xml truth.json roundtrip.xml");
        return 0;
    } catch(const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
