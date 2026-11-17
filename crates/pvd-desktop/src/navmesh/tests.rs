use super::*;

fn loaded(binding: OverlayBinding) -> LoadedNavMesh {
    LoadedNavMesh {
        mesh: Arc::new(NavMesh {
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
            indices: vec![0, 1, 2],
            edges: vec![[[0.0; 3], [1.0, 0.0, 0.0]]],
            tile_count: 1,
            polygon_count: 1,
            bounds_min: [0.0; 3],
            bounds_max: [1.0, 0.0, 1.0],
        }),
        path: "test.bin".into(),
        binding,
    }
}
fn scene(id: u64) -> OverlayBinding {
    OverlayBinding::Room {
        epoch: 0,
        room: FrameObject { id, generation: 0 },
    }
}
fn pending(
    binding: OverlayBinding,
) -> (
    PendingImport,
    mpsc::SyncSender<Result<LoadedNavMesh, Message>>,
) {
    let (sender, receiver) = mpsc::sync_channel(1);
    (
        PendingImport {
            binding,
            receiver: Mutex::new(receiver),
            cancelled: Arc::new(AtomicBool::new(false)),
        },
        sender,
    )
}

#[test]
fn scenes_keep_independent_geometry_settings_and_replacement() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(1))).unwrap();
    overlay.select_binding(scene(1));
    let first_mesh = overlay.selected().unwrap().loaded.mesh.clone();
    overlay.selected_mut().unwrap().settings.alignment.offset = [3.0, 4.0, 5.0];
    overlay.selected_mut().unwrap().settings.alignment.flip_x = false;
    overlay.selected_mut().unwrap().settings.opacity = 0.8;
    let first_settings = overlay.selected().unwrap().settings;
    overlay.publish(loaded(scene(2))).unwrap();
    overlay.select_binding(scene(2));
    assert_eq!(
        overlay.selected().unwrap().settings,
        OverlaySettings::default()
    );
    overlay.selected_mut().unwrap().settings.opacity = 0.2;
    let mut replacement = loaded(scene(2));
    replacement.path = "second.bin".into();
    overlay.publish(replacement).unwrap();
    assert_eq!(overlay.selected().unwrap().settings.opacity, 0.2);
    assert_eq!(
        overlay.selected().unwrap().loaded.path,
        PathBuf::from("second.bin")
    );
    overlay.select_binding(scene(1));
    assert_eq!(overlay.selected().unwrap().settings, first_settings);
    assert!(Arc::ptr_eq(
        &overlay.selected().unwrap().loaded.mesh,
        &first_mesh
    ));
    overlay.unload();
    assert!(overlay.entry(scene(1)).is_none());
    assert!(overlay.entry(scene(2)).is_some());
}

#[test]
fn failed_replacement_preserves_all_scene_geometry_settings_and_revisions() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(1))).unwrap();
    overlay.publish(loaded(scene(2))).unwrap();
    overlay.select_binding(scene(1));
    overlay.selected_mut().unwrap().settings.alignment.offset = [3.0, 4.0, 5.0];
    let geometry = overlay.selected().unwrap().loaded.mesh.clone();
    let settings = overlay.selected().unwrap().settings;
    let revision = overlay.selected().unwrap().revision;
    let (pending, sender) = pending(scene(1));
    overlay.pending = Some(pending);
    sender
        .send(Err(Message::new("bad tile", "bad tile")))
        .unwrap();
    overlay.poll_import();
    assert!(Arc::ptr_eq(
        &overlay.selected().unwrap().loaded.mesh,
        &geometry
    ));
    assert_eq!(overlay.selected().unwrap().revision, revision);
    assert_eq!(overlay.selected().unwrap().settings, settings);
    assert_eq!(overlay.entries.len(), 2);
    assert!(!overlay.is_loading());
    assert!(overlay.message.is_some());
}

#[test]
fn cancel_discards_late_success_without_removing_other_scenes() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(1))).unwrap();
    overlay.publish(loaded(scene(2))).unwrap();
    let (pending, sender) = pending(scene(1));
    let cancellation = pending.cancelled.clone();
    overlay.pending = Some(pending);
    overlay.cancel_import();
    assert!(cancellation.load(Ordering::Acquire));
    assert!(sender.send(Ok(loaded(scene(1)))).is_err());
    overlay.poll_import();
    assert_eq!(overlay.entries.len(), 2);
}

