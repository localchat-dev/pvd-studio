#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::legacy_fixture;
use pvd_core::{
    Recording,
    analysis::{Allocation, MemoryEvent, MemoryEventKind, MemoryState},
    clip::trim_scene_frames,
};
use pvd_protocol::{Decoder, Endian, Event, Name, rewrite::map_group_events};
use std::{collections::BTreeSet, fs, path::PathBuf, sync::Arc};

fn capture(variable: &str, filename: &str) -> Vec<u8> {
    let path = std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures")
                .join(filename)
        });
    fs::read(&path)
        .unwrap_or_else(|error| panic!("generate real SDK capture {}: {error}", path.display()))
}

fn remap_allocation(value: &Arc<Allocation>, first: usize) -> Arc<Allocation> {
    let mut value = (**value).clone();
    value.received_frame = value.received_frame.saturating_sub(first);
    Arc::new(value)
}

fn remap_memory_event(value: &MemoryEvent, first: usize) -> MemoryEvent {
    MemoryEvent {
        received_frame: value.received_frame.saturating_sub(first),
        kind: match &value.kind {
            MemoryEventKind::Allocate {
                allocation,
                replaced,
            } => MemoryEventKind::Allocate {
                allocation: remap_allocation(allocation, first),
                replaced: replaced
                    .as_ref()
                    .map(|value| remap_allocation(value, first)),
            },
            MemoryEventKind::Free { allocation } => MemoryEventKind::Free {
                allocation: allocation
                    .as_ref()
                    .map(|value| remap_allocation(value, first)),
            },
        },
        ..value.clone()
    }
}

fn remap_memory_state(mut value: MemoryState, first: usize) -> MemoryState {
    for allocation in value.live.values_mut() {
        *allocation = remap_allocation(allocation, first);
    }
    value
}

fn compare(bytes: &[u8], source: &Recording, first: usize, last: usize) -> Recording {
    let output = trim_scene_frames(bytes, first, last)
        .unwrap_or_else(|error| panic!("trim {first}..={last}: {error}"));
    let trimmed = Recording::from_bytes(&output).unwrap();
    trimmed.verify_replay().unwrap();
    assert_eq!(trimmed.frame_count(), last - first + 1);
    for frame in (0..trimmed.frame_count()).rev() {
        assert_eq!(
            trimmed.state_at(frame).unwrap(),
            source.state_at(first + frame).unwrap(),
            "complete state at source frame {}",
            first + frame
        );
        assert_eq!(
            trimmed.frames[frame].timestamp,
            source.frames[first + frame].timestamp
        );
        assert_eq!(
            trimmed.frames[frame].scene_id,
            source.frames[first + frame].scene_id
        );
        assert_eq!(
            legacy_fixture(&trimmed).memory.state_at_frame(frame),
            remap_memory_state(
                legacy_fixture(source).memory.state_at_frame(first + frame),
                first
            )
        );
    }
    trimmed
}

#[test]
#[ignore = "requires real SDK 3.4 baseline, rich, joint-limit, heightfield and query-shape captures"]
fn real_debug_captures_preserve_every_selected_frame() {
    for (variable, filename, ranges, expect_debug) in [
        (
            "PVD_SDK34_CAPTURE",
            "physx-3.4.pvdstream",
            &[(0, 0), (1, 119), (59, 61), (70, 100), (119, 119)][..],
            false,
        ),
        (
            "PVD_SDK34_RICH_CAPTURE",
            "physx-3.4-rich.pvdstream",
            &[(0, 119), (1, 1), (1, 119), (59, 61), (70, 100), (119, 119)][..],
            true,
        ),
        (
            "PVD_JOINT_LIMIT_CAPTURE",
            "physx-3.4-joint-limits.pvdstream",
            &[(0, 5), (1, 5), (2, 5), (5, 5)][..],
            true,
        ),
        (
            "PVD_HEIGHTFIELD_CAPTURE",
            "physx-3.4-heightfield.pvdstream",
            &[(0, 74), (1, 74), (35, 55), (45, 45), (74, 74)][..],
            false,
        ),
        (
            "PVD_QUERY_SHAPES_CAPTURE",
            "physx-3.4-query-shapes.pvdstream",
            &[(0, 11), (1, 11), (3, 3), (5, 8), (11, 11)][..],
            false,
        ),
        (
            "PVD_CLIP_ERROR_CAPTURE",
            "physx-3.4-error.pvdstream",
            &[(0, 11), (1, 11), (5, 8), (11, 11)][..],
            false,
        ),
    ] {
        let bytes = capture(variable, filename);
        let source = Recording::from_bytes(&bytes).unwrap();
        if filename == "physx-3.4-error.pvdstream" {
            assert!(source.error_events().next().is_some());
        }
        if expect_debug {
            assert!(
                source
                    .state_at(1)
                    .unwrap()
                    .debug_draw
                    .values()
                    .any(|batch| !batch.is_empty()),
                "{filename} must contain real debug draw"
            );
        }
        for &(first, last) in ranges {
            compare(&bytes, &source, first, last);
        }
        assert!(trim_scene_frames(&bytes, 2, 1).is_err());
        assert!(trim_scene_frames(&bytes, 0, source.frame_count()).is_err());
        eprintln!(
            "{filename}: {} ranges, complete scene/debug/memory states verified",
            ranges.len()
        );
    }
}

