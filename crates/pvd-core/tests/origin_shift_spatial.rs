use pvd_core::{
    LiveRecording, Recording, SceneState, Value,
    debug_draw::Primitive,
    frame_sources::{FrameSection, FrameSourceKey},
    live::{HistoryConfig, ReplayCursor},
    queries,
};
use pvd_protocol::{Decoder, Event};
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Deserialize)]
struct Truth {
    sdk: String,
    warmup_frames: usize,
    frames: Vec<Frame>,
}

#[derive(Deserialize)]
struct Frame {
    frame: usize,
    shifted_scene: i32,
    shift: [f32; 3],
    worlds: Vec<World>,
}

#[derive(Deserialize)]
struct World {
    scene: u64,
    actor: u64,
    position: [f32; 3],
    ray_origin: [f32; 3],
    ray_hits: Vec<LocationHit>,
    sweep_origin: [f32; 3],
    sweep_hits: Vec<LocationHit>,
    overlap_origin: [f32; 3],
    overlap_hits: Vec<ReferenceHit>,
    batch_queries: Vec<BatchQuery>,
    contacts: Vec<Contact>,
    renderer: u64,
    packet_positions: Vec<[f32; 3]>,
    scene_renderer: u64,
    scene_packet_positions: Vec<[f32; 3]>,
    render_buffer_before: Vec<[f32; 3]>,
    render_buffer_after: Vec<[f32; 3]>,
}

#[derive(Deserialize)]
struct BatchQuery {
    index: usize,
    ray_origin: [f32; 3],
    ray_hits: Vec<LocationHit>,
    sweep_origin: [f32; 3],
    sweep_hits: Vec<LocationHit>,
    overlap_origin: [f32; 3],
    overlap_hits: Vec<ReferenceHit>,
}

#[derive(Deserialize)]
struct ReferenceHit {
    actor: u64,
    shape: u64,
}

#[derive(Deserialize)]
struct LocationHit {
    actor: u64,
    shape: u64,
    position: [f32; 3],
    normal: [f32; 3],
    distance: f32,
}

#[derive(Deserialize)]
struct Contact {
    shapes: [u64; 2],
    position: [f32; 3],
    normal: [f32; 3],
    separation: f32,
}

fn vector(actual: [f32; 3], expected: [f32; 3]) {
    assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
}

fn hits(actual: &[queries::Hit], expected: &[LocationHit]) {
    assert_eq!(actual.len(), expected.len());
    assert!(!actual.is_empty());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.actor.id, expected.actor);
        assert_eq!(actual.shape.id, expected.shape);
        vector(actual.position.unwrap(), expected.position);
        vector(actual.normal.unwrap(), expected.normal);
        assert_eq!(actual.distance.to_bits(), expected.distance.to_bits());
    }
}

fn references(actual: &[queries::Hit], expected: &[ReferenceHit]) {
    let mut actual: Vec<_> = actual
        .iter()
        .map(|hit| (hit.actor.id, hit.shape.id))
        .collect();
    let mut expected: Vec<_> = expected.iter().map(|hit| (hit.actor, hit.shape)).collect();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected);
    assert!(!actual.is_empty());
}

fn renderer(primitives: &[Primitive], expected_instance: u64, expected: &[[f32; 3]]) {
    assert_eq!(primitives.len(), 4);
    let mut positions = Vec::new();
    for primitive in primitives {
        let instance = match primitive {
            Primitive::Point {
                instance,
                position,
                color: 0xff00ff00,
            } => {
                positions.push(*position);
                *instance
            }
            Primitive::Line {
                instance,
                positions: line,
                colors,
            } => {
                assert_eq!(*colors, [0xff0000ff; 2]);
                positions.extend(line);
                *instance
            }
            Primitive::Triangle {
                instance,
                positions: triangle,
                colors,
            } => {
                assert_eq!(*colors, [0xffff0000; 3]);
                positions.extend(triangle);
                *instance
            }
            Primitive::Text {
                instance,
                position,
                text,
                size,
                color: 0xffffffff,
            } => {
                assert_eq!(text, "origin marker");
                assert_eq!(*size, 0.75);
                positions.push(*position);
                *instance
            }
            _ => panic!("unexpected SDK renderer packet: {primitive:?}"),
        };
        assert_eq!(instance, expected_instance);
    }
    assert_eq!(positions.len(), expected.len());
    for (actual, expected) in positions.into_iter().zip(expected) {
        vector(actual, *expected);
    }
}

