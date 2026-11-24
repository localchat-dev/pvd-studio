use super::*;
use crate::Action;

const IDENTITY: [f32; 4] = [0., 0., 0., 1.];

fn create(state: &mut SceneState, id: u64, generation: u32, class: &str) {
    Action::Create(Object {
        id,
        generation,
        type_name: format!("physx3::{class}"),
        properties: BTreeMap::new(),
    })
    .apply(state);
}

fn set(state: &mut SceneState, id: u64, name: &str, value: Value) {
    Action::Set(id, name.into(), value).apply(state);
}

fn static_shape(state: &mut SceneState, id: u64, geometry: &str) {
    create(state, id, 0, "PxRigidStatic");
    create(state, id + 1, 0, "PxShape");
    create(state, id + 2, 0, geometry);
    set(state, id, "Shapes", Value::Ref(id + 1));
    set(state, id + 1, "Geometry", Value::Ref(id + 2));
}

fn pose(state: &mut SceneState, id: u64, name: &str, position: [f32; 3], rotation: [f32; 4]) {
    set(state, id, name, Value::Transform { position, rotation });
}

#[test]
fn transformed_static_bounds_keep_a_far_pivot_with_nearby_geometry() {
    let mut state = SceneState::default();
    static_shape(&mut state, 1, "PxBoxGeometry");
    set(&mut state, 3, "HalfExtents", Value::Vec3([150., 0.1, 0.1]));
    let half_turn = [0., 0., 1., 0.];
    let quarter_turn = [0., 0., 1., 1.];
    pose(&mut state, 1, "GlobalPose", [0., 100., 0.], half_turn);
    pose(&mut state, 2, "LocalPose", [0., 10., 0.], quarter_turn);
    let mut index = DisplaySpatialIndex::new(&state);
    let region = RenderRegion::new([0.; 3], 0.5).unwrap();
    assert_eq!(index.candidates(region).unwrap(), [1]);
    assert_eq!(
        state
            .render_scene_in_region(&mut MeshCache::default(), region)
            .objects
            .len(),
        1
    );

    pose(&mut state, 2, "LocalPose", [0., 10., 0.], IDENTITY);
    index.sync(&state, &[2]);
    assert!(index.candidates(region).unwrap().is_empty());

    pose(&mut state, 1, "GlobalPose", [0., 10., 0.], half_turn);
    index.sync(&state, &[1]);
    assert_eq!(index.candidates(region).unwrap(), [1]);
}

#[test]
fn mesh_resource_edits_and_generation_rebinding_update_static_candidates() {
    let mut state = SceneState::default();
    static_shape(&mut state, 1, "PxTriangleMeshGeometry");
    create(&mut state, 4, 0, "PxTriangleMesh");
    set(&mut state, 3, "TriangleMesh", Value::Ref(4));
    set(&mut state, 3, "Scale.Scale", Value::Vec3([1.; 3]));
    set(&mut state, 3, "Scale.Rotation", Value::Quat(IDENTITY));
    let write_mesh = |state: &mut SceneState, y| {
        let points = [[-1000., y, 0.], [1000., y, 0.], [0., y, 1.]];
        set(
            state,
            4,
            "Points",
            Value::Array(points.map(Value::Vec3).to_vec().into()),
        );
        set(
            state,
            4,
            "Triangles",
            Value::Array(vec![Value::U64(0), Value::U64(1), Value::U64(2)].into()),
        );
    };
    write_mesh(&mut state, 100.);
    let mut index = DisplaySpatialIndex::new(&state);
    let region = RenderRegion::new([0.; 3], 1.).unwrap();
    assert!(index.candidates(region).unwrap().is_empty());

    write_mesh(&mut state, 0.);
    index.sync(&state, &[4]);
    assert_eq!(index.candidates(region).unwrap(), [1]);
    assert_eq!(
        state
            .render_scene_in_region(&mut MeshCache::default(), region)
            .objects
            .len(),
        1
    );

    Action::Destroy(4).apply(&mut state);
    create(&mut state, 4, 1, "PxTriangleMesh");
    write_mesh(&mut state, 100.);
    index.sync(&state, &[4]);
    let uncertain = index.candidates(region).unwrap();
    assert_eq!(
        uncertain,
        [1],
        "an unresolved mesh is left to the exact tests"
    );
    assert!(
        state
            .render_actors_in_region(uncertain, &mut MeshCache::default(), region)
            .objects
            .is_empty()
    );

    set(&mut state, 3, "TriangleMesh", Value::Ref(4));
    index.sync(&state, &[3]);
    assert!(index.candidates(region).unwrap().is_empty());
}
