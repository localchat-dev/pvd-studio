use pvd_core::{Geometry, Recording};
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Deserialize)]
struct GeometryTruth {
    frames: usize,
    shapes: usize,
    unique_meshes: usize,
    unique_vertices: usize,
    unique_triangles: usize,
    instanced_mesh_triangles: usize,
}

#[test]
#[ignore = "requires a PhysX 3.4.2 triangle mesh capture and its SDK getter counts"]
fn sdk_triangle_meshes_keep_all_geometry_across_seeks() {
    let path = PathBuf::from(std::env::var_os("PVD_SDK34_LARGE_MESH_CAPTURE").unwrap());
    let truth: GeometryTruth =
        serde_json::from_slice(&fs::read(path.with_extension("sdk.json")).unwrap()).unwrap();
    let recording = Recording::from_path(&path).unwrap();
    assert_eq!(recording.frame_count(), truth.frames);
    assert!(truth.frames > 0 && truth.unique_meshes > 0);
    for frame in [truth.frames - 1, 0, truth.frames / 2] {
        let state = recording.state_at(frame).unwrap();
        let mut vertices = 0;
        let mut indices = 0;
        let mut meshes = 0;
        for mesh in state
            .objects
            .values()
            .filter(|o| o.type_name.ends_with("::PxTriangleMesh"))
        {
            meshes += 1;
            vertices += mesh.properties["Points"].array_len().unwrap();
            indices += mesh.properties["Triangles"].array_len().unwrap();
        }
        assert_eq!(
            (meshes, vertices, indices),
            (
                truth.unique_meshes,
                truth.unique_vertices,
                3 * truth.unique_triangles
            )
        );
        let scene = state.render_scene();
        assert_eq!(scene.objects.len(), truth.shapes, "{:?}", scene.diagnostics);
        let submitted = scene
            .objects
            .iter()
            .map(|o| match &o.geometry {
                Geometry::TriangleMesh { indices, .. } => indices.len() / 3,
                _ => panic!("expected triangle mesh Shape"),
            })
            .sum::<usize>();
        assert_eq!(submitted, truth.instanced_mesh_triangles);
    }
}

#[test]
#[ignore = "requires a PhysX 3.4.2 mesh capture whose RepX resource XML fits the export budget"]
fn sdk_triangle_mesh_repx_writes_each_resource_once() {
    let path = PathBuf::from(std::env::var_os("PVD_SDK34_LARGE_MESH_CAPTURE").unwrap());
    let truth: GeometryTruth =
        serde_json::from_slice(&fs::read(path.with_extension("sdk.json")).unwrap()).unwrap();
    let recording = Recording::from_path(path).unwrap();
    let state = recording.state_at(0).unwrap();
    let exported = pvd_core::repx::export(&state, pvd_core::repx::Selection::Frame).unwrap();
    let mut vertices = 0;
    let mut indices = 0;
    let mut meshes = 0;
    for resource in exported.xml.split("<PxBVH33TriangleMesh>").skip(1) {
        let resource = resource.split_once("</PxBVH33TriangleMesh>").unwrap().0;
        let points = resource
            .split_once("<Points>")
            .unwrap()
            .1
            .split_once("</Points>")
            .unwrap()
            .0;
        let triangles = resource
            .split_once("<Triangles>")
            .unwrap()
            .1
            .split_once("</Triangles>")
            .unwrap()
            .0;
        vertices += points.split_ascii_whitespace().count();
        indices += triangles.split_ascii_whitespace().count();
        meshes += 1;
    }
    assert_eq!(
        (meshes, vertices, indices),
        (
            truth.unique_meshes,
            3 * truth.unique_vertices,
            3 * truth.unique_triangles
        )
    );
}