fn check_source_directory(recording: &Recording, truth: &Truth) {
    let expected: [&[usize]; 2] = [&[0, 2, 3, 4, 5, 6, 7, 8, 10], &[1, 9]];
    assert_eq!(recording.frame_sources.sources().count(), 2);
    for (world, indices) in truth.frames[0].worlds.iter().zip(expected) {
        let key = FrameSourceKey::Captured {
            section: FrameSection::PhysX,
            source: world.scene,
        };
        let source = recording.frame_sources.source(key).unwrap();
        assert_eq!(source.len(), indices.len());
        assert_eq!(source.observed_frames(), indices.len());
        for (ordinal, index) in indices.iter().enumerate() {
            let frame = source.at_ordinal(ordinal).unwrap();
            assert_eq!((frame.index, frame.ordinal), (*index, ordinal));
            assert_eq!(frame.source_object.unwrap().id, world.scene);
            assert_eq!(frame.scene, frame.source_object);
            assert_eq!(recording.frame_info(*index).unwrap().scene_id, world.scene);
        }
    }
}

fn check_draw_window(state: &SceneState, index: usize, truth: &Truth) {
    let markers = match index {
        0 | 1 | 8 => &truth.frames[0..0],
        2..=7 => &truth.frames[index - 2..index - 1],
        9 => &truth.frames[0..6],
        10 => &truth.frames[6..7],
        _ => panic!("unexpected spatial fixture frame"),
    };
    for (world_index, world) in truth.frames[0].worlds.iter().enumerate() {
        for (owner, instance, scene_bound) in [
            (world.renderer, 0, false),
            (world.scene_renderer, world.scene, true),
        ] {
            let actual = state
                .debug_draw
                .get(&owner)
                .map_or(&[][..], |draw| draw.as_slice());
            assert_eq!(
                actual.len(),
                markers.len() * 4,
                "global frame {index}, owner {owner}"
            );
            for (packet, marker) in actual.chunks_exact(4).zip(markers) {
                let expected = &marker.worlds[world_index];
                assert_eq!(expected.scene, world.scene);
                assert_eq!(expected.renderer, world.renderer);
                assert_eq!(expected.scene_renderer, world.scene_renderer);
                let positions = if scene_bound {
                    &expected.scene_packet_positions
                } else {
                    &expected.packet_positions
                };
                renderer(packet, instance, positions);
            }
        }
    }
}

