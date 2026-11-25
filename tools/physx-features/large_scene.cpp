#include "feature_harness.h"
#include <cmath>
#include <cstdint>
#include <fstream>
#include <iostream>
#include <vector>

PxTriangleMesh* make_grid(FeatureContext& sdk, unsigned cells, unsigned variant) {
    const unsigned stride = cells + 1;
    std::vector<PxVec3> vertices;
    std::vector<PxU32> indices;
    vertices.reserve(stride * stride);
    indices.reserve(6 * cells * cells);
    for (unsigned z = 0; z <= cells; ++z) {
        for (unsigned x = 0; x <= cells; ++x) {
            const float y = (float(cells) / 14.f) * std::sin(float(x) / cells * 5.f + variant * .1f)
                * std::cos(float(z) / cells * 5.f);
            vertices.emplace_back(float(x) - cells * .5f, y, float(z) - cells * .5f);
        }
    }
    for (unsigned z = 0; z < cells; ++z) {
        for (unsigned x = 0; x < cells; ++x) {
            const unsigned corner = z * stride + x;
            indices.insert(indices.end(), {
                corner, corner + stride, corner + 1,
                corner + 1, corner + stride, corner + stride + 1
            });
        }
    }
    PxTriangleMeshDesc desc;
    desc.points.count = static_cast<PxU32>(vertices.size());
    desc.points.data = vertices.data();
    desc.points.stride = sizeof(PxVec3);
    desc.triangles.count = static_cast<PxU32>(indices.size() / 3);
    desc.triangles.data = indices.data();
    desc.triangles.stride = 3 * sizeof(PxU32);
    PxDefaultMemoryOutputStream bytes;
    if (!sdk.cooking->cookTriangleMesh(desc, bytes)) throw std::runtime_error("cook failed");
    PxDefaultMemoryInputData data(bytes.getData(), bytes.getSize());
    return feature_require(sdk.physics->createTriangleMesh(data), "mesh");
}

unsigned parse_count(const std::string& text, unsigned maximum) {
    size_t end = 0;
    const auto value = std::stoul(text, &end);
    if (end != text.size() || text.front() == '-' || value > maximum)
        throw std::runtime_error("count outside supported range: " + text);
    return static_cast<unsigned>(value);
}

