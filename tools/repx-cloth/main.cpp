#define main pvd_cloth_display_probe_main
#include "../physx-cloth/cloth.cpp"
#undef main

template<typename T> T* require(T* value, const char* name) { return feature_require(value, name); }
#include "../physx-sample/repx_probe.h"

static void dump_tethers(PxClothFabric& fabric, const std::string& path) {
    std::ofstream out(path);
    if (!out) throw std::runtime_error("Cannot open tether getter output");
    std::vector<PxU32> anchors(fabric.getNbTethers());
    std::vector<PxReal> lengths(fabric.getNbTethers());
    fabric.getTetherAnchors(anchors.data(), PxU32(anchors.size()));
    fabric.getTetherLengths(lengths.data(), PxU32(lengths.size()));
    out << std::setprecision(9) << "{\"sdk\":\"3.4.2\",\"particles\":" << fabric.getNbParticles()
        << ",\"tethers\":" << fabric.getNbTethers() << ",\"anchors\":";
    array(out, anchors); out << ",\"lengths\":"; array(out, lengths); out << "}\n";
    out.close();
    if (!out) throw std::runtime_error("Tether getter write failed");
}

static void produce(const char* capture, const std::string& directory) {
    FeatureContext context(capture);
    FabricInput input(context, 4, 5);
    auto* value = cloth(context, input, PxVec3(0), 0);
    PxClothCollisionSphere sphere;
    sphere.pos = PxVec3(100,100,100); sphere.radius = 0.25f;
    value->setCollisionSpheres(&sphere,1);
    context.step();
    dump_tethers(*input.fabric, directory + "/tether-truth.json");
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*context.physics), "registry");
    resources.collection = require(PxCreateCollection(), "collection");
    resources.collection->add(*value);
    PxSerialization::complete(*resources.collection, *resources.registry);
    for (PxU32 i=0; i<resources.collection->getNbObjects(); ++i) {
        auto& object = resources.collection->getObject(i);
        resources.collection->addId(object, id(&object));
    }
    const auto path = directory + "/reference.xml";
    PxDefaultFileOutputStream output(path.c_str());
    if (!output.isValid() || !PxSerialization::serializeCollectionToXml(output,*resources.collection,*resources.registry)) {
        throw std::runtime_error("Cloth reference XML serialization failed");
    }
    value->release(); input.fabric->release();
}

static void read_reference(const char* path, const char* truth) {
    RepxImportEnvironment sdk;
    sdk.foundation = require(PxCreateFoundation(PX_FOUNDATION_VERSION,sdk.allocator,sdk.errors), "foundation");
    sdk.physics = require(PxCreatePhysics(PX_PHYSICS_VERSION,*sdk.foundation,PxTolerancesScale()), "physics");
    sdk.extensions = PxInitExtensions(*sdk.physics,nullptr);
    if (!sdk.extensions) throw std::runtime_error("Extensions initialization failed");
    sdk.cooking = require(PxCreateCooking(PX_PHYSICS_VERSION,*sdk.foundation,PxCookingParams(sdk.physics->getTolerancesScale())), "cooking");
    sdk.strings = &PxStringTableExt::createStringTable(sdk.allocator);
    RepxResources resources;
    resources.registry = require(PxSerialization::createSerializationRegistry(*sdk.physics), "registry");
    PxDefaultFileInputData input(path);
    if (!input.isValid()) throw std::runtime_error("Cannot open cloth reference XML");
    resources.collection = require(PxSerialization::createCollectionFromXml(input,*sdk.cooking,*resources.registry,nullptr,sdk.strings), "collection");
    resources.own_objects = true;
    for (PxU32 i=0; i<resources.collection->getNbObjects(); ++i) {
        if (auto* fabric = resources.collection->getObject(i).is<PxClothFabric>()) {
            dump_tethers(*fabric,truth); return;
        }
    }
    throw std::runtime_error("Reference XML has no Fabric");
}

int main(int argc, char** argv) {
    try {
        if (argc == 4 && std::string(argv[1]) == "--import") read_reference(argv[2],argv[3]);
        else if (argc == 3) produce(argv[1],argv[2]);
        else throw std::runtime_error("Usage: pvd-repx-cloth capture.pvdstream output-directory | --import reference.xml truth.json");
        return 0;
    } catch (const std::exception& error) { std::cerr << error.what() << '\n'; return 1; }
}