fn check(state: &SceneState, frame: &Frame) {
    let queries = queries::collect(state);
    assert!(queries.diagnostics.is_empty(), "{:?}", queries.diagnostics);
    assert_eq!(frame.worlds.len(), 2);
    for (index, world) in frame.worlds.iter().enumerate() {
        let Value::Transform { position, .. } =
            state.objects[&world.actor].properties["GlobalPose"]
        else {
            panic!("missing actor pose")
        };
        vector(position, world.position);
        let rays: Vec<_> = queries
            .raycasts
            .iter()
            .filter(|q| q.scene_id == world.scene && !q.batched)
            .collect();
        let sweeps: Vec<_> = queries
            .sweeps
            .iter()
            .filter(|q| q.scene_id == world.scene && !q.batched)
            .collect();
        let overlaps: Vec<_> = queries
            .overlaps
            .iter()
            .filter(|q| q.scene_id == world.scene && !q.batched)
            .collect();
        assert_eq!((rays.len(), sweeps.len(), overlaps.len()), (1, 1, 1));
        vector(rays[0].origin, world.ray_origin);
        vector(rays[0].direction, [0.0, -1.0, 0.0]);
        assert_eq!(rays[0].distance.to_bits(), 10.0_f32.to_bits());
        hits(&rays[0].hits, &world.ray_hits);
        vector(sweeps[0].geometry.position, world.sweep_origin);
        vector(sweeps[0].direction, [1.0, 0.0, 0.0]);
        hits(&sweeps[0].hits, &world.sweep_hits);
        vector(overlaps[0].geometry.position, world.overlap_origin);
        references(&overlaps[0].hits, &world.overlap_hits);
        assert_eq!(world.batch_queries.len(), 2);
        let rays: Vec<_> = queries
            .raycasts
            .iter()
            .filter(|q| q.scene_id == world.scene && q.batched)
            .collect();
        let sweeps: Vec<_> = queries
            .sweeps
            .iter()
            .filter(|q| q.scene_id == world.scene && q.batched)
            .collect();
        let overlaps: Vec<_> = queries
            .overlaps
            .iter()
            .filter(|q| q.scene_id == world.scene && q.batched)
            .collect();
        assert_eq!((rays.len(), sweeps.len(), overlaps.len()), (2, 2, 2));
        for expected in &world.batch_queries {
            let ray = rays.iter().find(|q| q.index == expected.index).unwrap();
            let sweep = sweeps.iter().find(|q| q.index == expected.index).unwrap();
            let overlap = overlaps.iter().find(|q| q.index == expected.index).unwrap();
            vector(ray.origin, expected.ray_origin);
            vector(ray.direction, [0.0, -1.0, 0.0]);
            assert_eq!(ray.distance.to_bits(), 10.0_f32.to_bits());
            hits(&ray.hits, &expected.ray_hits);
            vector(sweep.geometry.position, expected.sweep_origin);
            assert_eq!(sweep.geometry.rotation, [0.0, 0.0, 0.0, 1.0]);
            assert!(
                matches!(sweep.geometry.geometry, pvd_core::Geometry::Sphere { radius } if radius.to_bits() == 0.2_f32.to_bits())
            );
            vector(sweep.direction, [1.0, 0.0, 0.0]);
            assert_eq!(sweep.distance.to_bits(), 6.0_f32.to_bits());
            hits(&sweep.hits, &expected.sweep_hits);
            vector(overlap.geometry.position, expected.overlap_origin);
            assert_eq!(overlap.geometry.rotation, [0.0, 0.0, 0.0, 1.0]);
            assert!(
                matches!(overlap.geometry.geometry, pvd_core::Geometry::Sphere { radius } if radius.to_bits() == 0.2_f32.to_bits())
            );
            references(&overlap.hits, &expected.overlap_hits);
        }
        let contacts = &state.contacts[&world.scene];
        assert_eq!(contacts.len(), world.contacts.len());
        assert!(!contacts.is_empty());
        for contact in contacts.iter() {
            let expected = world
                .contacts
                .iter()
                .find(|expected| {
                    (contact.shapes == expected.shapes
                        || contact.shapes == [expected.shapes[1], expected.shapes[0]])
                        && contact.position.map(f32::to_bits) == expected.position.map(f32::to_bits)
                })
                .expect("contact position and shapes match the SDK callback");
            vector(contact.position, expected.position);
            assert_eq!(contact.separation.to_bits(), expected.separation.to_bits());
            let sign = if contact.shapes == expected.shapes {
                1.0
            } else {
                -1.0
            };
            assert_eq!(contact.normal, expected.normal.map(|value| value * sign));
        }
        renderer(
            &state.debug_draw[&world.renderer],
            0,
            &world.packet_positions,
        );
        renderer(
            &state.debug_draw[&world.scene_renderer],
            world.scene,
            &world.scene_packet_positions,
        );
        assert_eq!(
            world.render_buffer_before.len(),
            world.render_buffer_after.len()
        );
        assert!(!world.render_buffer_before.is_empty());
        for (before, after) in world
            .render_buffer_before
            .iter()
            .zip(&world.render_buffer_after)
        {
            let expected = if index as i32 == frame.shifted_scene {
                std::array::from_fn(|axis| before[axis] - frame.shift[axis])
            } else {
                *before
            };
            vector(*after, expected);
        }
    }
}