#[test]
fn unloading_a_scene_does_not_cancel_another_scene_import() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(1))).unwrap();
    let (pending, sender) = pending(scene(2));
    overlay.pending = Some(pending);
    overlay.select_binding(scene(1));
    overlay.unload();
    sender.send(Ok(loaded(scene(2)))).unwrap();
    overlay.poll_import();
    assert!(overlay.entry(scene(1)).is_none());
    assert!(overlay.entry(scene(2)).is_some());
}

#[test]
fn unloaded_scene_cannot_be_resurrected_by_an_already_completed_import() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(1))).unwrap();
    overlay.select_binding(scene(1));
    overlay.focus_requested = Some(scene(1));
    let (pending, sender) = pending(scene(1));
    overlay.pending = Some(pending);
    sender.send(Ok(loaded(scene(1)))).unwrap();
    overlay.unload();
    overlay.poll_import();
    assert!(overlay.entries.is_empty());
    assert!(overlay.focus_requested.is_none());
}

#[test]
fn completed_import_keeps_target_when_selection_changes() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(2))).unwrap();
    let geometry = overlay.entry(scene(2)).unwrap().loaded.mesh.clone();
    let (pending, sender) = pending(scene(1));
    overlay.pending = Some(pending);
    overlay.select_binding(scene(2));
    sender.send(Ok(loaded(scene(1)))).unwrap();
    overlay.poll_import();
    assert_eq!(overlay.selected_binding, scene(2));
    assert!(overlay.entry(scene(1)).is_some());
    assert!(Arc::ptr_eq(
        &overlay.entry(scene(2)).unwrap().loaded.mesh,
        &geometry
    ));
}

#[test]
fn starting_another_scene_import_keeps_the_existing_worker_and_target() {
    let mut overlay = NavMeshOverlay::default();
    let (pending, sender) = pending(scene(1));
    let cancellation = pending.cancelled.clone();
    overlay.pending = Some(pending);
    overlay.select_binding(scene(2));
    overlay.request_selected_import("other.bin".into(), &Studio::default(), None);
    assert!(overlay.message.is_some());
    assert!(!cancellation.load(Ordering::Acquire));
    sender.send(Ok(loaded(scene(1)))).unwrap();
    overlay.poll_import();
    assert!(overlay.entry(scene(1)).is_some());
    assert!(overlay.entry(scene(2)).is_none());
    assert_eq!(overlay.selected_binding, scene(2));
}

#[test]
fn scene_identity_includes_epoch_and_generation() {
    let bound = FrameObject {
        id: 23,
        generation: 4,
    };
    let binding = OverlayBinding::Room {
        epoch: 7,
        room: bound,
    };
    assert!(binding.matches(7, Some(bound)));
    assert!(!binding.matches(8, Some(bound)));
    assert!(!binding.matches(
        7,
        Some(FrameObject {
            generation: 5,
            ..bound
        })
    ));
    assert!(!binding.matches(7, Some(FrameObject { id: 24, ..bound })));
    assert!(!binding.matches(7, None));
    assert!(OverlayBinding::Global.matches(999, None));
}

#[test]
fn scene_selection_follows_display_and_capture_change_releases_old_entries() {
    let mut studio = Studio::default();
    let mut overlay = NavMeshOverlay::default();
    overlay.sync_selection(&studio);
    let first_epoch = studio.room_session_epoch();
    let room = FrameObject {
        id: 31,
        generation: 2,
    };
    studio.room_view.displayed = Some(room);
    overlay.sync_selection(&studio);
    let binding = OverlayBinding::Room {
        epoch: first_epoch,
        room,
    };
    assert_eq!(overlay.selected_binding, binding);
    overlay.publish(loaded(binding)).unwrap();
    overlay.publish(loaded(OverlayBinding::Global)).unwrap();
    let (pending, sender) = pending(binding);
    overlay.pending = Some(pending);
    studio.close();
    overlay.sync_selection(&studio);
    assert!(overlay.entry(binding).is_none());
    assert!(overlay.entry(OverlayBinding::Global).is_some());
    assert!(!overlay.is_loading());
    assert!(sender.send(Ok(loaded(binding))).is_err());
}