#[test]
#[ignore = "requires real SDK 3.4 eALL capture"]
fn real_analysis_preroll_preserves_events_and_original_timestamps() {
    let bytes = capture("PVD_CLIP_ANALYSIS_CAPTURE", "physx-3.4-all.pvdstream");
    let source = Recording::from_bytes(&bytes).unwrap();
    let first = 70;
    let last = 100;
    let trimmed = compare(&bytes, &source, first, last);
    let prefix = Recording::from_bytes(&trim_scene_frames(&bytes, 0, last).unwrap()).unwrap();
    let actual = legacy_fixture(&trimmed);
    let expected = legacy_fixture(&prefix);
    assert!(!expected.spans.is_empty());
    assert!(!expected.memory.events().is_empty());
    assert_eq!(actual.zones.len(), expected.zones.len());
    for (id, zone) in &expected.zones {
        assert_eq!(actual.zones[id].name, zone.name);
        assert_eq!(actual.zones[id].events, zone.events);
    }
    assert_eq!(actual.spans.len(), expected.spans.len());
    for (a, b) in actual.spans.iter().zip(&expected.spans) {
        assert_eq!(
            (
                a.zone,
                a.event,
                a.thread,
                a.context,
                a.start,
                a.end,
                a.received_frame
            ),
            (
                b.zone,
                b.event,
                b.thread,
                b.context,
                b.start,
                b.end,
                b.received_frame.saturating_sub(first)
            )
        );
    }
    assert_eq!(actual.values.len(), expected.values.len());
    for (a, b) in actual.values.iter().zip(&expected.values) {
        assert_eq!(
            (a.zone, a.event, a.thread, a.context, a.raw_value),
            (b.zone, b.event, b.thread, b.context, b.raw_value)
        );
    }
    assert_eq!(actual.unmatched_stops, expected.unmatched_stops);
    assert_eq!(actual.unmatched_stop_events, expected.unmatched_stop_events);
    assert_eq!(actual.legacy_cuda_buffers, expected.legacy_cuda_buffers);
    assert_eq!(actual.memory.events().len(), expected.memory.events().len());
    for (a, b) in actual.memory.events().iter().zip(expected.memory.events()) {
        assert_eq!(**a, remap_memory_event(b, first));
    }
    assert_eq!(actual.memory.samples.len(), expected.memory.samples.len());
    for (a, b) in actual.memory.samples.iter().zip(&expected.memory.samples) {
        assert_eq!(
            (
                a.received_frame,
                a.live_bytes,
                a.live_allocations,
                a.allocations,
                a.frees
            ),
            (
                b.received_frame.saturating_sub(first),
                b.live_bytes,
                b.live_allocations,
                b.allocations,
                b.frees
            )
        );
    }
}

fn n32(value: u32, endian: Endian) -> [u8; 4] {
    match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    }
}
fn n64(value: u64, endian: Endian) -> [u8; 8] {
    match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    }
}
fn group(output: &mut Vec<u8>, raw: &[u8], header: &[u8], endian: Endian) {
    output.extend(n32(raw.len() as u32, endian));
    output.extend(n32(1, endian));
    output.extend_from_slice(&header[8..24]);
    output.extend_from_slice(raw);
}
fn begin(id: u64, property: u32, datatype: Name, endian: Endian) -> Vec<u8> {
    let mut bytes = vec![8];
    bytes.extend(n64(id, endian));
    for value in [property, datatype.namespace, datatype.name] {
        bytes.extend(n32(value, endian));
    }
    bytes
}
fn append(bytes: &[u8], endian: Endian) -> Vec<u8> {
    let mut output = vec![9];
    output.extend(n32(bytes.len() as u32, endian));
    output.extend_from_slice(bytes);
    output.extend(n32(bytes.len() as u32, endian));
    output
}

enum Variation {
    Chunked,
    CrossBoundary,
    Silent,
}

