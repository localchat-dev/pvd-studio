use pvd_core::{
    Recording, Value,
    repx::{self, Selection},
};
use std::{path::PathBuf, sync::Arc};

fn capture() -> PathBuf {
    std::env::var_os("PVD_REPX_MESH_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-repx-meshes.pvdstream")
        })
}

#[test]
#[ignore = "requires official SDK tools/repx-meshes producer capture"]
fn mesh_dependencies_are_shared_and_corrupt_mesh_state_is_rejected() {
    let recording = Recording::from_path(capture()).unwrap();
    let state = recording.state_at(0).unwrap();
    let exported = repx::export(&state, Selection::Frame).unwrap();
    assert_eq!(
        (exported.actors, exported.shapes, exported.materials),
        (6, 6, 2)
    );
    assert_eq!(exported.xml.matches("<PxConvexMesh>").count(), 1);
    assert_eq!(exported.xml.matches("<PxBVH33TriangleMesh>").count(), 1);
    assert_eq!(exported.xml.matches("<PxHeightField>").count(), 1);
    let find = |name: &str| {
        state
            .objects
            .values()
            .find(|object| object.type_name.ends_with(&format!("::{name}")))
            .unwrap()
            .id
    };
    let triangle = find("PxTriangleMesh");
    let convex = find("PxConvexMesh");
    let geometry = find("PxTriangleMeshGeometry");
    let field = find("PxHeightField");
    let field_geometry = find("PxHeightFieldGeometry");
    let corruptions = [
        (
            triangle,
            "Triangles",
            Value::Array(vec![Value::U64(u32::MAX as u64); 12].into()),
        ),
        (triangle, "NbTriangles", Value::U64(5)),
        (
            triangle,
            "MaterialIndices",
            Value::Array(vec![Value::U64(2); 4].into()),
        ),
        (
            triangle,
            "MaterialIndices",
            Value::Array(vec![Value::U64(0); 3].into()),
        ),
        (
            triangle,
            "Points",
            Value::Array(vec![Value::Vec3([f32::NAN, 0.0, 0.0]); 6].into()),
        ),
        (
            convex,
            "PolygonIndexes",
            Value::Array(vec![Value::U64(0); 16].into()),
        ),
        (geometry, "Scale.Scale", Value::Vec3([0.0, 1.0, 1.0])),
        (geometry, "Scale.Rotation", Value::Quat([0.0; 4])),
        (geometry, "MeshFlags", Value::U64(4)),
        (field, "NbRows", Value::U64(u64::MAX)),
        (field, "Flags", Value::U64(2)),
        (field, "Format", Value::U64(2)),
        (field_geometry, "HeightScale", Value::F32(0.0)),
        (field_geometry, "HeightFieldFlags", Value::U64(1)),
    ];
    for (object, field, replacement) in corruptions {
        let mut corrupted = state.clone();
        corrupted
            .objects
            .get_mut(&object)
            .unwrap()
            .properties
            .insert(field.into(), replacement);
        assert!(
            repx::export(&corrupted, Selection::Frame).is_err(),
            "accepted corrupt {field}"
        );
    }
    let mut missing = state.clone();
    missing
        .objects
        .get_mut(&triangle)
        .unwrap()
        .properties
        .remove("MaterialIndices");
    assert!(repx::export(&missing, Selection::Frame).is_err());
    let mut stale = state.clone();
    stale.objects.get_mut(&triangle).unwrap().generation += 1;
    assert!(repx::export(&stale, Selection::Frame).is_err());
    let mut open_hull = state.clone();
    let Value::Array(polygons) = open_hull
        .objects
        .get_mut(&convex)
        .unwrap()
        .properties
        .get_mut("HullPolygons")
        .unwrap()
    else {
        unreachable!()
    };
    Arc::make_mut(polygons).pop();
    assert!(repx::export(&open_hull, Selection::Frame).is_err());
    for replacement in [Value::I64(32768), Value::String("0".into())] {
        let mut invalid = state.clone();
        let Value::Array(samples) = invalid
            .objects
            .get_mut(&field)
            .unwrap()
            .properties
            .get_mut("Samples")
            .unwrap()
        else {
            unreachable!()
        };
        let Value::Array(sample) = &mut Arc::make_mut(samples)[0] else {
            unreachable!()
        };
        Arc::make_mut(sample)[0] = replacement;
        assert!(repx::export(&invalid, Selection::Frame).is_err());
    }
}
