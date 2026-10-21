#include "../physx-features/feature_harness.h"
#include <cstdint>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <vector>
template<typename T> T* require(T* value, const char* label) { return feature_require(value, label); }
#include "../physx-sample/repx_probe.h"

static void vec(std::ostream& out, PxVec3 v) { out << '[' << v.x << ',' << v.y << ',' << v.z << ']'; }
static void quat(std::ostream& out, PxQuat q) { out << '[' << q.x << ',' << q.y << ',' << q.z << ',' << q.w << ']'; }
static void pose(std::ostream& out, PxTransform p) { out << "{\"p\":"; vec(out,p.p); out << ",\"q\":"; quat(out,p.q); out << '}'; }

static void shape_json(std::ostream& out, PxShape& shape, PxCollection& collection) {
    out << "{\"pose\":";
    pose(out, shape.getLocalPose());
    out << ",\"flags\":" << unsigned(shape.getFlags()) << ",\"materials\":[";
    std::vector<PxMaterial*> materials(shape.getNbMaterials());
    shape.getMaterials(materials.data(), static_cast<PxU32>(materials.size()));
    for (std::size_t i = 0; i < materials.size(); ++i) {
        if (i) out << ',';
        auto* m = materials[i];
        out << '[' << m->getStaticFriction() << ',' << m->getDynamicFriction() << ',' << m->getRestitution() << ',' << unsigned(m->getFlags()) << ',' << m->getFrictionCombineMode() << ',' << m->getRestitutionCombineMode() << ']';
    }
    out << "],\"type\":" << shape.getGeometryType();
    const PxGeometryHolder holder = shape.getGeometry();
    if (holder.getType() == PxGeometryType::eHEIGHTFIELD) {
        const auto& geometry = holder.heightField();
        auto* field = geometry.heightField;
        out << ",\"resource\":" << collection.getId(*field) << ",\"scale\":[" << geometry.rowScale << ',' << geometry.heightScale << ',' << geometry.columnScale << ']'
            << ",\"mesh_flags\":" << unsigned(geometry.heightFieldFlags)
            << ",\"rows\":" << field->getNbRows() << ",\"columns\":" << field->getNbColumns()
            << ",\"format\":" << field->getFormat() << ",\"field_flags\":" << unsigned(field->getFlags())
            << ",\"thickness\":" << field->getThickness() << ",\"edge_threshold\":" << field->getConvexEdgeThreshold() << ",\"samples\":[";
        std::vector<PxHeightFieldSample> samples(field->getNbRows()*field->getNbColumns());
        field->saveCells(samples.data(),static_cast<PxU32>(samples.size()*sizeof(PxHeightFieldSample)));
        for(std::size_t i=0;i<samples.size();++i) {
            if(i) out << ',';
            const auto* raw=reinterpret_cast<const PxU8*>(&samples[i]);
            out << '[' << samples[i].height << ',' << unsigned(raw[2]) << ',' << unsigned(raw[3]) << ']';
        }
        out << "],\"triangles\":[";
        bool first=true;
        for(PxU32 row=0;row+1<field->getNbRows();++row) for(PxU32 column=0;column+1<field->getNbColumns();++column) for(PxU32 half=0;half<2;++half) {
            const PxU32 index=(row*field->getNbColumns()+column)*2+half;
            if(field->getTriangleMaterialIndex(index)==PxHeightFieldMaterial::eHOLE) continue;
            PxTriangle triangle;
            PxMeshQuery::getTriangle(geometry,PxTransform(PxIdentity),index,triangle);
            if(!first) out << ',';
            first=false;
            out << '['; vec(out,triangle.verts[0]); out << ','; vec(out,triangle.verts[1]); out << ','; vec(out,triangle.verts[2]); out << ']';
        }
        out << "]}";
        return;
    }
    const bool convex = holder.getType() == PxGeometryType::eCONVEXMESH;
    const PxMeshScale scale = convex ? holder.convexMesh().scale : holder.triangleMesh().scale;
    out << ",\"scale\":"; vec(out,scale.scale);
    out << ",\"rotation\":"; quat(out,scale.rotation);
    out << ",\"mesh_flags\":" << (convex ? unsigned(holder.convexMesh().meshFlags) : unsigned(holder.triangleMesh().meshFlags));
    if (convex) out << ",\"max_margin\":" << holder.convexMesh().maxMargin;
    PxBase* mesh = convex ? static_cast<PxBase*>(holder.convexMesh().convexMesh) : static_cast<PxBase*>(holder.triangleMesh().triangleMesh);
    out << ",\"resource\":" << collection.getId(*mesh);
    const PxVec3* vertices = convex ? holder.convexMesh().convexMesh->getVertices() : holder.triangleMesh().triangleMesh->getVertices();
    const PxU32 count = convex ? holder.convexMesh().convexMesh->getNbVertices() : holder.triangleMesh().triangleMesh->getNbVertices();
    out << ",\"vertices\":[";
    for (PxU32 i=0; i<count; ++i) { if (i) out << ','; vec(out,vertices[i]); }
    out << "],\"faces\":[";
    if (convex) {
        auto* m = holder.convexMesh().convexMesh;
        for (PxU32 i=0; i<m->getNbPolygons(); ++i) {
            if (i) out << ',';
            PxHullPolygon polygon; m->getPolygonData(i,polygon);
            out << '[';
            for (PxU32 j=0; j<polygon.mNbVerts; ++j) { if(j) out << ','; out << unsigned(m->getIndexBuffer()[polygon.mIndexBase+j]); }
            out << ']';
        }
        PxReal mass; PxMat33 inertia; PxVec3 center;
        m->getMassInformation(mass,inertia,center);
        out << "],\"mesh_mass\":" << mass << ",\"mesh_center\":";
        vec(out,center);
        out << ",\"mesh_inertia\":["; vec(out,inertia.column0); out << ','; vec(out,inertia.column1); out << ','; vec(out,inertia.column2); out << ']';
    } else {
        auto* m = holder.triangleMesh().triangleMesh;
        for (PxU32 i=0; i<m->getNbTriangles(); ++i) {
            if (i) out << ',';
            out << '[';
            for (PxU32 j=0; j<3; ++j) { if(j) out << ','; out << (m->getTriangleMeshFlags() & PxTriangleMeshFlag::e16_BIT_INDICES ? static_cast<const PxU16*>(m->getTriangles())[i*3+j] : static_cast<const PxU32*>(m->getTriangles())[i*3+j]); }
            out << ']';
        }
        out << "],\"material_indices\":[";
        for (PxU32 i=0; i<m->getNbTriangles(); ++i) { if(i) out << ','; out << m->getTriangleMaterialIndex(i); }
        out << ']';
    }
    out << '}';
}

