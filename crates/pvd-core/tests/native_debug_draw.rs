use pvd_core::{Recording, Value, debug_draw::Primitive, native};
use pvd_protocol::{Decoder, Event};
use std::{collections::BTreeMap, path::Path};

#[path = "support/joint_limits.rs"]
mod sdk_joint_limits;

fn fixture(stem: &str, suffix: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../captures/native")
        .join(format!("{stem}-paired{suffix}"))
}

#[test]
#[ignore = "requires the original PVD3 rich file and its byte-identical forwarded SDK TCP capture"]
fn original_joint_draws_match_all_raw_buffers_ids_and_replay_frames() {
    let bytes = std::fs::read(fixture("rich", "-native.pxd2")).unwrap();
    let (native, info) = native::read_with_progress(&bytes, |_, _| true).unwrap();
    let raw_bytes = std::fs::read(fixture("rich", ".pvdstream")).unwrap();
    let raw = Recording::from_bytes(&raw_bytes).unwrap();
    let renderer = info.source_ids[&5];
    let joint = info.source_ids[&20];
    let mut strings = BTreeMap::new();
    let mut frame = 0;
    let mut buffers = Vec::new();
    for group in Decoder::default().feed(&raw_bytes).unwrap() {
        for event in group.events {
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::EndSection { name, .. }
                    if strings.get(&name).is_some_and(|s| s == "frame") =>
                {
                    frame += 1;
                }
                Event::SetPropertyValue { id, data, .. } if id == renderer => {
                    buffers.push((frame, data));
                }
                _ => {}
            }
        }
    }
    assert_eq!(buffers.len(), 120);
    assert_eq!(frame, 120);
    for (frame, (received, buffer)) in buffers.iter().enumerate() {
        assert_eq!(*received, frame);
        assert_eq!(buffer.len(), 66);
        assert_eq!(buffer[0], 1);
        assert_eq!(buffer[9], 5);
        assert_eq!(u64::from_le_bytes(buffer[1..9].try_into().unwrap()), joint);
    }
    for frame in (0..120)
        .chain((0..120).rev())
        .chain((0..120).map(|i| (i * 73) % 120))
    {
        let state = native.state_at(frame).unwrap();
        let expected = raw.state_at(frame).unwrap();
        assert_eq!(state.debug_draw.len(), 1);
        assert_eq!(expected.debug_draw.len(), 1);
        let batch = &state.debug_draw[&5];
        assert_eq!(batch.len(), 1);
        assert_eq!(expected.debug_draw[&renderer].len(), 1);
        let (
            Primitive::JointFrames {
                instance,
                parent,
                child,
            },
            Primitive::JointFrames {
                instance: source,
                parent: source_parent,
                child: source_child,
            },
        ) = (&batch[0], &expected.debug_draw[&renderer][0])
        else {
            panic!("expected SDK joint frames");
        };
        assert_eq!(*instance, 20);
        assert_eq!(*source, joint);
        assert_eq!(parent, source_parent);
        assert_eq!(child, source_child);
        let Value::Bytes(saved) = &state.objects[&5].properties["events"] else {
            panic!("missing saved buffer");
        };
        assert_eq!(saved.len(), 66);
        assert_eq!(&saved[9..], &buffers[frame].1[9..]);
        assert_eq!(u64::from_le_bytes(saved[1..9].try_into().unwrap()), 20);
        let floats: Vec<_> = saved[10..]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()).to_bits())
            .collect();
        assert_eq!(parent.rotation.map(f32::to_bits), floats[0..4]);
        assert_eq!(parent.position.map(f32::to_bits), floats[4..7]);
        assert_eq!(child.rotation.map(f32::to_bits), floats[7..11]);
        assert_eq!(child.position.map(f32::to_bits), floats[11..14]);
    }
    native.verify_replay().unwrap();
    let mut callbacks = 0;
    let failure = native::from_bytes_with_progress(&bytes, |index, _| {
        if index == 145 {
            callbacks += 1;
        }
        callbacks < 3
    })
    .unwrap_err();
    assert!(failure.to_string().contains("renderer decoding cancelled"));
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(callbacks, 3);
    eprintln!(
        "120 native joint-frame buffers match SDK payloads and ID mapping; forward/reverse/random replay, frame clearing and in-buffer cancellation passed"
    );
}

