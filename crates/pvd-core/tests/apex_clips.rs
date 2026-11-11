use pvd_core::{
    Recording,
    clip::{TrimPhase, trim_scene_frames, trim_scene_frames_streaming},
    frame_sources::{FrameSection, FrameSourceKey},
};
use pvd_protocol::{Decoder, Endian, Event, rewrite::map_group_events};
use std::{
    collections::BTreeMap,
    io::{self, Cursor},
    path::PathBuf,
};

fn word(output: &mut Vec<u8>, value: u32, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn long(output: &mut Vec<u8>, value: u64, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn string(handle: u32, text: &str, endian: Endian) -> Vec<u8> {
    let mut output = vec![1];
    word(&mut output, text.len() as u32 + 1, endian);
    output.extend(text.as_bytes());
    output.push(0);
    word(&mut output, handle, endian);
    output
}

fn section(begin: bool, source: u64, name: u32, timestamp: u64, endian: Endian) -> Vec<u8> {
    let mut output = vec![if begin { 18 } else { 19 }];
    long(&mut output, source, endian);
    word(&mut output, name, endian);
    long(&mut output, timestamp, endian);
    output
}

fn camera(name: &str, value: f32, endian: Endian) -> Vec<u8> {
    let mut output = vec![23];
    word(&mut output, name.len() as u32 + 1, endian);
    output.extend(name.as_bytes());
    output.push(0);
    for component in [value, 2., 3., 0., 1., 0., 4., 5., 6.] {
        word(&mut output, component.to_bits(), endian);
    }
    output
}

fn group(output: &mut Vec<u8>, events: &[Vec<u8>], stream: u64, endian: Endian) {
    word(
        output,
        events.iter().map(Vec::len).sum::<usize>() as u32,
        endian,
    );
    word(output, events.len() as u32, endian);
    long(output, stream, endian);
    long(output, 123_456 + stream, endian);
    for event in events {
        output.extend(event);
    }
}

fn header(endian: Endian) -> Vec<u8> {
    let mut output = Vec::new();
    word(&mut output, pvd_protocol::STREAM_MAGIC, endian);
    word(&mut output, 1, endian);
    long(&mut output, 3, endian);
    long(&mut output, 7, endian);
    word(&mut output, 1, endian);
    group(
        &mut output,
        &[string(1, "frame", endian), string(2, "ApexFrame", endian)],
        1,
        endian,
    );
    output
}

fn nested_fixture(endian: Endian, end_only: bool) -> Vec<u8> {
    let mut output = header(endian);
    for step in 0..4 {
        if !end_only {
            group(
                &mut output,
                &[section(true, 0, 2, 2000 + step * 97, endian)],
                7,
                endian,
            );
            group(
                &mut output,
                &[section(true, 0, 1, 100 + step * 23, endian)],
                11,
                endian,
            );
        }
        group(
            &mut output,
            &[
                camera("PhysX", step as f32 * 10. + 1., endian),
                section(false, 0, 1, 111 + step * 23, endian),
            ],
            13,
            endian,
        );
        group(
            &mut output,
            &[
                camera("APEX", step as f32 * 10. + 2., endian),
                section(false, 0, 2, 2010 + step * 97, endian),
            ],
            17,
            endian,
        );
    }
    group(
        &mut output,
        &[
            section(true, 0, 2, 9999, endian),
            camera("tail only", 999., endian),
        ],
        23,
        endian,
    );
    output
}

fn renderer_data(position: f32, endian: Endian) -> Vec<u8> {
    let mut data = vec![1];
    long(&mut data, 17, endian);
    data.push(2);
    word(&mut data, 1, endian);
    for value in [position, 0., 0.] {
        word(&mut data, value.to_bits(), endian);
    }
    word(&mut data, 0xffabcdef, endian);
    data
}

fn renderer_property(position: f32, endian: Endian) -> Vec<u8> {
    let data = renderer_data(position, endian);
    let mut event = vec![7];
    long(&mut event, 17, endian);
    for value in [5, 3, 6, data.len() as u32, data.len() as u32] {
        word(&mut event, value, endian);
    }
    event.extend(data);
    event
}

fn renderer_fixture(endian: Endian, crossing_transaction: bool) -> Vec<u8> {
    let mut output = header(endian);
    let mut instance = vec![6];
    word(&mut instance, 3, endian);
    word(&mut instance, 4, endian);
    long(&mut instance, 17, endian);
    group(
        &mut output,
        &[
            string(3, "physx3", endian),
            string(4, "PvdUserRenderer", endian),
            string(5, "events", endian),
            string(6, "PvdU8", endian),
            instance,
        ],
        1,
        endian,
    );
    let append = |data: &[u8]| {
        let mut event = vec![9];
        word(&mut event, data.len() as u32, endian);
        event.extend(data);
        word(&mut event, data.len() as u32, endian);
        event
    };
    for step in 0..3 {
        group(
            &mut output,
            &[
                section(true, 20, 2, step * 100 + 1, endian),
                section(true, 10, 1, step * 100 + 2, endian),
            ],
            1,
            endian,
        );
        if step == 0 && crossing_transaction {
            let mut begin = vec![8];
            long(&mut begin, 17, endian);
            for value in [5, 3, 6] {
                word(&mut begin, value, endian);
            }
            let data = renderer_data(1., endian);
            group(
                &mut output,
                &[
                    begin,
                    append(&data[..7]),
                    section(false, 10, 1, 10, endian),
                    append(&data[7..]),
                    vec![10],
                ],
                1,
                endian,
            );
        } else {
            group(
                &mut output,
                &[
                    renderer_property(step as f32 * 10. + 1., endian),
                    section(false, 10, 1, step * 100 + 10, endian),
                ],
                1,
                endian,
            );
        }
        group(
            &mut output,
            &[
                renderer_property(step as f32 * 10. + 2., endian),
                section(false, 20, 2, step * 100 + 20, endian),
            ],
            1,
            endian,
        );
    }
    output
}

fn renderer_points(recording: &Recording, index: usize) -> Vec<f32> {
    recording
        .state_at(index)
        .unwrap()
        .debug_draw
        .get(&17)
        .into_iter()
        .flat_map(|batch| batch.iter())
        .map(|primitive| {
            let pvd_core::debug_draw::Primitive::Point { position, .. } = primitive else {
                panic!("point renderer input");
            };
            position[0]
        })
        .collect()
}

#[test]
fn renderer_preroll_uses_the_previous_end_of_the_same_source_and_whole_transactions() {
    for endian in [Endian::Little, Endian::Big] {
        for crossing in [false, true] {
            let bytes = renderer_fixture(endian, crossing);
            let source = Recording::from_bytes(&bytes).unwrap();
            assert_eq!(renderer_points(&source, 1), [1., 2.]);
            assert_eq!(renderer_points(&source, 3), [11., 12.]);
            let expected_physx = if crossing {
                vec![1., 2., 11.]
            } else {
                vec![2., 11.]
            };
            assert_eq!(renderer_points(&source, 2), expected_physx);
            let physx = compare(&bytes, 2, 2);
            assert_eq!(
                renderer_points(&Recording::from_bytes(&physx).unwrap(), 0),
                expected_physx
            );
            let apex = compare(&bytes, 3, 3);
            assert_eq!(
                renderer_points(&Recording::from_bytes(&apex).unwrap(), 0),
                [11., 12.]
            );
            let prefix = compare(&bytes, 0, 5);
            assert_eq!(non_begin_prefix(&prefix, 5), non_begin_prefix(&bytes, 5));
            let failure = trim_scene_frames(&bytes, 2, 3).unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains("cannot preserve complete scene state")
            );
            let failure = trim_scene_frames_streaming(
                &mut Cursor::new(&bytes),
                &mut Vec::new(),
                2,
                3,
                |_| true,
            )
            .unwrap_err();
            assert!(
                failure
                    .to_string()
                    .contains("cannot preserve complete scene state")
            );
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Boundary {
    begin: bool,
    source: u64,
    name: String,
    timestamp: u64,
}

fn boundaries(bytes: &[u8]) -> Vec<Boundary> {
    let mut decoder = Decoder::default();
    let mut strings = BTreeMap::new();
    let mut result = Vec::new();
    for group in decoder.feed(bytes).unwrap() {
        for event in group.events {
            match event {
                Event::StringHandle { handle, value } => {
                    strings.insert(handle, value);
                }
                Event::BeginSection {
                    id,
                    name,
                    timestamp,
                }
                | Event::EndSection {
                    id,
                    name,
                    timestamp,
                } if strings
                    .get(&name)
                    .is_some_and(|name| FrameSection::from_name(name).is_some()) =>
                {
                    result.push(Boundary {
                        begin: matches!(event, Event::BeginSection { .. }),
                        source: id,
                        name: strings[&name].clone(),
                        timestamp,
                    });
                }
                _ => {}
            }
        }
    }
    decoder.finish().unwrap();
    result
}

fn compare(source_bytes: &[u8], first: usize, last: usize) -> Vec<u8> {
    let source = Recording::from_bytes(source_bytes).unwrap();
    let output = trim_scene_frames(source_bytes, first, last).unwrap();
    let mut streamed = Vec::new();
    let report = trim_scene_frames_streaming(
        &mut Cursor::new(source_bytes),
        &mut streamed,
        first,
        last,
        |_| true,
    )
    .unwrap();
    assert_eq!(streamed, output);
    assert_eq!(report.source_blake3, *blake3::hash(source_bytes).as_bytes());
    assert_eq!(report.source_frames, source.frame_count());
    assert_eq!(report.output_frames, last - first + 1);
    let actual = Recording::from_bytes(&output).unwrap();
    assert_eq!(actual.frame_count(), last - first + 1);
    let expected_ends: Vec<_> = boundaries(source_bytes)
        .into_iter()
        .filter(|boundary| !boundary.begin)
        .skip(first)
        .take(last - first + 1)
        .collect();
    let actual_ends: Vec<_> = boundaries(&output)
        .into_iter()
        .filter(|boundary| !boundary.begin)
        .collect();
    assert_eq!(actual_ends, expected_ends);
    let mut ordinals = BTreeMap::new();
    for index in (0..actual.frame_count())
        .rev()
        .chain(0..actual.frame_count())
    {
        assert_eq!(
            actual.state_at(index).unwrap(),
            source.state_at(first + index).unwrap()
        );
        let expected = source.frame_sources.frame(first + index).unwrap();
        let frame = actual.frame_sources.frame(index).unwrap();
        assert_eq!(frame.0, expected.0);
        assert_eq!(frame.1.source_object, expected.1.source_object);
        assert_eq!(frame.1.scene, expected.1.scene);
    }
    for index in 0..actual.frame_count() {
        let (key, frame) = actual.frame_sources.frame(index).unwrap();
        let ordinal = ordinals.entry(key).or_insert(0);
        assert_eq!(frame.ordinal, *ordinal);
        *ordinal += 1;
    }
    actual.verify_replay().unwrap();
    output
}

#[test]
fn nested_sections_keep_the_selected_raw_pairs_and_rebase_each_clock() {
    for endian in [Endian::Little, Endian::Big] {
        let bytes = nested_fixture(endian, false);
        for (first, last) in [(0, 0), (1, 1), (1, 2), (2, 2), (3, 6), (7, 7), (0, 7)] {
            let output = compare(&bytes, first, last);
            let mut open = BTreeMap::new();
            for boundary in boundaries(&output) {
                let key = (boundary.source, boundary.name);
                if boundary.begin {
                    assert!(open.insert(key, boundary.timestamp).is_none());
                } else {
                    assert!(open.remove(&key).is_some());
                }
            }
            assert!(open.is_empty());
            assert!(
                !Recording::from_bytes(&output)
                    .unwrap()
                    .state_at(last - first)
                    .unwrap()
                    .cameras
                    .contains_key("tail only")
            );
        }
        let apex_only = compare(&bytes, 1, 1);
        assert_eq!(
            boundaries(&apex_only),
            vec![
                Boundary {
                    begin: true,
                    source: 0,
                    name: "ApexFrame".into(),
                    timestamp: 2000
                },
                Boundary {
                    begin: false,
                    source: 0,
                    name: "ApexFrame".into(),
                    timestamp: 2010
                },
            ]
        );
        let middle = compare(&bytes, 1, 6);
        let repeated = compare(&middle, 1, 3);
        let direct = compare(&bytes, 2, 4);
        let repeated = Recording::from_bytes(&repeated).unwrap();
        let direct = Recording::from_bytes(&direct).unwrap();
        for index in 0..3 {
            assert_eq!(
                repeated.state_at(index).unwrap(),
                direct.state_at(index).unwrap()
            );
        }
    }
}

#[test]
fn end_only_streams_do_not_receive_invented_section_begins_or_tail_ends() {
    let bytes = nested_fixture(Endian::Little, true);
    let output = compare(&bytes, 2, 6);
    assert!(boundaries(&output).iter().all(|boundary| !boundary.begin));
    let recording = Recording::from_bytes(&output).unwrap();
    assert_eq!(recording.frame_sources.sources().count(), 2);
    assert!(
        recording
            .frame_sources
            .source(FrameSourceKey::Captured {
                section: FrameSection::PhysX,
                source: 0
            })
            .is_some()
    );
    assert!(
        recording
            .frame_sources
            .source(FrameSourceKey::Captured {
                section: FrameSection::Apex,
                source: 0
            })
            .is_some()
    );
}

#[test]
fn overlapping_same_source_and_name_is_rejected_even_across_streams() {
    for second_stream in [1, 2] {
        let mut bytes = header(Endian::Little);
        group(
            &mut bytes,
            &[section(true, 11, 2, 100, Endian::Little)],
            1,
            Endian::Little,
        );
        group(
            &mut bytes,
            &[
                section(true, 11, 2, 101, Endian::Little),
                section(false, 11, 2, 102, Endian::Little),
            ],
            second_stream,
            Endian::Little,
        );
        assert!(
            trim_scene_frames(&bytes, 0, 0)
                .unwrap_err()
                .to_string()
                .contains("overlapping")
        );
        let mut output = Vec::new();
        let failure =
            trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, 0, 0, |_| true)
                .unwrap_err();
        assert!(failure.to_string().contains("overlapping"));
        assert!(output.is_empty());
    }
}

#[test]
fn an_unmatched_begin_is_not_reassigned_to_another_source_end() {
    let mut bytes = header(Endian::Little);
    group(
        &mut bytes,
        &[
            section(true, 11, 2, 100, Endian::Little),
            section(false, 12, 2, 101, Endian::Little),
        ],
        1,
        Endian::Little,
    );
    let output = compare(&bytes, 0, 0);
    assert_eq!(
        boundaries(&output),
        vec![Boundary {
            begin: false,
            source: 12,
            name: "ApexFrame".into(),
            timestamp: 101
        }]
    );
}

#[test]
fn section_boundary_plan_and_cancellation_remain_bounded() {
    let bytes = nested_fixture(Endian::Little, false);
    for phase in [TrimPhase::Inspect, TrimPhase::Export] {
        let mut output = Vec::new();
        let failure =
            trim_scene_frames_streaming(&mut Cursor::new(&bytes), &mut output, 1, 5, |progress| {
                !(progress.phase == phase && progress.bytes_read > 28)
            })
            .unwrap_err();
        assert_eq!(
            failure.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        if phase == TrimPhase::Inspect {
            assert!(output.is_empty());
        }
    }
    let mut many = header(Endian::Little);
    for source in 0..65_537 {
        group(
            &mut many,
            &[section(true, source, 2, source, Endian::Little)],
            1,
            Endian::Little,
        );
    }
    group(
        &mut many,
        &[section(false, 0, 2, 99_999, Endian::Little)],
        1,
        Endian::Little,
    );
    let mut output = Vec::new();
    let failure = trim_scene_frames_streaming(&mut Cursor::new(&many), &mut output, 0, 0, |_| true)
        .unwrap_err();
    assert!(failure.to_string().contains("boundary budget"));
    assert!(output.is_empty());
}

fn non_begin_prefix(bytes: &[u8], last: usize) -> Vec<(u64, u64, Vec<u8>)> {
    let mut decoder = Decoder::default();
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    let endian = decoder.header().unwrap().endian;
    let mut strings = BTreeMap::new();
    let mut frames = 0usize;
    let mut result = Vec::new();
    for group in groups {
        let offset = group.offset as usize;
        let size: [u8; 4] = bytes[offset..offset + 4].try_into().unwrap();
        let size = match endian {
            Endian::Little => u32::from_le_bytes(size),
            Endian::Big => u32::from_be_bytes(size),
        } as usize;
        map_group_events(&bytes[offset..offset + 24 + size], endian, |event, raw| {
            if frames > last { return Ok(None); }
            if let Event::StringHandle { handle, value } = event { strings.insert(*handle, value.clone()); }
            if !matches!(event, Event::BeginSection { .. }) { result.push((group.stream_id, group.timestamp, raw.to_vec())); }
            if matches!(event, Event::EndSection { name, .. } if strings.get(name).is_some_and(|name| FrameSection::from_name(name).is_some())) { frames += 1; }
            Ok(None)
        }).unwrap();
        if frames > last {
            break;
        }
    }
    result
}

#[test]
#[ignore = "requires official PhysX 3.4.2/APEX 1.4 sync and async debug captures in PVD_APEX_SYNC_CAPTURE and PVD_APEX_ASYNC_CAPTURE"]
fn real_apex_prefixes_preserve_raw_payloads_endpoints_and_complete_frame_states() {
    for variable in ["PVD_APEX_SYNC_CAPTURE", "PVD_APEX_ASYNC_CAPTURE"] {
        let path = PathBuf::from(std::env::var_os(variable).expect(variable));
        let bytes = std::fs::read(path).unwrap();
        let recording = Recording::from_bytes(&bytes).unwrap();
        assert!(recording.frame_sources.sources().any(|source| matches!(
            source.source(),
            FrameSourceKey::Captured {
                section: FrameSection::Apex,
                ..
            }
        )));
        assert!(recording.frame_sources.sources().any(|source| matches!(
            source.source(),
            FrameSourceKey::Captured {
                section: FrameSection::PhysX,
                ..
            }
        )));
        for last in [0, 1, recording.frame_count() - 1] {
            let output = compare(&bytes, 0, last);
            assert_eq!(
                non_begin_prefix(&output, last),
                non_begin_prefix(&bytes, last)
            );
        }
    }
}
