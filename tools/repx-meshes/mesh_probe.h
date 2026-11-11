#pragma once

static PxTriangleMesh* cook_probe_triangle(FeatureContext& context) {
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
    return triangle;
}

static PxHeightField* cook_probe_height_field(FeatureContext& context) {
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
    return field;
}

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
    out << "],\"type\":" << shape.getGeometryType()
        << ",\"exclusive\":" << (shape.isExclusive() ? "true" : "false");
    if (!shape.isExclusive()) out << ",\"shared_id\":" << collection.getId(shape);
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
    if (holder.getType() == PxGeometryType::ePLANE) {
        out << '}';
        return;
    }
    if (holder.getType() == PxGeometryType::eBOX) {
        out << ",\"half_extents\":"; vec(out, holder.box().halfExtents);
        out << '}';
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
        if (auto* dynamic = actor->is<PxRigidDynamic>()) {
            out << ",\"body_flags\":" << PxU32(dynamic->getRigidBodyFlags())
                << ",\"mass\":" << dynamic->getMass() << ",\"inertia\":";
            vec(out, dynamic->getMassSpaceInertiaTensor());
            out << ",\"cmass\":"; pose(out, dynamic->getCMassLocalPose());
            out << ",\"client_flags\":" << PxU32(dynamic->getClientBehaviorFlags());
        }
        out << ",\"actor_flags\":" << PxU32(actor->getActorFlags());
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
