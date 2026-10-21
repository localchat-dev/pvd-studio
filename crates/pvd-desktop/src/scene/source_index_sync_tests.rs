use super::*;
use pvd_core::{Object, SceneState, live::SceneUpdate};
use std::collections::BTreeMap;

fn app() -> App {
    let mut app = App::new();
    app.insert_resource(Studio::default())
        .insert_resource(Viewport::default())
        .insert_resource(Assets::<Mesh>::default())
        .insert_resource(Assets::<StandardMaterial>::default())
        .add_systems(Update, sync_scene);
    app
}

fn object(id: u64, generation: u32, name: &str) -> Object {
    Object {
        id,
        generation,
        type_name: name.into(),
        properties: BTreeMap::new(),
    }
}

fn patch(app: &mut App, objects: impl IntoIterator<Item = Object>, full: bool) {
    let mut state = SceneState::default();
    state
        .objects
        .extend(objects.into_iter().map(|object| (object.id, object)));
    let mut update = SceneUpdate::full(&state);
    update.replace_all = full;
    let mut studio = app.world_mut().resource_mut::<Studio>();
    let revision = studio.revision;
    studio.scene_changes.record(&update, revision);
    update.apply_owned(&mut studio.state);
    studio.revision += 1;
}

fn compare(app: &App) {
    use crate::i18n::Language;
    let studio = app.world().resource::<Studio>();
    let viewport = app.world().resource::<Viewport>();
    assert!(viewport.source_index.complete);
    assert_eq!(viewport.revision, studio.revision);
    let expected = crate::point_clouds::PointClouds::collect(&studio.state);
    let actual = &viewport.point_clouds;
    assert_eq!(actual.point_count, expected.point_count);
    assert_eq!(actual.constraints, expected.constraints);
    assert_eq!(actual.clouds.len(), expected.clouds.len());
    for (a, e) in actual.clouds.iter().zip(&expected.clouds) {
        assert_eq!(a.id, e.id);
        assert!(a.kind == e.kind);
        assert_eq!(a.inactive, e.inactive);
        assert_eq!(a.points, e.points);
    }
    for language in [Language::English, Language::Chinese] {
        assert_eq!(
            actual
                .diagnostics
                .iter()
                .map(|d| d.text(language))
                .collect::<Vec<_>>(),
            expected
                .diagnostics
                .iter()
                .map(|d| d.text(language))
                .collect::<Vec<_>>()
        );
    }
    let mut queries = pvd_core::queries::collect(&studio.state);
    for detail in queries.diagnostics.drain(..) {
        let expected = format!("Scene query diagnostic: {detail}");
        assert!(
            viewport
                .diagnostics
                .iter()
                .any(|d| d.text(Language::English) == expected)
        );
    }
    assert_eq!(viewport.queries, queries);
}

#[test]
fn source_index_sync_full_batched_reuse_settings_gap_and_reset() {
    let mut app = app();
    patch(
        &mut app,
        [
            object(1, 0, "physx3::PxScene"),
            object(2, 0, "physx3::PxCloth"),
            object(3, 0, "physx3::PxParticleSystem"),
        ],
        true,
    );
    app.update();
    compare(&app);
    assert_eq!(
        app.world().resource::<Viewport>().source_index.scenes,
        [1].into()
    );
    patch(&mut app, [object(1, 1, "physx3::PxParticleFluid")], false);
    patch(
        &mut app,
        [
            object(1, 2, "physx3::PxCloth"),
            object(4, 0, "physx3::PxScene"),
        ],
        false,
    );
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.state.objects.remove(&4);
    }
    app.update();
    compare(&app);
    let viewport = app.world().resource::<Viewport>();
    assert!(viewport.source_index.scenes.is_empty());
    assert_eq!(viewport.source_index.cloths, [1, 2].into());
    assert_eq!(viewport.source_index.particles, [3].into());
    let revision = viewport.revision;
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.show_cloth_points = !studio.prefs.show_cloth_points;
        studio.prefs.meters_per_unit = 2.0;
    }
    app.update();
    compare(&app);
    assert_eq!(app.world().resource::<Viewport>().revision, revision);
    assert_eq!(
        app.world().resource::<Viewport>().source_index.cloths,
        [1, 2].into()
    );
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.state.objects.clear();
        studio
            .state
            .objects
            .insert(9, object(9, 0, "physx3::PxScene"));
        studio.revision += 2;
    }
    app.update();
    compare(&app);
    let viewport = app.world().resource::<Viewport>();
    assert_eq!(viewport.source_index.scenes, [9].into());
    assert!(viewport.source_index.cloths.is_empty());
    assert!(viewport.source_index.particles.is_empty());
    patch(&mut app, [], true);
    app.update();
    compare(&app);
    let viewport = app.world().resource::<Viewport>();
    assert!(viewport.source_index.scenes.is_empty());
    assert!(viewport.point_clouds.clouds.is_empty());
    assert!(viewport.queries.raycasts.is_empty());
}

#[test]
#[ignore = "Requires official SDK captures; headless CPU scene synchronization"]
fn source_index_sync_real_cloth_particles_and_queries_match_complete_collectors() {
    let root = std::path::PathBuf::from(std::env::var_os("PVD_SOURCE_INDEX_ROOT").unwrap());
    let mut app = app();
    for filename in [
        "physx-3.4-cloth.pvdstream",
        "physx-3.4-cloth-reused-fabric.pvdstream",
        "physx-3.4-particles.pvdstream",
        "physx-3.4-query.pvdstream",
        "physx-3.4-query-shapes.pvdstream",
    ] {
        let recording =
            pvd_core::Recording::from_path(root.join("captures").join(filename)).unwrap();
        for frame in [0, recording.frame_count() - 1, 0] {
            let state = recording.state_at(frame).unwrap();
            {
                let update = SceneUpdate::full(&state);
                let mut studio = app.world_mut().resource_mut::<Studio>();
                let revision = studio.revision;
                studio.scene_changes.record(&update, revision);
                update.apply_owned(&mut studio.state);
                studio.revision += 1;
            }
            app.update();
            compare(&app);
        }
        patch(&mut app, [], true);
        app.update();
        compare(&app);
    }
}
