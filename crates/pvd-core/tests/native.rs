#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::legacy_fixture;
use pvd_core::{
    Recording, RecordingSource, Value,
    analysis::{MemoryCursor, MemoryState},
    native,
    profile_curves::{self, ReferenceScene},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

fn fixture(stem: &str, suffix: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../captures/native")
        .join(format!("{stem}-paired{suffix}"))
}

fn source_ids(bytes: &[u8]) -> BTreeMap<u64, u64> {
    let u32_at =
        |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
    let comm = 16 + u32_at(8);
    let count = u32_at(comm + 20);
    (0..count)
        .map(|i| {
            let start = comm + 24 + i * 26;
            (
                u32_at(start) as u64,
                u64::from_le_bytes(bytes[start + 4..start + 12].try_into().unwrap()),
            )
        })
        .collect()
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

fn verify_pair(stem: &str) {
    let bytes = std::fs::read(fixture(stem, "-native.pxd2")).unwrap();
    let ids = source_ids(&bytes);
    let native = Recording::from_bytes(&bytes).unwrap();
    let raw = Recording::from_path(fixture(stem, ".pvdstream")).unwrap();
    assert_eq!(native.frame_count(), 120);
    assert_eq!(native.frames.len(), raw.frames.len());
    native.verify_replay().unwrap();
    let mut properties = 0;
    for frame in (0..120).map(|i| (i * 73) % 120) {
        let state = native.state_at(frame).unwrap();
        let expected = raw.state_at(frame).unwrap();
        assert_eq!(native.frames[frame].timestamp, raw.frames[frame].timestamp);
        assert_eq!(
            ids[&native.frames[frame].scene_id],
            raw.frames[frame].scene_id
        );
        assert_eq!(
            state.render_scene().objects.len(),
            expected.render_scene().objects.len()
        );
        for render in state.render_scene().objects {
            let other = expected
                .render_scene()
                .objects
                .into_iter()
                .find(|o| o.shape_id == ids[&render.shape_id])
                .unwrap();
            assert_eq!(render.geometry, other.geometry);
            assert_eq!(render.position, other.position);
            assert_eq!(render.rotation, other.rotation);
            assert_eq!(render.dynamic, other.dynamic);
            assert_eq!(render.sleeping, other.sleeping);
        }
        for (scene, contacts) in &state.contacts {
            let other = &expected.contacts[&ids[scene]];
            assert_eq!(contacts.len(), other.len());
            for (contact, reference) in contacts.iter().zip(other.iter()) {
                assert_eq!(contact.position, reference.position);
                assert_eq!(contact.normal, reference.normal);
                assert_eq!(
                    contact.shapes.map(|id| *ids.get(&id).unwrap_or(&id)),
                    reference.shapes
                );
                assert_eq!(contact.separation, reference.separation);
                assert_eq!(contact.faces, reference.faces);
                assert_eq!(
                    contact.normal_force_available,
                    reference.normal_force_available
                );
                if contact.normal_force_available {
                    assert_eq!(contact.normal_force, reference.normal_force);
                }
            }
        }
        for object in state.objects.values() {
            if matches!(
                object.type_name.as_str(),
                "_debugger::MemoryEventTotals" | "_debugger_::UserCustomedEvent"
            ) {
                continue;
            }
            let source_id = ids.get(&object.id).unwrap_or_else(|| {
                panic!("missing source map for {} {}", object.id, object.type_name)
            });
            let source = expected.objects.get(source_id).unwrap_or_else(|| {
                panic!(
                    "missing source object for {} {} source {source_id}",
                    object.id, object.type_name
                )
            });
            assert_eq!(
                object.type_name.rsplit("::").next(),
                source.type_name.rsplit("::").next()
            );
            for (key, value) in &object.properties {
                if key == "events" || matches!(value, Value::Bytes(_)) {
                    continue;
                }
                let other = &source.properties[key];
                assert_eq!(
                    &mapped(value, &ids),
                    other,
                    "{stem} frame {frame}, {}::{key}",
                    object.type_name
                );
                properties += 1;
            }
        }
    }
    assert!(properties > 40_000);
    let csv = std::fs::read_to_string(fixture(stem, ".csv")).unwrap();
    let mut poses = 0;
    for row in csv.lines().skip(1) {
        let columns = row.split(',').collect::<Vec<_>>();
        let frame = columns[0].parse::<usize>().unwrap();
        let source_id = columns[1].parse::<u64>().unwrap();
        let state = native.state_at(frame).unwrap();
        let object = state
            .objects
            .values()
            .find(|o| ids.get(&o.id) == Some(&source_id))
            .unwrap();
        assert_eq!(object.properties["Name"], Value::String(columns[2].into()));
        let Value::Transform { position, rotation } = &object.properties["GlobalPose"] else {
            panic!("missing pose")
        };
        for (value, text) in position.iter().chain(rotation).zip(&columns[3..]) {
            assert!((*value - text.parse::<f32>().unwrap()).abs() < 1e-6);
        }
        poses += 1;
    }
    assert!(poses >= 240);
    eprintln!(
        "{stem}: {properties} semantic properties and {poses} independent SDK poses verified"
    );
}

#[test]
#[ignore = "requires original PVD3 saved baseline and exact TCP/SDK truth pair"]
fn original_baseline_matches_same_tcp_stream_and_sdk_getters() {
    verify_pair("baseline");
}

#[test]
#[ignore = "requires original PVD3 saved rich scene and exact TCP/SDK truth pair"]
fn original_rich_matches_same_tcp_stream_and_sdk_getters() {
    verify_pair("rich");
}

fn map_memory_state(mut state: MemoryState, ids: &BTreeMap<u64, u64>) -> MemoryState {
    state.live = state
        .live
        .into_iter()
        .map(|((buffer, address), allocation)| ((ids[&buffer], address), allocation))
        .collect();
    state
}

#[test]
#[ignore = "requires original PVD3 rich container and exact same TCP capture"]
fn original_rich_analysis_matches_every_raw_event_flush_and_frame() {
    let bytes = std::fs::read(fixture("rich", "-native.pxd2")).unwrap();
    let ids = source_ids(&bytes);
    let native = Recording::from_bytes(&bytes).unwrap();
    let raw = Recording::from_path(fixture("rich", ".pvdstream")).unwrap();
    assert_eq!(native.source, RecordingSource::Pvd3Database);
    assert_eq!(raw.source, RecordingSource::SdkStream);
    assert_eq!(
        native.header.timestamp_numerator,
        raw.header.timestamp_numerator
    );
    assert_eq!(
        native.header.timestamp_denominator,
        raw.header.timestamp_denominator
    );
    for (a, b) in native.frames.iter().zip(&raw.frames) {
        assert_eq!((ids[&a.scene_id], a.timestamp), (b.scene_id, b.timestamp));
    }
    let a = legacy_fixture(&native);
    let b = legacy_fixture(&raw);
    assert_eq!(a.zones.len(), b.zones.len());
    assert_eq!(a.zones.values().map(|z| z.events.len()).sum::<usize>(), 140);
    for (id, zone) in &a.zones {
        let expected = &b.zones[&ids[id]];
        assert_eq!(zone.name, expected.name);
        assert_eq!(zone.events, expected.events);
        assert!(
            zone.events
                .keys()
                .all(|event| !a.profile_metadata_ambiguous(*id, *event))
        );
    }
    assert!(!a.spans.is_empty());
    assert_eq!(a.spans.len(), b.spans.len());
    for (span, expected) in a.spans.iter().zip(&b.spans) {
        assert_eq!(
            (
                ids[&span.zone],
                span.event,
                span.thread,
                span.context,
                span.start,
                span.end,
                span.received_frame
            ),
            (
                expected.zone,
                expected.event,
                expected.thread,
                expected.context,
                expected.start,
                expected.end,
                expected.received_frame
            )
        );
    }
    assert_eq!(a.values.len(), b.values.len());
    for (value, expected) in a.values.iter().zip(&b.values) {
        assert_eq!(
            (
                ids[&value.zone],
                value.event,
                value.thread,
                value.context,
                value.raw_value
            ),
            (
                expected.zone,
                expected.event,
                expected.thread,
                expected.context,
                expected.raw_value
            )
        );
    }
    let pending: BTreeMap<_, _> = a
        .open_event_starts()
        .map(|(&(zone, event, thread, context), ticks)| {
            ((ids[&zone], event, thread, context), ticks)
        })
        .collect();
    assert_eq!(
        pending,
        b.open_event_starts()
            .map(|(key, ticks)| (*key, ticks))
            .collect()
    );
    assert_eq!(a.unmatched_stops, b.unmatched_stops);
    assert_eq!(a.legacy_cuda_buffers, b.legacy_cuda_buffers);
    assert_eq!(
        a.unmatched_stop_events
            .iter()
            .map(|(&(zone, event, thread, context), count)| (
                (ids[&zone], event, thread, context),
                *count
            ))
            .collect::<BTreeMap<_, _>>(),
        b.unmatched_stop_events
    );

    let am = &a.memory;
    let bm = &b.memory;
    assert_eq!(am.events().len(), 7549);
    assert_eq!(am.events().len(), bm.events().len());
    for (event, expected) in am.events().iter().zip(bm.events()) {
        assert_eq!(ids[&event.buffer], expected.buffer);
        assert_eq!(
            (event.address, event.received_frame, &event.kind),
            (expected.address, expected.received_frame, &expected.kind)
        );
    }
    assert_eq!(am.samples.len(), 135);
    assert_eq!(am.samples.len(), bm.samples.len());
    for (sample, expected) in am.samples.iter().zip(&bm.samples) {
        assert_eq!(
            (
                sample.received_frame,
                sample.live_bytes,
                sample.live_allocations,
                sample.allocations,
                sample.frees
            ),
            (
                expected.received_frame,
                expected.live_bytes,
                expected.live_allocations,
                expected.allocations,
                expected.frees
            )
        );
    }
    assert_eq!(
        (am.allocations, am.frees, am.peak_bytes),
        (4321, 3228, bm.peak_bytes)
    );
    let mut cursor = MemoryCursor::default();
    for frame in (0..=120)
        .chain((0..=120).rev())
        .chain((0..121).map(|i| (i * 73) % 121))
    {
        let expected = bm.state_at_frame(frame);
        assert_eq!(map_memory_state(am.state_at_frame(frame), &ids), expected);
        assert_eq!(
            map_memory_state(am.seek(&mut cursor, frame).clone(), &ids),
            expected
        );
        assert_eq!(am.churn_at_frame(frame), bm.churn_at_frame(frame));
    }
    assert_eq!(
        am.samples
            .iter()
            .filter(|sample| sample.received_frame == 120)
            .count(),
        2
    );
    assert!(!am.events_at_frame(120).is_empty());
    assert_ne!(am.state_at_frame(119), am.state_at_frame(120));

    let cancel = AtomicBool::new(false);
    let scene = ReferenceScene {
        id: native.frames[0].scene_id,
        generation: 0,
    };
    let raw_scene = ReferenceScene {
        id: ids[&scene.id],
        generation: 0,
    };
    let sampled = profile_curves::sampling_frames(&native, scene, &cancel).unwrap();
    assert_eq!(sampled.frames.len(), 120);
    let catalog = profile_curves::catalog(&native, &cancel).unwrap();
    let mut recorded = 0;
    for entry in &catalog {
        let key = entry.key;
        let mut raw_key = key;
        raw_key.zone = ids[&key.zone];
        let actual = profile_curves::sample(&native, key, scene, &sampled.frames, &cancel).unwrap();
        let expected =
            profile_curves::sample(&raw, raw_key, raw_scene, &sampled.frames, &cancel).unwrap();
        for (point, reference) in actual.iter().zip(expected) {
            assert_eq!(
                (
                    point.frame,
                    point.milliseconds,
                    point.occurrence_count,
                    point.status
                ),
                (
                    reference.frame,
                    reference.milliseconds,
                    reference.occurrence_count,
                    reference.status
                )
            );
            recorded += usize::from(point.milliseconds.is_some());
        }
    }
    assert!(recorded > 0);
    eprintln!(
        "native analysis: {} spans, 140 event names, {} profile samples, 7549 memory events and 135 flushes match TCP; memory states match in forward/reverse/random order",
        a.spans.len(),
        recorded
    );
}

#[test]
#[ignore = "requires original PVD3 rich container and exact same TCP error event"]
fn original_rich_untimed_error_matches_raw_fields_and_receive_order() {
    let native = Recording::from_path(fixture("rich", "-native.pxd2")).unwrap();
    let raw = Recording::from_path(fixture("rich", ".pvdstream")).unwrap();
    let expected: Vec<_> = raw.error_events().collect();
    assert_eq!(expected.len(), 1);
    let errors = native.untimed_error_events();
    assert_eq!(errors.len(), expected.len());
    assert!(native.error_events().next().is_none());
    assert!(raw.untimed_error_events().is_empty());
    for (actual, (frame, event)) in errors.iter().zip(expected) {
        assert_eq!(
            (
                actual.code,
                actual.message.as_str(),
                actual.file.as_str(),
                actual.line
            ),
            (
                event.code,
                event.message.as_str(),
                event.file.as_str(),
                event.line
            )
        );
        assert_eq!(Some(actual.received_frame), frame);
        assert!(event.timestamp > 0);
        assert_eq!(
            (actual.index, actual.history_event, actual.received_frame),
            (0, 981, 30)
        );
        assert_eq!(
            (actual.container_instance, actual.container_generation),
            (29, 0)
        );
    }
    for frame in [0, 29, 30, 119, 30, 29, 30] {
        let state = native.state_at(frame).unwrap();
        if frame < 30 {
            assert!(!state.objects.contains_key(&29));
        } else {
            let Value::Array(records) = &state.objects[&29].properties["Events"] else {
                panic!("error payload not retained")
            };
            assert_eq!(records.len(), 1);
            assert_eq!(
                state.objects[&29].type_name,
                "_debugger_::UserCustomedEvent"
            );
        }
        assert!(state.errors.is_empty());
    }
    native.verify_replay().unwrap();
}

#[test]
fn rejects_short_headers_versions_lengths_and_cancellation() {
    for len in 0..16 {
        assert!(native::from_bytes_with_progress(&vec![0; len], |_, _| true).is_err());
    }
    let mut header = native::MAGIC.to_le_bytes().to_vec();
    header.extend(2u32.to_le_bytes());
    header.extend([0; 8]);
    assert!(
        native::from_bytes_with_progress(&header, |_, _| true)
            .unwrap_err()
            .to_string()
            .contains("version")
    );
    header[4..8].copy_from_slice(&1u32.to_le_bytes());
    header[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        native::from_bytes_with_progress(&header, |_, _| true)
            .unwrap_err()
            .to_string()
            .contains("64 MiB")
    );
    assert!(
        native::from_bytes_with_progress(&header, |_, _| false)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    let error = pvd_core::clip::trim_scene_frames(&header, 0, 1).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("trimming original PVD3 containers is not supported")
    );
}

#[test]
#[ignore = "requires original PVD3 saved baseline for corruption regressions"]
fn original_container_truncation_offsets_unknown_events_and_cancel() {
    let bytes = std::fs::read(fixture("baseline", "-native.pxd2")).unwrap();
    for length in [15, 1000, 100000, 110592, 114688, bytes.len() - 1] {
        assert!(
            Recording::from_bytes(&bytes[..length]).is_err(),
            "length {length}"
        );
    }
    let mut corrupt = bytes.clone();
    let history = 0x1b000;
    corrupt[history + 8..history + 12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        Recording::from_bytes(&corrupt)
            .unwrap_err()
            .to_string()
            .contains("million")
    );
    let page = 0x1d000;
    corrupt = bytes.clone();
    corrupt[page + 4..page + 8].copy_from_slice(&1u32.to_le_bytes());
    assert!(
        Recording::from_bytes(&corrupt)
            .unwrap_err()
            .to_string()
            .contains("first event offset")
    );
    corrupt = bytes.clone();
    let event = page + 4 + 2602 * 4;
    corrupt[event..event + 4].copy_from_slice(&0xffffu32.to_le_bytes());
    assert!(
        Recording::from_bytes(&corrupt)
            .unwrap_err()
            .to_string()
            .contains("unsupported PVD3 history event")
    );
    corrupt = bytes.clone();
    let message_offset = u32::from_le_bytes(
        bytes[page + 4 + 32 * 4..page + 8 + 32 * 4]
            .try_into()
            .unwrap(),
    ) as usize
        + event;
    assert_eq!(
        u32::from_le_bytes(
            bytes[message_offset..message_offset + 4]
                .try_into()
                .unwrap()
        ),
        4
    );
    assert_eq!(
        u32::from_le_bytes(
            bytes[message_offset + 4..message_offset + 8]
                .try_into()
                .unwrap()
        ),
        8
    );
    corrupt[message_offset + 4..message_offset + 8].copy_from_slice(&4u32.to_le_bytes());
    assert!(
        Recording::from_bytes(&corrupt)
            .unwrap_err()
            .to_string()
            .contains("property owner")
    );
    corrupt = bytes.clone();
    corrupt[0x1c000..0x1c004].copy_from_slice(&1u32.to_le_bytes());
    assert!(
        Recording::from_bytes(&corrupt)
            .unwrap_err()
            .to_string()
            .contains("array count exceeds containing block")
    );
    let mut callbacks = 0;
    let error = native::from_bytes_with_progress(&bytes, |event, _| {
        callbacks += 1;
        event < 100
    })
    .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert!(callbacks >= 100);
    let recording = Recording::from_path(fixture("baseline", "-native.pxd2")).unwrap();
    assert_eq!(recording.frame_count(), 120);
    let error =
        Recording::from_path_with_progress(fixture("baseline", "-native.pxd2"), |loaded, _| {
            loaded < 100_000
        })
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
}