int main(int argc, char** argv) {
    try {
        const bool tcp = argc > 1 && std::string(argv[1]) == "--tcp";
        const int static_argc = tcp ? 8 : 6;
        if (argc != static_argc && argc != static_argc + 2)
            throw std::runtime_error("usage: pvd-large-scene (capture | --tcp host port) truth.json actors meshes grid_cells [frames moving_boxes]");
        const int truth_arg = tcp ? 4 : 2;
        const unsigned actor_count = parse_count(argv[truth_arg + 1], 60000);
        const unsigned mesh_count = parse_count(argv[truth_arg + 2], 1000);
        const unsigned cells = parse_count(argv[truth_arg + 3], 1000);
        const unsigned frames = argc == static_argc ? 3 : parse_count(argv[static_argc], 100000);
        const unsigned moving_count = argc == static_argc ? 0 : parse_count(argv[static_argc + 1], 4096);
        if (mesh_count > actor_count || (mesh_count == 0) != (cells == 0)
            || 2ull * cells * cells * mesh_count > 10000000 || !frames)
            throw std::runtime_error("invalid actor or mesh count");
        const auto connection = tcp
            ? FeatureConnection::socket(argv[2], parse_count(argv[3], 65535))
            : FeatureConnection::file(argv[1]);
        FeatureContext sdk(connection, PxPvdInstrumentationFlag::eDEBUG);
        std::vector<PxTriangleMesh*> meshes;
        for (unsigned i = 0; i < mesh_count; ++i) meshes.push_back(make_grid(sdk, cells, i));
        std::vector<PxActor*> batch;
        batch.reserve(actor_count + moving_count);
        const unsigned columns = actor_count ? static_cast<unsigned>(std::ceil(std::sqrt(double(actor_count)))) : 1;
        const unsigned rows = (actor_count + columns - 1) / columns;
        for (unsigned i = 0; i < actor_count; ++i) {
            PxTransform pose(PxVec3((float(i % columns) - (columns - 1) * .5f) * 2.5f, 0.f,
                                    (float(i / columns) - (rows - 1) * .5f) * 2.5f));
            PxBoxGeometry box(.85f, .75f, .85f);
            PxTriangleMeshGeometry tri;
            if (mesh_count) tri = PxTriangleMeshGeometry(meshes[i % mesh_count], PxMeshScale(2.f / cells));
            const PxGeometry& geometry = mesh_count
                ? static_cast<const PxGeometry&>(tri)
                : static_cast<const PxGeometry&>(box);
            auto* actor = feature_require(PxCreateStatic(*sdk.physics, pose, geometry, *sdk.material), "actor");
            batch.push_back(actor);
        }
        std::vector<PxRigidDynamic*> moving;
        std::vector<PxTransform> rest_poses;
        moving.reserve(moving_count);
        rest_poses.reserve(moving_count);
        for (unsigned i = 0; i < moving_count; ++i) {
            const float x = (float(i % 32) - 15.5f) * 2.5f;
            const float z = (float(i / 32) - 11.5f) * 2.5f;
            const PxTransform pose(i ? PxVec3(x, 3.f, z) : PxVec3(0.f, 9.f, 0.f));
            const PxBoxGeometry geometry = i ? PxBoxGeometry(.42f, .35f, .42f) : PxBoxGeometry(2.f, 1.f, 1.f);
            auto* actor = feature_require(PxCreateKinematic(*sdk.physics, pose, geometry, *sdk.material, 1.f), "moving box");
            moving.push_back(actor);
            rest_poses.push_back(pose);
            batch.push_back(actor);
        }
        if (!batch.empty()) sdk.scene->addActors(batch.data(), static_cast<PxU32>(batch.size()));
        for (unsigned frame = 0; frame < frames; ++frame) {
            for (unsigned i = 0; i < moving_count; ++i) {
                auto pose = rest_poses[i];
                const float phase = frame * (i ? .073f : .029f) + i * .39f;
                pose.p.x += (i ? .8f : 21.f) * std::sin(phase);
                pose.p.y += (i ? .65f : 2.f) * std::cos(phase * 1.7f);
                pose.q = PxQuat(phase, PxVec3(0.f, 1.f, 0.f));
                moving[i]->setKinematicTarget(pose);
            }
            sdk.step(moving_count ? 1.f / 30.f : 1.f / 60.f);
        }
        sdk.flush_and_require_connected();
        const PxActorTypeFlags types = PxActorTypeFlag::eRIGID_STATIC | PxActorTypeFlag::eRIGID_DYNAMIC;
        std::vector<PxActor*> actors(sdk.scene->getNbActors(types));
        if (actors.size() != batch.size()
            || (!actors.empty() && sdk.scene->getActors(types, actors.data(), static_cast<PxU32>(actors.size())) != actors.size()))
            throw std::runtime_error("SDK actor count mismatch");
        size_t shapes = 0, boxes = 0;
        uint64_t instanced_triangles = 0, unique_triangles = 0, vertices = 0;
        for (auto* actor : actors) {
            auto* rigid = actor->is<PxRigidActor>();
            PxShape* shape = nullptr;
            if (!rigid || rigid->getNbShapes() != 1 || rigid->getShapes(&shape, 1) != 1)
                throw std::runtime_error("SDK shape count mismatch");
            ++shapes;
            if (shape->getGeometryType() == PxGeometryType::eBOX) ++boxes;
            else {
                PxTriangleMeshGeometry geometry;
                if (!shape->getTriangleMeshGeometry(geometry)) throw std::runtime_error("unexpected geometry");
                instanced_triangles += geometry.triangleMesh->getNbTriangles();
            }
        }
        std::vector<PxTriangleMesh*> registered(sdk.physics->getNbTriangleMeshes());
        if (!registered.empty()) sdk.physics->getTriangleMeshes(registered.data(), static_cast<PxU32>(registered.size()));
        for (auto* mesh : registered) {
            unique_triangles += mesh->getNbTriangles();
            vertices += mesh->getNbVertices();
        }
        if (registered.size() != mesh_count || unique_triangles != 2ULL * cells * cells * mesh_count
            || instanced_triangles != 2ULL * cells * cells * actor_count
            || boxes != moving_count + (mesh_count ? 0u : actor_count))
            throw std::runtime_error("SDK geometry count mismatch");
        std::ofstream truth(argv[truth_arg]);
        truth << "{\"sdk\":\"" << feature_sdk_version() << "\",\"frames\":" << frames << ",\"static_actors\":"
              << sdk.scene->getNbActors(PxActorTypeFlag::eRIGID_STATIC) << ",\"dynamic_actors\":"
              << sdk.scene->getNbActors(PxActorTypeFlag::eRIGID_DYNAMIC) << ",\"shapes\":" << shapes
              << ",\"box_shapes\":" << boxes << ",\"unique_meshes\":" << registered.size()
              << ",\"unique_vertices\":" << vertices << ",\"unique_triangles\":" << unique_triangles
              << ",\"instanced_mesh_triangles\":" << instanced_triangles << "}\n";
        truth.close();
        if (!truth) throw std::runtime_error("write truth failed");
        for (auto* mesh : meshes) mesh->release();
        sdk.pvd->disconnect();
        return 0;
    } catch (const std::exception& e) {
        std::cerr << e.what() << '\n';
        return 1;
    }
}