#[test]
#[ignore = "requires original baseline file and exact same SDK TCP stream"]
fn original_baseline_keeps_empty_draw_state() {
    for suffix in ["-native.pxd2", ".pvdstream"] {
        let recording = Recording::from_path(fixture("baseline", suffix)).unwrap();
        assert_eq!(recording.frame_count(), 120);
        for frame in 0..120 {
            assert!(recording.state_at(frame).unwrap().debug_draw.is_empty());
        }
    }
}

fn source_instance(local: u64, source_ids: &BTreeMap<u64, u64>) -> u64 {
    if local == 0 {
        0
    } else {
        *source_ids
            .get(&local)
            .expect("missing native instance mapping")
    }
}

fn source_primitive(primitive: &Primitive, source_ids: &BTreeMap<u64, u64>) -> Primitive {
    let mut source = primitive.clone();
    let instance = match &mut source {
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
    *instance = source_instance(*instance, source_ids);
    source
}

fn source_limit_buffer(
    saved: &[u8],
    source_ids: &BTreeMap<u64, u64>,
) -> (Vec<u8>, BTreeMap<u8, usize>) {
    let mut source = saved.to_vec();
    let mut position = 0;
    let mut counts = BTreeMap::new();
    while position < source.len() {
        let kind = source[position];
        position += 1;
        let (size, count) = match kind {
            1 => {
                let local = u64::from_le_bytes(source[position..position + 8].try_into().unwrap());
                source[position..position + 8]
                    .copy_from_slice(&source_instance(local, source_ids).to_le_bytes());
                (8, 1)
            }
            4 => {
                let count = u32::from_le_bytes(source[position..position + 4].try_into().unwrap());
                (4 + 48 * count as usize, count as usize)
            }
            5 => (56, 1),
            6 => (61, 1),
            7 | 8 => (37, 1),
            9 => (33, 1),
            11 => {
                assert_eq!(
                    &source[position..position + 12],
                    &[0; 12],
                    "fixture scene bulk buffer has no points, lines or triangles"
                );
                (12, 1)
            }
            _ => panic!("unexpected renderer event {kind} in limit fixture"),
        };
        *counts.entry(kind).or_default() += count;
        position += size;
        assert!(position <= source.len(), "incomplete renderer event {kind}");
    }
    (source, counts)
}

#[test]
#[ignore = "requires original PVD3 joint-limit save, same TCP bytes and independent SDK geometry truth"]
fn original_limits_and_rgb_triangle_match_same_sdk_truth_and_replay() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/native/limits");
    let raw_bytes = std::fs::read(directory.join("joint-limits-live.pvdstream")).unwrap();
    let raw = Recording::from_bytes(&raw_bytes).unwrap();
    let truth: serde_json::Value = serde_json::from_slice(
        &std::fs::read(directory.join("joint-limits-live.joints.json")).unwrap(),
    )
    .unwrap();
    sdk_joint_limits::verify_sdk_capture(&raw, &truth);
    let (native, info) = native::read_with_progress(
        &std::fs::read(directory.join("joint-limits-live-native.pxd2")).unwrap(),
        |_, _| true,
    )
    .unwrap();
    assert_eq!(native.frame_count(), 6);
    let source_renderer = *raw.state_at(0).unwrap().debug_draw.keys().next().unwrap();
    let identity_ids = info.source_ids.values().map(|id| (*id, *id)).collect();
    let mut strings = BTreeMap::new();
    let mut completed_frames = 0;
    let mut buffers = vec![Vec::new(); 6];
    for group in Decoder::default().feed(&raw_bytes).unwrap() {
        for event in group.events {
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::EndSection { name, .. }
                    if strings.get(&name).is_some_and(|value| value == "frame") =>
                {
                    completed_frames += 1;
                }
                Event::SetPropertyValue { id, data, .. } if id == source_renderer => {
                    buffers[completed_frames].push(data);
                }
                _ => {}
            }
        }
    }
    assert_eq!(completed_frames, 6);
    for (frame, frame_buffers) in buffers.iter().enumerate() {
        assert_eq!(frame_buffers.len(), 2, "frame {frame}: renderer flushes");
        let (_, counts) = source_limit_buffer(&frame_buffers[0], &identity_ids);
        assert_eq!(counts, BTreeMap::from([(4, 1), (11, 1)]));
    }
    let frames = (0..6).chain((0..6).rev()).chain([3, 0, 5, 1, 4, 2]);
    for frame in frames {
        let saved = native.state_at(frame).unwrap();
        let sdk = raw.state_at(frame).unwrap();
        assert_eq!(saved.debug_draw.len(), 1, "native frame {frame}");
        assert_eq!(sdk.debug_draw.len(), 1, "SDK frame {frame}");
        for (renderer, primitives) in &saved.debug_draw {
            let source_renderer = source_instance(*renderer, &info.source_ids);
            assert_ne!(*renderer, source_renderer);
            let normalized: Vec<_> = primitives
                .iter()
                .map(|primitive| source_primitive(primitive, &info.source_ids))
                .collect();
            assert_eq!(
                normalized.as_slice(),
                sdk.debug_draw[&source_renderer].as_slice(),
                "frame {frame}: primitive order, IDs, pose bits, parameters and colors"
            );
            let mut frame_counts = BTreeMap::new();
            for primitive in &normalized {
                let kind = match primitive {
                    Primitive::Triangle { .. } => 4,
                    Primitive::JointFrames { .. } => 5,
                    Primitive::LinearLimit { .. } => 6,
                    Primitive::AngularLimit { .. } => 7,
                    Primitive::LimitCone { .. } => 8,
                    Primitive::DoubleCone { .. } => 9,
                    _ => panic!("unexpected primitive in joint-limit fixture"),
                };
                *frame_counts.entry(kind).or_default() += 1;
            }
            assert_eq!(
                frame_counts,
                BTreeMap::from([(4, 1), (5, 8), (6, 4), (7, 2), (8, 2), (9, 2)]),
                "frame {frame}: all flushes, four limit kinds and exactly one RGB triangle"
            );
            let Value::Bytes(saved_buffer) = &saved.objects[renderer].properties["events"] else {
                panic!("native renderer buffer missing");
            };
            let Value::Bytes(raw_buffer) = &sdk.objects[&source_renderer].properties["events"]
            else {
                panic!("SDK renderer buffer missing");
            };
            assert_eq!(raw_buffer.as_ref(), buffers[frame][1].as_ref());
            let (normalized_buffer, mut counts) =
                source_limit_buffer(saved_buffer, &info.source_ids);
            assert_eq!(
                normalized_buffer.as_slice(),
                raw_buffer.as_ref(),
                "frame {frame}: serialized fields differ beyond mapped instance IDs"
            );
            assert!(counts.remove(&1).unwrap_or_default() >= 8);
            assert_eq!(
                counts,
                BTreeMap::from([(5, 8), (6, 4), (7, 2), (8, 2), (9, 2)]),
                "frame {frame}: final flush contains joint frames and limits"
            );
        }
    }
    native.verify_replay().unwrap();
    eprintln!(
        "6 original native frames: four limit kinds, RGB triangle, IDs, all primitive fields and final-buffer bytes match same-run SDK truth; forward/reverse/random replay passed"
    );
}