static void dump_collection(PxCollection& collection, const char* path) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot open mesh truth output");
    out << std::setprecision(9) << "{\"actors\":[";
    bool first=true;
    for (PxU32 i=0; i<collection.getNbObjects(); ++i) {
        auto* actor=collection.getObject(i).is<PxRigidActor>();
        if (!actor) continue;
        if (!first) out << ',';
        first=false;
        out << "{\"name\":\"" << actor->getName() << "\",\"pose\":";
        pose(out,actor->getGlobalPose());
        out << ",\"shapes\":[";
        std::vector<PxShape*> shapes(actor->getNbShapes());
        actor->getShapes(shapes.data(),static_cast<PxU32>(shapes.size()));
        for(std::size_t j=0;j<shapes.size();++j) { if(j) out << ','; shape_json(out,*shapes[j],collection); }
        out << "]}";
    }
    out << "]}\n";
    out.close();
    if(!out) throw std::runtime_error("Mesh truth write failed");
}

static void import_collection(const char* input, const char* truth, const char* roundtrip) {
    RepxImportEnvironment sdk;
    sdk.foundation=require(PxCreateFoundation(PX_FOUNDATION_VERSION,sdk.allocator,sdk.errors),"foundation");
    sdk.physics=require(PxCreatePhysics(PX_PHYSICS_VERSION,*sdk.foundation,PxTolerancesScale()),"physics");
    sdk.extensions=PxInitExtensions(*sdk.physics,nullptr);
    sdk.cooking=require(PxCreateCooking(PX_PHYSICS_VERSION,*sdk.foundation,PxCookingParams(sdk.physics->getTolerancesScale())),"cooking");
    sdk.strings=&PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry=require(PxSerialization::createSerializationRegistry(*sdk.physics),"registry");
    PxDefaultFileInputData stream(input);
    if(!stream.isValid()) throw std::runtime_error("Cannot open RepX input");
    PxSerialization::PxXmlMiscParameter parameters;
    resources.collection=require(PxSerialization::createCollectionFromXml(stream,*sdk.cooking,*resources.registry,nullptr,sdk.strings,&parameters),"RepX mesh collection");
    resources.own_objects=true;
    dump_collection(*resources.collection,truth);
    PxDefaultFileOutputStream output(roundtrip);
    if(!output.isValid() || !PxSerialization::serializeCollectionToXml(output,*resources.collection,*resources.registry,nullptr,nullptr,&parameters)) throw std::runtime_error("RepX mesh roundtrip failed");
}

