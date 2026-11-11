use pvd_core::{
    Recording, Value,
    analysis::MemoryState,
    debug_draw::Primitive,
    native::{
        self,
        export::{rewrite, trim_scene_frames},
    },
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[path = "support/native_initial.rs"]
mod native_initial;
#[path = "support/native_initial_analysis_middle.rs"]
mod native_initial_analysis_middle;
#[path = "support/native_memory_interval.rs"]
mod native_memory_interval;
#[path = "support/native_render_context.rs"]
mod native_render_context;
#[path = "support/joint_limits.rs"]
mod sdk_joint_limits;

fn root() -> PathBuf {
    std::env::var_os("PVD_NATIVE_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native"))
}
fn pair(stem: &str, suffix: &str) -> PathBuf {
    root().join(format!("{stem}-paired{suffix}"))
}
fn limit(suffix: &str) -> PathBuf {
    root()
        .join("limits")
        .join(format!("joint-limits-live{suffix}"))
}

fn mapped(value: &Value, ids: &BTreeMap<u64, u64>) -> Value {
    match value {
        Value::Ref(id) => Value::Ref(*ids.get(id).unwrap_or(id)),
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|v| mapped(v, ids))
                .collect::<Vec<_>>()
                .into(),
        ),
        _ => value.clone(),
    }
}

fn mapped_primitive(value: &Primitive, ids: &BTreeMap<u64, u64>) -> Primitive {
    let mut value = value.clone();
    let instance = match &mut value {
        Primitive::Point { instance, .. }
        | Primitive::Line { instance, .. }
        | Primitive::Triangle { instance, .. }
        | Primitive::Text { instance, .. }
        | Primitive::JointFrames { instance, .. }
        | Primitive::LinearLimit { instance, .. }
        | Primitive::AngularLimit { instance, .. }
        | Primitive::LimitCone { instance, .. }
        | Primitive::DoubleCone { instance, .. } => instance,
    };
    *instance = *ids.get(instance).unwrap_or(instance);
    value
}

fn mapped_memory(mut state: MemoryState, ids: &BTreeMap<u64, u64>) -> MemoryState {
    state.live = state
        .live
        .into_iter()
        .map(|((buffer, address), allocation)| ((ids[&buffer], address), allocation))
        .collect();
    state
}

fn analysis(recording: &Recording) -> &pvd_core::analysis::Analysis {
    recording
        .analysis
        .legacy()
        .expect("in-memory fixtures retain complete analysis")
}

fn check_frame(
    native: &Recording,
    raw: &Recording,
    frame: usize,
    ids: &BTreeMap<u64, u64>,
    initial: &native_initial::NativeInitialValues,
) -> usize {
    let properties = check_frame_state(native, raw, frame, ids, initial);
    assert_eq!(
        mapped_memory(analysis(native).memory.state_at_frame(frame), ids),
        analysis(raw).memory.state_at_frame(frame)
    );
    properties
}