#[test]
#[ignore = "requires the official origin-shift spatial producer and independent query/contact/render truth"]
fn sdk_query_contact_and_scene_bound_draw_coordinates_survive_shifts_and_refresh() {
    let path = std::env::var_os("PVD_ORIGIN_SHIFT_SPATIAL_CAPTURE")
        .map(PathBuf::from)
        .expect("PVD_ORIGIN_SHIFT_SPATIAL_CAPTURE must name the generated SDK capture");
    let truth: Truth =
        serde_json::from_slice(&fs::read(path.with_extension("json")).unwrap()).unwrap();
    assert_eq!(truth.sdk, "3.4.2");
    assert_eq!(truth.warmup_frames, 2);
    assert_eq!(
        truth
            .frames
            .iter()
            .filter(|frame| frame.shifted_scene >= 0)
            .count(),
        4
    );
    assert_eq!(
        truth
            .frames
            .iter()
            .map(|frame| frame.frame)
            .collect::<Vec<_>>(),
        [2, 3, 4, 5, 6, 7, 10]
    );
    let bytes = fs::read(&path).unwrap();
    let mut decoder = Decoder::default();
    let groups = decoder.feed(&bytes).unwrap();
    decoder.finish().unwrap();
    let shifts: Vec<_> = groups
        .iter()
        .flat_map(|group| &group.events)
        .filter_map(|event| match event {
            Event::OriginShift { id, shift } => Some((*id, *shift)),
            _ => None,
        })
        .collect();
    assert_eq!(shifts.len(), 4);
    for ((scene, shift), frame) in shifts
        .iter()
        .zip(truth.frames.iter().filter(|frame| frame.shifted_scene >= 0))
    {
        assert_eq!(*scene, frame.worlds[frame.shifted_scene as usize].scene);
        vector(*shift, frame.shift);
    }
    let recording = Recording::from_path(&path).unwrap();
    assert_eq!(recording.frame_count(), 11);
    check_source_directory(&recording, &truth);
    recording.verify_replay().unwrap();
    let cache = path
        .parent()
        .unwrap()
        .join(format!("spatial-history-{}", std::process::id()));
    assert!(!cache.exists());
    let mut disk = LiveRecording::with_history(HistoryConfig {
        directory: cache.clone(),
        disk_budget_bytes: 32 << 20,
        segment_frames: 2,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut fragmented = LiveRecording::default();
    for chunk in bytes.chunks(997) {
        disk.feed(chunk).unwrap();
        fragmented.feed(chunk).unwrap();
    }
    let disk = disk.finish().unwrap();
    let fragmented = fragmented.finish().unwrap();
    assert!(disk.disk_usage().unwrap().segments > 1);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    check_source_directory(&disk, &truth);
    check_source_directory(&fragmented, &truth);
    for index in (0..11).chain((0..11).rev()).chain([9, 8, 10, 1, 0, 9, 2]) {
        let expected = recording.state_at(index).unwrap();
        check_draw_window(&expected, index, &truth);
        assert_eq!(fragmented.state_at(index).unwrap(), expected);
        assert_eq!(disk.state_at(index).unwrap(), expected);
        cursor
            .seek_scene(&disk, index, || false)
            .unwrap()
            .update
            .apply_owned(&mut displayed);
        assert_eq!(displayed, expected);
        check_draw_window(&displayed, index, &truth);
    }
    for index in (0..truth.frames.len())
        .chain((0..truth.frames.len()).rev())
        .chain([3, 0, 6, 1, 5])
    {
        let frame = &truth.frames[index];
        let expected = recording.state_at(frame.frame).unwrap();
        check(&expected, frame);
        assert_eq!(fragmented.state_at(frame.frame).unwrap(), expected);
        assert_eq!(disk.state_at(frame.frame).unwrap(), expected);
        cursor
            .seek_scene(&disk, frame.frame, || false)
            .unwrap()
            .update
            .apply_owned(&mut displayed);
        assert_eq!(displayed, expected);
        check(&displayed, frame);
    }
    let saved = path.parent().unwrap().join(format!(
        "retained-spatial-{}.pvdcapture",
        std::process::id()
    ));
    pvd_core::capture::write_retained_new(
        &disk,
        &saved,
        pvd_core::capture::CaptureRange { first: 3, last: 10 },
        &Default::default(),
        0,
        |_| true,
    )
    .unwrap();
    drop(cursor);
    drop(disk);
    assert!(fs::read_dir(&cache).unwrap().next().is_none());
    fs::remove_dir(cache).unwrap();
    let reopened = Recording::from_path(&saved).unwrap();
    for index in 3..=10 {
        let state = reopened.state_at(index).unwrap();
        check_draw_window(&state, index, &truth);
        assert_eq!(state, recording.state_at(index).unwrap());
    }
    for frame in truth.frames.iter().filter(|frame| frame.frame >= 3) {
        let state = reopened.state_at(frame.frame).unwrap();
        check(&state, frame);
        assert_eq!(state, recording.state_at(frame.frame).unwrap());
    }
    drop(reopened);
    fs::remove_file(saved).unwrap();
    println!(
        "4 SDK shifts, two Scenes, 6 ordinary and 12 batched queries, contact callback truth, Scene-bound/unbound retained point/line/triangle/text packets, SDK render-buffer translation, simulation refresh, disk forward/reverse/random and retained capture verified"
    );
}