#[test]
fn capture_change_resets_manually_selected_target_in_all_scenes_view() {
    let mut studio = Studio::default();
    let mut overlay = NavMeshOverlay::default();
    overlay.sync_selection(&studio);
    overlay.select_binding(scene(1));
    studio.close();
    overlay.sync_selection(&studio);
    assert_eq!(overlay.selected_binding, OverlayBinding::Global);
}

#[test]
fn layer_budget_rejects_new_scenes_but_allows_replacement() {
    let mut overlay = NavMeshOverlay::default();
    for id in 0..MAX_OVERLAYS as u64 {
        overlay.publish(loaded(scene(id))).unwrap();
    }
    assert!(overlay.publish(loaded(scene(MAX_OVERLAYS as u64))).is_err());
    assert_eq!(overlay.entries.len(), MAX_OVERLAYS);
    overlay.publish(loaded(scene(0))).unwrap();
    assert_eq!(overlay.entries.len(), MAX_OVERLAYS);
}

#[test]
fn queued_import_waits_for_initial_seek_and_scene_commit() {
    let mut studio = Studio::default();
    studio.initialized = true;
    studio.seeking = Some(0);
    let room = FrameObject {
        id: 31,
        generation: 2,
    };
    studio.room_view.requested = Some(room);
    let mut overlay = NavMeshOverlay::default();
    overlay.queue_import("scene-navmesh.bin".into());
    assert!(studio.job.is_none());
    assert!(overlay.take_ready_startup_path(&studio).is_none());
    assert!(overlay.is_busy());
    studio.seeking = None;
    assert!(overlay.take_ready_startup_path(&studio).is_none());
    studio.room_view.displayed = Some(room);
    assert_eq!(
        overlay.take_ready_startup_path(&studio),
        Some("scene-navmesh.bin".into())
    );
    assert_eq!(
        OverlayBinding::current(&studio),
        OverlayBinding::Room {
            epoch: studio.room_session_epoch(),
            room
        }
    );
    assert!(overlay.take_ready_startup_path(&studio).is_none());
}

#[test]
fn geometry_budget_accounts_for_other_scenes_and_releases_replaced_allocation() {
    let mut overlay = NavMeshOverlay::default();
    overlay.publish(loaded(scene(1))).unwrap();
    overlay.publish(loaded(scene(2))).unwrap();
    let one = geometry_bytes(&overlay.entry(scene(1)).unwrap().loaded.mesh);
    let replacement_limit = MAX_OVERLAY_GEOMETRY_BYTES - one;
    assert!(overlay.check_capacity(scene(1), replacement_limit).is_ok());
    assert!(
        overlay
            .check_capacity(scene(1), replacement_limit + 1)
            .is_err()
    );
    assert!(overlay.check_capacity(scene(3), replacement_limit).is_err());
    assert!(overlay.check_capacity(scene(3), usize::MAX).is_err());
    assert_eq!(overlay.entries.len(), 2);
}

#[test]
fn alignment_is_applied_before_shared_axis_unit_and_handedness_conversion() {
    use crate::coordinates::{Coordinates, Handedness, UpAxis};

    let alignment = Alignment {
        offset: [10.0, 20.0, 30.0],
        rotation_degrees: [0.0, 90.0, 0.0],
        scale: 2.0,
        flip_x: true,
    };
    // [1,2,3] -> mirror [-1,2,3] -> scale [-2,4,6]
    // -> Y rotation [6,4,2] -> translation [16,24,32].
    let source = alignment.point(Vec3::new(1.0, 2.0, 3.0));
    assert!((source - Vec3::new(16.0, 24.0, 32.0)).length() < 1e-4);
    let coordinates = Coordinates::new(UpAxis::Z, Handedness::Left, 0.01);
    let expected = coordinates.point(Vec3::new(16.0, 24.0, 32.0));
    assert!((coordinates.point(source) - expected).length() < 1e-5);
    assert!(alignment.mirrored());
    assert!(Alignment::default().mirrored());
}

