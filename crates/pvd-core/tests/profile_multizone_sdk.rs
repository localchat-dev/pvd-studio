use pvd_core::{Recording, clip};
use serde::Deserialize;
use std::{collections::BTreeMap, fs};

#[derive(Deserialize)]
struct Zone {
    label: usize,
    id: u64,
    name: String,
}
#[derive(Deserialize)]
struct Call {
    sequence: usize,
    zone: usize,
    frame: usize,
    kind: String,
    event: u16,
    lower: u64,
    upper: u64,
    start_sequence: usize,
}
#[derive(Deserialize)]
struct Flush {
    zone: usize,
    frame: usize,
    hex: String,
}
#[derive(Deserialize)]
struct Truth {
    sdk: String,
    frames: usize,
    context: u64,
    thread: u32,
    zones: Vec<Zone>,
    calls: Vec<Call>,
    flushes: Vec<Flush>,
}

fn uint(data: &[u8], cursor: &mut usize, width: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes[..width].copy_from_slice(&data[*cursor..*cursor + width]);
    *cursor += width;
    u64::from_le_bytes(bytes)
}

fn sdk_ticks(truth: &Truth) -> Vec<u64> {
    let mut ticks = vec![None; truth.calls.len()];
    let mut per_zone = [0usize; 2];
    let expected = (0..2)
        .map(|zone| {
            truth
                .calls
                .iter()
                .filter(|call| call.zone == zone)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for flush in &truth.flushes {
        assert!(flush.zone < 2 && flush.frame < 8);
        assert!(flush.hex.len().is_multiple_of(2));
        let bytes = flush
            .hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let mut cursor = 0;
        let mut context = None;
        let mut timestamp = 0u64;
        while cursor < bytes.len() {
            let kind = uint(&bytes, &mut cursor, 1);
            let flags = uint(&bytes, &mut cursor, 1) as usize;
            let event = uint(&bytes, &mut cursor, 2) as u16;
            assert!(
                (1..=4).contains(&kind),
                "unexpected custom SDK profile kind"
            );
            if kind <= 2 {
                let thread = uint(&bytes, &mut cursor, 4) as u32;
                let id = uint(&bytes, &mut cursor, 1 << ((flags >> 2) & 3));
                let cpu = uint(&bytes, &mut cursor, 1);
                let priority = uint(&bytes, &mut cursor, 1);
                assert_eq!((cpu, priority), (0, 0));
                context = Some((thread, id));
            }
            assert_eq!(context, Some((truth.thread, truth.context)));
            let width = 1 << (flags & 3);
            let encoded = uint(&bytes, &mut cursor, width);
            timestamp = if width == 8 {
                encoded
            } else {
                timestamp.checked_add(encoded).unwrap()
            };
            let call = expected[flush.zone][per_zone[flush.zone]];
            assert_eq!(call.kind == "start", kind == 1 || kind == 3);
            assert_eq!((call.event, call.frame), (event, flush.frame));
            assert!((call.lower..=call.upper).contains(&timestamp));
            assert!(ticks[call.sequence].replace(timestamp).is_none());
            per_zone[flush.zone] += 1;
        }
    }
    for zone in 0..2 {
        assert_eq!(per_zone[zone], expected[zone].len());
    }
    ticks.into_iter().map(Option::unwrap).collect()
}

#[test]
#[ignore = "requires PVD_MULTIZONE_SDK_CAPTURE and PVD_MULTIZONE_SDK_TRUTH"]
fn actual_sdk_same_name_zones_keep_separate_spans_and_pending_starts() {
    let bytes = fs::read(std::env::var_os("PVD_MULTIZONE_SDK_CAPTURE").unwrap()).unwrap();
    let truth: Truth = serde_json::from_slice(
        &fs::read(std::env::var_os("PVD_MULTIZONE_SDK_TRUTH").unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        (truth.sdk.as_str(), truth.frames, truth.calls.len()),
        ("3.4.2", 8, 19)
    );
    assert_eq!(truth.zones.len(), 2);
    assert_eq!((truth.zones[0].label, truth.zones[1].label), (0, 1));
    assert_ne!(truth.zones[0].id, truth.zones[1].id);
    assert_eq!(truth.zones[0].name, truth.zones[1].name);
    for (index, call) in truth.calls.iter().enumerate() {
        assert_eq!(call.sequence, index);
        assert!(call.zone < 2 && call.frame < 8 && call.lower <= call.upper);
        assert!(matches!(call.kind.as_str(), "start" | "stop"));
        if call.kind == "stop" {
            let start = &truth.calls[call.start_sequence];
            assert_eq!(start.kind, "start");
            assert_eq!((start.zone, start.event), (call.zone, call.event));
            assert!(start.sequence < call.sequence && start.frame <= call.frame);
        }
    }
    let ticks = sdk_ticks(&truth);
    let source = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(source.frame_count(), 8);
    source.verify_replay().unwrap();
    let custom = |zone: u64| truth.zones.iter().any(|expected| expected.id == zone);
    for last in 0..8 {
        let output = clip::trim_scene_frames(&bytes, 0, last).unwrap();
        let recording = Recording::from_bytes(&output).unwrap();
        assert_eq!(recording.frame_count(), last + 1);
        recording.verify_replay().unwrap();
        let analysis = recording.analysis.legacy().unwrap();
        for zone in &truth.zones {
            assert_eq!(analysis.zones[&zone.id].name, zone.name);
            assert_eq!(
                analysis.zones[&zone.id].events[&17],
                "PvdStudio.Multi.Shared"
            );
            assert_eq!(
                analysis.zones[&zone.id].events[&18],
                "PvdStudio.Multi.Pending"
            );
            assert!(!analysis.profile_metadata_ambiguous(zone.id, 17));
        }
        let mut expected_spans = Vec::new();
        let mut closed = std::collections::BTreeSet::new();
        for call in truth
            .calls
            .iter()
            .filter(|call| call.frame <= last && call.kind == "stop")
        {
            let start = &truth.calls[call.start_sequence];
            assert!(closed.insert(start.sequence));
            expected_spans.push((
                truth.zones[call.zone].id,
                call.event,
                truth.thread,
                truth.context,
                ticks[start.sequence],
                ticks[call.sequence],
                call.frame,
            ));
        }
        let mut actual_spans = analysis
            .spans
            .iter()
            .filter(|span| custom(span.zone))
            .map(|span| {
                (
                    span.zone,
                    span.event,
                    span.thread,
                    span.context,
                    span.start,
                    span.end,
                    span.received_frame,
                )
            })
            .collect::<Vec<_>>();
        actual_spans.sort_unstable();
        expected_spans.sort_unstable();
        assert_eq!(actual_spans, expected_spans, "last {last}");
        let mut expected_pending = BTreeMap::<_, Vec<_>>::new();
        for call in truth.calls.iter().filter(|call| {
            call.frame <= last && call.kind == "start" && !closed.contains(&call.sequence)
        }) {
            expected_pending
                .entry((
                    truth.zones[call.zone].id,
                    call.event,
                    truth.thread,
                    truth.context,
                ))
                .or_default()
                .push(ticks[call.sequence]);
        }
        let actual_pending = analysis
            .open_event_starts()
            .filter(|(key, _)| custom(key.0))
            .map(|(key, starts)| (*key, starts.to_vec()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(actual_pending, expected_pending, "last {last}");
        if last == 1 {
            assert_eq!(actual_pending.len(), 1);
            assert_eq!(actual_pending.keys().next().unwrap().0, truth.zones[1].id);
        }
        if last == 7 {
            assert_eq!(actual_spans.len(), 9);
            assert_eq!(
                actual_pending.keys().copied().collect::<Vec<_>>(),
                [(truth.zones[1].id, 18, truth.thread, truth.context)]
            );
        }
        for frame in (0..=last).rev() {
            assert_eq!(
                recording.state_at(frame).unwrap().objects,
                source.state_at(frame).unwrap().objects
            );
        }
    }
}