static void produce(const char* capture, const char* reference, const char* truth) {
    FeatureContext context(capture);
    context.scene->setGravity(PxVec3(0));
    auto* alternate=require(context.physics->createMaterial(0.8f,0.6f,0.45f),"alternate material");
    alternate->setFrictionCombineMode(PxCombineMode::eMAX);
    const PxVec3 points[]={PxVec3(0,0,0),PxVec3(2,0,0),PxVec3(0,1,2),PxVec3(2,1,2),PxVec3(0,0,4),PxVec3(2,0,4)};
    const PxU32 triangles[]={0,2,1,1,2,3,2,4,3,3,4,5};
    const PxMaterialTableIndex materials[]={1,0,1,0};
    PxTriangleMeshDesc triangle_desc;
    triangle_desc.points.count=6;
    triangle_desc.points.stride=sizeof(PxVec3);
    triangle_desc.points.data=points;
    triangle_desc.triangles.count=4;
    triangle_desc.triangles.stride=3*sizeof(PxU32);
    triangle_desc.triangles.data=triangles;
    triangle_desc.materialIndices.data=materials;
    triangle_desc.materialIndices.stride=sizeof(PxMaterialTableIndex);
    PxDefaultMemoryOutputStream cooked;
    if(!context.cooking->cookTriangleMesh(triangle_desc,cooked)) throw std::runtime_error("Cannot cook triangle mesh");
    PxDefaultMemoryInputData triangle_input(cooked.getData(),cooked.getSize());
    auto* triangle=require(context.physics->createTriangleMesh(triangle_input),"triangle mesh");
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
    std::vector<PxHeightFieldSample> samples(20);
    for(PxU32 i=0;i<samples.size();++i) {
        samples[i].height=static_cast<PxI16>((int(i)*7)%19-9);
        samples[i].materialIndex0=i%2;
        samples[i].materialIndex1=(i+1)%2;
        if(i%2) samples[i].setTessFlag(); else samples[i].clearTessFlag();
    }
    samples[0].materialIndex0=PxHeightFieldMaterial::eHOLE;
    samples[7].materialIndex1=PxHeightFieldMaterial::eHOLE;
    PxHeightFieldDesc field_desc;
    field_desc.nbRows=4; field_desc.nbColumns=5;
    field_desc.samples.data=samples.data(); field_desc.samples.stride=sizeof(PxHeightFieldSample);
    field_desc.thickness=-1.7f; field_desc.convexEdgeThreshold=0.2f;
    field_desc.flags=PxHeightFieldFlag::eNO_BOUNDARY_EDGES;
    auto* field=require(context.cooking->createHeightField(field_desc,context.physics->getPhysicsInsertionCallback()),"height field");
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