#[test]
fn invalid_alignment_values_do_not_reach_render_geometry() {
    let alignment = Alignment {
        offset: [f32::NAN, f32::INFINITY, f32::MAX],
        rotation_degrees: [f32::NEG_INFINITY, f32::NAN, 10000.0],
        scale: f32::NAN,
        flip_x: false,
    };
    let safe = alignment.sanitized();
    assert_eq!(safe.offset, [0.0, 0.0, 1e9]);
    assert_eq!(safe.rotation_degrees, [0.0, 0.0, 360.0]);
    assert_eq!(safe.scale, 1.0);
    assert!(alignment.point(Vec3::ONE).is_finite());
}

#[test]
fn oversized_and_cancelled_imports_do_not_read_the_file() {
    struct MustNotRead;
    impl Read for MustNotRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("file must not be read");
        }
    }
    let error = read_bounded(
        &mut MustNotRead,
        pvd_core::navmesh::MAX_FILE_BYTES + 1,
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let error = read_bounded(&mut MustNotRead, 10, &AtomicBool::new(true)).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

#[test]
fn cancelled_chunked_read_stops_before_the_next_read() {
    struct CancelAfterRead<'a>(&'a AtomicBool, usize);
    impl Read for CancelAfterRead<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.1 += 1;
            bytes[0] = 1;
            self.0.store(true, Ordering::Release);
            Ok(1)
        }
    }
    let cancelled = AtomicBool::new(false);
    let mut reader = CancelAfterRead(&cancelled, 0);
    let error = read_bounded(&mut reader, 0, &cancelled).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(reader.1, 1);
}

#[test]
fn real_file_import_replaces_only_on_success_and_releases_tracked_workers() {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    struct Directory(PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("valid.bin"));
            let _ = std::fs::remove_file(self.0.join("invalid.bin"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    fn wait_for_import(overlay: &mut NavMeshOverlay, studio: &Studio) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            overlay.poll_import();
            if !overlay.is_loading() && studio.background_jobs.active_count() == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "NavMesh worker did not finish");
            std::thread::park_timeout(Duration::from_millis(1));
        }
    }

    let directory = Directory(std::env::temp_dir().join(format!(
        "pvd-navmesh-import-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    )));
    std::fs::create_dir(&directory.0).unwrap();
    let valid = directory.0.join("valid.bin");
    let invalid = directory.0.join("invalid.bin");
    std::fs::write(
        &valid,
        include_bytes!("../../../pvd-core/src/navmesh/fixtures/recast-0.43.1.bin"),
    )
    .unwrap();
    std::fs::write(&invalid, b"not a Detour NavMesh").unwrap();
    let mut studio = Studio::default();
    let room = FrameObject {
        id: 90,
        generation: 3,
    };
    studio.room_view.displayed = Some(room);
    let mut overlay = NavMeshOverlay {
        picker_binding: Some(OverlayBinding::Global),
        ..default()
    };
    overlay.request_import(valid.clone(), &studio, None);
    assert!(overlay.picker_binding.is_none());
    assert!(overlay.is_loading());
    wait_for_import(&mut overlay, &studio);
    let imported = &overlay
        .selected()
        .expect("valid upstream file should load")
        .loaded;
    assert_eq!(imported.path, valid);
    assert_eq!(
        (
            imported.mesh.tile_count,
            imported.mesh.polygon_count,
            imported.mesh.indices.len() / 3
        ),
        (1, 3, 6)
    );
    assert_eq!(imported.mesh.bounds_min, [-4.5, 0.2, -4.5]);
    assert_eq!(
        imported.binding,
        OverlayBinding::Room {
            epoch: studio.room_session_epoch(),
            room
        }
    );
    let geometry = imported.mesh.clone();
    let revision = overlay.selected().unwrap().revision;

    overlay.request_import(invalid, &studio, None);
    wait_for_import(&mut overlay, &studio);
    assert!(overlay.message.is_some());
    assert!(Arc::ptr_eq(
        &overlay.selected().unwrap().loaded.mesh,
        &geometry
    ));
    assert_eq!(overlay.selected().unwrap().loaded.path, valid);
    assert_eq!(overlay.selected().unwrap().revision, revision);
    assert_eq!(studio.background_jobs.active_count(), 0);
}