fn vary_renderer_transactions(bytes: &[u8], variation: Variation) -> Vec<u8> {
    let source = Recording::from_bytes(bytes).unwrap();
    let endian = source.header.endian;
    let renderers: BTreeSet<_> = source
        .state_at(0)
        .unwrap()
        .objects
        .values()
        .filter(|object| object.type_name.ends_with("::PvdUserRenderer"))
        .map(|object| object.id)
        .collect();
    assert!(!renderers.is_empty());
    let mut decoder = Decoder::default();
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    let names: std::collections::BTreeMap<_, _> = groups
        .iter()
        .flat_map(|group| &group.events)
        .filter_map(|event| match event {
            Event::StringHandle { handle, value } => Some((*handle, value.as_str())),
            _ => None,
        })
        .collect();
    let mut source_frame = 0;
    let mut crossing_ordinal = None;
    for (ordinal, event) in groups.iter().flat_map(|group| &group.events).enumerate() {
        if source_frame == 1
            && matches!(event, Event::SetPropertyValue { id, property, .. } if renderers.contains(id) && names.get(property) == Some(&"events"))
        {
            crossing_ordinal = Some(ordinal);
        }
        if matches!(event, Event::EndSection { name, .. } if names.get(name) == Some(&"frame")) {
            source_frame += 1;
        }
    }
    assert!(crossing_ordinal.is_some());
    let mut output = bytes[..28].to_vec();
    let mut strings = std::collections::BTreeMap::new();
    let mut frame = 0;
    let mut changed = 0;
    let mut pending_end = None;
    let mut ordinal = 0;
    let mut crossed = false;
    for decoded in groups {
        let offset = decoded.offset as usize;
        let length = match endian {
            Endian::Little => u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()),
            Endian::Big => u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()),
        } as usize;
        let raw = &bytes[offset..offset + 24 + length];
        map_group_events(raw, endian, |event, payload| {
            let current_ordinal = ordinal;
            ordinal += 1;
            if let Event::StringHandle { handle, value } = event {
                strings.insert(*handle, value.clone());
            }
            let is_renderer = matches!(event, Event::SetPropertyValue { id, property, .. } if renderers.contains(id) && strings.get(property).is_some_and(|name| name == "events"));
            if let Event::SetPropertyValue { id, property, datatype, data, .. } = event
                && is_renderer {
                    match variation {
                        Variation::Silent if frame == 2 => {
                            changed += 1;
                            return Ok(None);
                        }
                        Variation::Chunked | Variation::CrossBoundary => {
                            group(&mut output, &begin(*id, *property, *datatype, endian), raw, endian);
                            for chunk in data.chunks(7) {
                                group(&mut output, &append(chunk, endian), raw, endian);
                            }
                            if matches!(variation, Variation::CrossBoundary) && Some(current_ordinal) == crossing_ordinal {
                                pending_end = Some(raw[..24].to_vec());
                                crossed = true;
                            } else {
                                group(&mut output, &[10], raw, endian);
                            }
                            changed += 1;
                            return Ok(None);
                        }
                        _ => {}
                    }
            }
            group(&mut output, payload, raw, endian);
            if matches!(event, Event::EndSection { name, .. } if strings.get(name).is_some_and(|name| name == "frame")) {
                frame += 1;
                if let Some(header) = pending_end.take() {
                    group(&mut output, &[10], &header, endian);
                }
            }
            Ok(None)
        }).unwrap();
    }
    assert!(changed > 0);
    if matches!(variation, Variation::CrossBoundary) {
        assert!(crossed);
    }
    output
}

#[test]
#[ignore = "requires real SDK joint-limit capture; transaction mutations test structure, not SDK provenance"]
fn renderer_transactions_and_silent_first_frame_have_explicit_outcomes() {
    let bytes = capture(
        "PVD_JOINT_LIMIT_CAPTURE",
        "physx-3.4-joint-limits.pvdstream",
    );
    let source = Recording::from_bytes(&bytes).unwrap();
    let chunked = vary_renderer_transactions(&bytes, Variation::Chunked);
    let chunked_source = Recording::from_bytes(&chunked).unwrap();
    for frame in 0..source.frame_count() {
        assert_eq!(
            source.state_at(frame).unwrap(),
            chunked_source.state_at(frame).unwrap()
        );
    }
    compare(&chunked, &chunked_source, 2, 5);
    let crossing = vary_renderer_transactions(&bytes, Variation::CrossBoundary);
    let crossing_source = Recording::from_bytes(&crossing).unwrap();
    assert_ne!(
        source.state_at(1).unwrap(),
        crossing_source.state_at(1).unwrap()
    );
    compare(&crossing, &crossing_source, 2, 5);
    let end_error = trim_scene_frames(&crossing, 0, 1).unwrap_err();
    assert!(
        end_error
            .to_string()
            .contains("stream ended inside a property transaction"),
        "{end_error}"
    );
    let silent = vary_renderer_transactions(&bytes, Variation::Silent);
    let silent_source = Recording::from_bytes(&silent).unwrap();
    assert!(
        silent_source
            .state_at(2)
            .unwrap()
            .debug_draw
            .values()
            .all(|batch| batch.is_empty())
    );
    let error = trim_scene_frames(&silent, 2, 5).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot preserve complete scene state"),
        "{error}"
    );
}