fn check_frame_state(
    native: &Recording,
    raw: &Recording,
    frame: usize,
    ids: &BTreeMap<u64, u64>,
    initial: &native_initial::NativeInitialValues,
) -> usize {
    let state = native.state_at(frame).unwrap();
    let expected = raw.state_at(frame).unwrap();
    assert_eq!(native.frames[frame].timestamp, raw.frames[frame].timestamp);
    assert_eq!(
        ids[&native.frames[frame].scene_id],
        raw.frames[frame].scene_id
    );
    let mut properties = 0;
    for object in state.objects.values() {
        if matches!(
            object.type_name.as_str(),
            "_debugger::MemoryEventTotals" | "_debugger_::UserCustomedEvent"
        ) {
            continue;
        }
        let other = &expected.objects[&ids[&object.id]];
        assert_eq!(
            object.type_name.rsplit("::").next(),
            other.type_name.rsplit("::").next()
        );
        for (key, expected) in &other.properties {
            if key == "events" || matches!(expected, Value::Bytes(_)) {
                continue;
            }
            let actual = object.properties.get(key).unwrap_or_else(|| {
                panic!(
                    "missing native field: frame {frame} {}.{key}",
                    object.type_name
                )
            });
            assert_eq!(
                &mapped(actual, ids),
                expected,
                "SDK field: frame {frame} {}.{key}",
                object.type_name
            );
        }
        for (key, value) in &object.properties {
            if key == "events" || matches!(value, Value::Bytes(_)) {
                continue;
            }
            if let Some(expected) = other.properties.get(key) {
                assert_eq!(
                    &mapped(value, ids),
                    expected,
                    "frame {frame} {} {key}",
                    object.type_name
                );
            } else {
                initial.assert_value(object.id, &object.type_name, key, value);
            }
            properties += 1;
        }
    }
    let draw: BTreeMap<_, _> = state
        .debug_draw
        .iter()
        .map(|(id, batch)| {
            (
                ids[id],
                batch
                    .iter()
                    .map(|v| mapped_primitive(v, ids))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        draw,
        expected
            .debug_draw
            .iter()
            .map(|(id, batch)| (*id, batch.to_vec()))
            .collect()
    );
    for (scene, contacts) in &state.contacts {
        let mut actual = contacts.to_vec();
        for contact in &mut actual {
            contact.shapes = contact.shapes.map(|id| *ids.get(&id).unwrap_or(&id));
        }
        assert_eq!(actual, *expected.contacts[&ids[scene]]);
    }
    let render = state.render_scene();
    let raw_render = expected.render_scene();
    assert_eq!(render.objects.len(), raw_render.objects.len());
    for object in render.objects {
        let other = raw_render
            .objects
            .iter()
            .find(|v| v.shape_id == ids[&object.shape_id])
            .unwrap();
        assert_eq!(object.geometry, other.geometry);
        assert_eq!(object.position, other.position);
        assert_eq!(object.rotation, other.rotation);
        assert_eq!(object.dynamic, other.dynamic);
        assert_eq!(object.sleeping, other.sleeping);
    }
    properties
}

fn check_analysis(
    native: &Recording,
    original: &Recording,
    raw: &Recording,
    last: usize,
    ids: &BTreeMap<u64, u64>,
) {
    let a = analysis(native);
    let b = analysis(raw);
    let tuples = |analysis: &pvd_core::analysis::Analysis, mapped: bool| {
        analysis
            .spans
            .iter()
            .map(|s| {
                (
                    if mapped { ids[&s.zone] } else { s.zone },
                    s.event,
                    s.thread,
                    s.context,
                    s.start,
                    s.end,
                    s.received_frame,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(tuples(a, true), tuples(b, false));
    assert_eq!(
        a.spans.len(),
        analysis(original)
            .spans
            .iter()
            .filter(|s| s.received_frame <= last)
            .count()
    );
    assert_eq!(a.memory.events().len(), b.memory.events().len());
    for (event, expected) in a.memory.events().iter().zip(b.memory.events()) {
        assert_eq!(ids[&event.buffer], expected.buffer);
        assert_eq!(
            (event.address, event.received_frame, &event.kind),
            (expected.address, expected.received_frame, &expected.kind)
        );
    }
    assert_eq!(
        a.memory.events().len(),
        analysis(original)
            .memory
            .events()
            .iter()
            .filter(|e| e.received_frame <= last)
            .count()
    );
    let pending = a
        .open_event_starts()
        .map(|(&(zone, event, thread, context), ticks)| {
            ((ids[&zone], event, thread, context), ticks)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        pending,
        b.open_event_starts()
            .map(|(key, ticks)| (*key, ticks))
            .collect()
    );
    assert_eq!(a.unmatched_stops, b.unmatched_stops);
    assert_eq!(
        a.unmatched_stop_events
            .iter()
            .map(|(&(zone, event, thread, context), n)| ((ids[&zone], event, thread, context), *n))
            .collect::<BTreeMap<_, _>>(),
        b.unmatched_stop_events
    );
    assert_eq!(
        a.values
            .iter()
            .map(|v| (
                ids[&v.zone],
                v.event,
                v.thread,
                v.context,
                v.raw_value,
                v.received_frame
            ))
            .collect::<Vec<_>>(),
        b.values
            .iter()
            .map(|v| (
                v.zone,
                v.event,
                v.thread,
                v.context,
                v.raw_value,
                v.received_frame
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(a.legacy_cuda_buffers, b.legacy_cuda_buffers);
    assert_eq!(
        native.untimed_error_events(),
        original
            .untimed_error_events()
            .iter()
            .filter(|e| e.received_frame <= last)
            .cloned()
            .collect::<Vec<_>>()
    );
    for (id, zone) in &a.zones {
        assert_eq!(zone.events, analysis(original).zones[id].events);
    }
}

fn export_file(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    out.write_all(bytes).unwrap();
}

#[test]
#[ignore = "requires original native containers and exact SDK/raw pairs"]
fn actual_native_roundtrip_and_strict_prefix_match_every_retained_frame() {
    let out = std::env::var_os("PVD_NATIVE_EXPORT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "run-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&out).unwrap();
    for stem in ["baseline", "rich", "limits"] {
        let path = if stem == "limits" {
            limit("-native.pxd2")
        } else {
            pair(stem, "-native.pxd2")
        };
        let raw_path = if stem == "limits" {
            limit(".pvdstream")
        } else {
            pair(stem, ".pvdstream")
        };
        let source = std::fs::read(&path).unwrap();
        let initial = native_initial::NativeInitialValues::parse(&source);
        let raw_bytes = std::fs::read(raw_path).unwrap();
        let mut roundtrip = Vec::new();
        let report = rewrite(&source, &mut roundtrip, || true).unwrap();
        assert_eq!(roundtrip, source);
        export_file(&out.join(format!("{stem}-roundtrip.pxd2")), &roundtrip);
        let (original, info) = native::read_with_progress(&source, |_, _| true).unwrap();
        let full_raw = Recording::from_bytes(&raw_bytes).unwrap();
        if stem == "limits" {
            let truth =
                serde_json::from_slice(&std::fs::read(limit(".joints.json")).unwrap()).unwrap();
            sdk_joint_limits::verify_sdk_capture(&full_raw, &truth);
        }
        eprintln!("{stem} roundtrip {report:?}");
        let boundaries = if stem == "limits" {
            vec![0, 2, 5]
        } else {
            vec![0, 29, 30, 89, 90, 119]
        };
        for last in boundaries {
            let mut clipped_bytes = Vec::new();
            let report = trim_scene_frames(&source, 0, last, &mut clipped_bytes, || true).unwrap();
            assert!(clipped_bytes.len() < source.len());
            export_file(&out.join(format!("{stem}-0-{last}.pxd2")), &clipped_bytes);
            let (clipped, clipped_info) =
                native::read_with_progress(&clipped_bytes, |_, _| true).unwrap();
            assert_eq!(clipped.frame_count(), last + 1);
            let raw_trimmed = pvd_core::clip::trim_scene_frames(&raw_bytes, 0, last).unwrap();
            let raw = Recording::from_bytes(&raw_trimmed).unwrap();
            let mut properties = 0;
            for frame in (0..=last).chain((0..=last).rev()) {
                assert_eq!(
                    clipped.state_at(frame).unwrap(),
                    original.state_at(frame).unwrap(),
                    "{stem} last {last} frame {frame}"
                );
                properties +=
                    check_frame(&clipped, &raw, frame, &clipped_info.source_ids, &initial);
            }
            check_analysis(&clipped, &original, &raw, last, &info.source_ids);
            if stem != "limits" {
                let csv = std::fs::read_to_string(pair(stem, ".csv")).unwrap();
                for line in csv.lines().skip(1) {
                    let columns = line.split(',').collect::<Vec<_>>();
                    let frame: usize = columns[0].parse().unwrap();
                    if frame > last {
                        continue;
                    }
                    let source_id: u64 = columns[1].parse().unwrap();
                    let state = clipped.state_at(frame).unwrap();
                    let object = state
                        .objects
                        .values()
                        .find(|o| clipped_info.source_ids.get(&o.id) == Some(&source_id))
                        .unwrap();
                    assert_eq!(object.properties["Name"], Value::String(columns[2].into()));
                    let Value::Transform { position, rotation } = &object.properties["GlobalPose"]
                    else {
                        panic!("pose missing")
                    };
                    for (value, text) in position.iter().chain(rotation).zip(&columns[3..]) {
                        assert!((*value - text.parse::<f32>().unwrap()).abs() < 1e-6);
                    }
                }
            }
            clipped.verify_replay().unwrap();
            eprintln!(
                "{stem} last={last} {report:?}, compared_properties={properties}, spans={}, memory_events={}",
                analysis(&clipped).spans.len(),
                analysis(&clipped).memory.events().len()
            );
        }
        assert_eq!(std::fs::read(path).unwrap(), source);
    }
}

#[test]
#[ignore = "requires preserved original native containers and exact SDK/raw pairs"]
fn actual_native_middle_intervals_preserve_source_and_sdk_states() {
    let output = std::env::var_os("PVD_NATIVE_EXPORT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "middle-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut accepted = 0;
    for (stem, first, last) in [
        ("baseline", 1, 2),
        ("baseline", 30, 31),
        ("baseline", 80, 95),
        ("baseline", 1, 119),
        ("limits", 1, 2),
        ("rich", 1, 2),
    ] {
        let path = if stem == "limits" {
            limit("-native.pxd2")
        } else {
            pair(stem, "-native.pxd2")
        };
        let source = std::fs::read(&path).unwrap();
        let initial = native_initial::NativeInitialValues::parse(&source);
        let mut bytes = Vec::new();
        let report = trim_scene_frames(&source, first, last, &mut bytes, || true).unwrap();
        let (original, _) = native::read_with_progress(&source, |_, _| true).unwrap();
        let (clipped, info) = native::read_with_progress(&bytes, |_, _| true).unwrap();
        let raw_path = if stem == "limits" {
            limit(".pvdstream")
        } else {
            pair(stem, ".pvdstream")
        };
        let raw_source = std::fs::read(raw_path).unwrap();
        let raw = pvd_core::clip::trim_scene_frames(&raw_source, first, last).unwrap();
        let raw = Recording::from_bytes(&raw).unwrap();
        assert_eq!(clipped.frame_count(), last - first + 1);
        for frame in (0..clipped.frame_count()).chain((0..clipped.frame_count()).rev()) {
            let actual = clipped.state_at(frame).unwrap();
            let expected = original.state_at(frame + first).unwrap();
            assert_eq!(actual.objects, expected.objects);
            assert_eq!(actual.contacts, expected.contacts);
            assert_eq!(actual.debug_draw, expected.debug_draw);
            if stem == "baseline" {
                check_frame(&clipped, &raw, frame, &info.source_ids, &initial);
            } else {
                check_frame_state(&clipped, &raw, frame, &info.source_ids, &initial);
            }
        }
        if stem != "baseline" {
            assert!(clipped.native_initial_memory_totals().unwrap().is_some());
            native_memory_interval::verify_partial_interval(
                &clipped,
                &Recording::from_bytes(&raw_source).unwrap(),
                first,
                last,
                &info.source_ids,
            );
        }
        clipped.verify_replay().unwrap();
        std::fs::create_dir_all(&output).unwrap();
        export_file(&output.join(format!("{stem}-{first}-{last}.pxd2")), &bytes);
        accepted += 1;
        eprintln!("accepted {stem} {first}..{last}: {report:?}");
        assert_eq!(std::fs::read(&path).unwrap(), source);
    }
    eprintln!("six native middle intervals: accepted={accepted}");
    assert_eq!(accepted, 6);
}

#[test]
#[ignore = "requires the original baseline container and its exact SDK/raw pair"]
fn actual_native_derived_snapshot_prefixes_preserve_source_and_sdk_states() {
    let path = pair("baseline", "-native.pxd2");
    let source = std::fs::read(&path).unwrap();
    let raw_bytes = std::fs::read(pair("baseline", ".pvdstream")).unwrap();
    let initial = native_initial::NativeInitialValues::parse(&source);
    let (original, _) = native::read_with_progress(&source, |_, _| true).unwrap();
    let output = std::env::var_os("PVD_NATIVE_EXPORT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "retrim-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&output).unwrap();
    let mut compared_properties = 0;
    for first in [1, 80] {
        let mut middle_bytes = Vec::new();
        trim_scene_frames(&source, first, 119, &mut middle_bytes, || true).unwrap();
        let (middle, _) = native::read_with_progress(&middle_bytes, |_, _| true).unwrap();
        export_file(
            &output.join(format!("baseline-{first}-119.pxd2")),
            &middle_bytes,
        );
        for last in [0, 15, middle.frame_count() - 2] {
            let mut bytes = Vec::new();
            let report = trim_scene_frames(&middle_bytes, 0, last, &mut bytes, || true).unwrap();
            let (recording, info) = native::read_with_progress(&bytes, |_, _| true).unwrap();
            assert_eq!(recording.frame_count(), last + 1);
            assert_eq!(
                recording.initial_untimed_errors(),
                middle.initial_untimed_errors()
            );
            let raw = Recording::from_bytes(
                &pvd_core::clip::trim_scene_frames(&raw_bytes, first, first + last).unwrap(),
            )
            .unwrap();
            let random = [last, 0, last / 2, last.saturating_sub(1), 0];
            for frame in (0..=last).chain((0..=last).rev()).chain(random) {
                let actual = recording.state_at(frame).unwrap();
                assert_eq!(actual, middle.state_at(frame).unwrap());
                let expected = original.state_at(first + frame).unwrap();
                assert_eq!(actual.objects, expected.objects);
                assert_eq!(actual.cameras, expected.cameras);
                assert_eq!(actual.contacts, expected.contacts);
                assert_eq!(actual.debug_draw, expected.debug_draw);
                assert_eq!(actual.errors, expected.errors);
                compared_properties +=
                    check_frame(&recording, &raw, frame, &info.source_ids, &initial);
            }
            recording.verify_replay().unwrap();
            let mut copy = Vec::new();
            rewrite(&bytes, &mut copy, || true).unwrap();
            assert_eq!(copy, bytes);
            export_file(
                &output.join(format!("baseline-{first}-119-prefix-{last}.pxd2")),
                &bytes,
            );
            eprintln!("derived snapshot first={first} last={last}: {report:?}");
        }
    }
    assert_eq!(std::fs::read(path).unwrap(), source);
    assert!(compared_properties > 10_000);
    eprintln!("six derived snapshot prefixes: compared_properties={compared_properties}");
}

#[test]
#[ignore = "requires the original baseline container and its exact SDK/raw pair"]
fn actual_native_derived_snapshot_middles_preserve_source_and_sdk_states() {
    let path = pair("baseline", "-native.pxd2");
    let source = std::fs::read(&path).unwrap();
    let raw_bytes = std::fs::read(pair("baseline", ".pvdstream")).unwrap();
    let initial = native_initial::NativeInitialValues::parse(&source);
    let original = Recording::from_bytes(&source).unwrap();
    let directory = std::env::var_os("PVD_NATIVE_EXPORT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "remiddle-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&directory).unwrap();
    let mut compared_properties = 0;
    let mut accepted = 0;
    for (base, ranges) in [
        (1, [(1, 2), (30, 31), (80, 95)]),
        (80, [(1, 2), (10, 15), (1, 37)]),
    ] {
        let mut middle = Vec::new();
        trim_scene_frames(&source, base, 119, &mut middle, || true).unwrap();
        export_file(
            &directory.join(format!("baseline-{base}-119.pxd2")),
            &middle,
        );
        for (first, last) in ranges {
            let mut bytes = Vec::new();
            let report = trim_scene_frames(&middle, first, last, &mut bytes, || true).unwrap();
            let (recording, info) = native::read_with_progress(&bytes, |_, _| true).unwrap();
            assert_eq!(recording.frame_count(), last - first + 1);
            let mut direct = Vec::new();
            trim_scene_frames(&source, base + first, base + last, &mut direct, || true).unwrap();
            let direct = Recording::from_bytes(&direct).unwrap();
            assert_eq!(
                recording.initial_untimed_errors(),
                direct.initial_untimed_errors()
            );
            assert_eq!(
                recording.untimed_error_events(),
                direct.untimed_error_events()
            );
            let sdk = Recording::from_bytes(
                &pvd_core::clip::trim_scene_frames(&raw_bytes, base + first, base + last).unwrap(),
            )
            .unwrap();
            let count = recording.frame_count();
            for frame in (0..count)
                .chain((0..count).rev())
                .chain([count - 1, 0, count / 2, 0])
            {
                let actual = recording.state_at(frame).unwrap();
                let expected = original.state_at(base + first + frame).unwrap();
                let direct = direct.state_at(frame).unwrap();
                assert_eq!(actual.objects, expected.objects);
                assert_eq!(actual.objects, direct.objects);
                assert_eq!(actual.cameras, expected.cameras);
                assert_eq!(actual.debug_draw, expected.debug_draw);
                assert_eq!(actual.contacts, expected.contacts);
                assert_eq!(actual.errors, expected.errors);
                compared_properties +=
                    check_frame(&recording, &sdk, frame, &info.source_ids, &initial);
            }
            recording.verify_replay().unwrap();
            let mut rewritten = Vec::new();
            rewrite(&bytes, &mut rewritten, || true).unwrap();
            assert_eq!(rewritten, bytes);
            export_file(
                &directory.join(format!("baseline-{base}-119-middle-{first}-{last}.pxd2")),
                &bytes,
            );
            accepted += 1;
            eprintln!("derived snapshot middle base={base} range={first}..{last}: {report:?}");
        }
    }
    assert_eq!(accepted, 6);
    assert!(compared_properties > 10_000);
    assert_eq!(std::fs::read(path).unwrap(), source);
    eprintln!("six derived snapshot middles: compared_properties={compared_properties}");
}
