use super::*;
use crate::{RecordingBuilder, analysis::Analysis};
use pvd_protocol::{Decoder, Event, EventGroup, Name};
use std::{io::Read, path::PathBuf};

#[derive(serde::Deserialize)]
enum LegacyRecord {
    Object(()),
    Debug(()),
    Contacts(()),
    Checkpoint(()),
    Frame(()),
    Analysis(Box<Analysis>),
}

struct Pair {
    resident: ResidentAnalysis,
    legacy: Box<Analysis>,
    buffers: usize,
    profile_buffers: usize,
    memory_buffers: usize,
    changed_dense_prefix_elements: usize,
}
impl Pair {
    fn property(&mut self, builder: &RecordingBuilder, id: u64, property: u32, bytes: &[u8]) {
        if builder
            .strings
            .get(&property)
            .is_none_or(|name| name != "events")
        {
            return;
        }
        let Some(kind) = builder.object_classes.get(&id) else {
            return;
        };
        if !kind.ends_with("PxProfileZone") && !kind.ends_with("PxProfileMemoryEventBuffer") {
            return;
        }
        let previous = self.resident.clone();
        let frame = builder.validation.as_ref().unwrap().frames;
        let endian = builder.recording.header.endian;
        self.resident
            .ingest(id, kind, bytes, endian, frame, &mut || false)
            .unwrap();
        self.legacy.ingest(id, kind, bytes, endian, frame).unwrap();
        self.buffers += 1;
        self.profile_buffers += usize::from(kind.ends_with("PxProfileZone"));
        self.memory_buffers += usize::from(kind.ends_with("PxProfileMemoryEventBuffer"));
        if !previous.data.spans.same(&self.resident.data.spans) {
            self.changed_dense_prefix_elements += previous.data.spans.len();
        }
        if !previous.data.values.same(&self.resident.data.values) {
            self.changed_dense_prefix_elements += previous.data.values.len();
        }
        if !previous
            .memory()
            .events
            .same(&self.resident.memory().events)
        {
            self.changed_dense_prefix_elements += previous.memory().events.len();
        }
        if !previous
            .memory()
            .samples
            .same(&self.resident.memory().samples)
        {
            self.changed_dense_prefix_elements += previous.memory().samples.len();
        }
    }
    fn message(&mut self, builder: &RecordingBuilder, id: u64, name: Name, bytes: &[u8]) {
        if builder.object_classes.get(&id).is_none_or(|kind| {
            !kind.ends_with("PxProfileZone") && !kind.ends_with("PxProfileMemoryEventBuffer")
        }) {
            return;
        }
        if let Some(message) = builder.messages.get(&name) {
            for arg in message.args.iter() {
                if builder.name(arg.datatype).unwrap().ends_with("::String") {
                    continue;
                }
                let start = arg.offset as usize;
                let end = start.checked_add(arg.size as usize).unwrap();
                self.property(builder, id, arg.property, bytes.get(start..end).unwrap());
            }
        }
    }
    fn event(&mut self, builder: &RecordingBuilder, event: &Event, stream: u64) {
        match event {
            Event::AddProfileZone { id, name } => {
                self.resident.add_zone(*id, name, &mut || false).unwrap();
                self.legacy.add_zone(*id, name.clone()).unwrap();
            }
            Event::AddProfileZoneEvent {
                id, event_id, name, ..
            } => {
                self.resident
                    .add_event(*id, *event_id, name, &mut || false)
                    .unwrap();
                self.legacy.add_event(*id, *event_id, name.clone()).unwrap();
            }
            Event::SetPropertyValue {
                id, property, data, ..
            } => self.property(builder, *id, *property, data),
            Event::EndSetPropertyValue => {
                if let Some(property) = builder.partial.get(&stream) {
                    self.property(builder, property.id, property.property, &property.data);
                }
            }
            Event::SetPropertyMessage { id, message, data } => {
                self.message(builder, *id, *message, data)
            }
            Event::SendPropertyMessageFromGroup { id, data } => {
                if let Some(message) = builder.groups.get(&stream) {
                    self.message(builder, *id, *message, data);
                }
            }
            _ => (),
        }
    }
    fn assert_wire(&self) {
        assert_eq!(
            postcard::to_stdvec(&self.resident).unwrap(),
            postcard::to_stdvec(&self.legacy).unwrap()
        );
    }
}

#[test]
#[ignore = "requires PVD_BASELINE_FIXTURE and its original PVD_RESIDENT_CAPTURE SDK stream"]
fn real_seed_continues_actual_sdk_buffers_with_bounded_tail_and_path_copies() {
    let baseline_path =
        PathBuf::from(std::env::var_os("PVD_BASELINE_FIXTURE").expect("baseline record"));
    let metadata: serde_json::Value =
        serde_json::from_slice(&std::fs::read(baseline_path.with_extension("json")).unwrap())
            .unwrap();
    let bytes = std::fs::read(&baseline_path).unwrap();
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        metadata["payload_blake3"].as_str().unwrap()
    );
    let domain = Domain::new(192 << 20);
    let resident =
        ResidentAnalysis::from_legacy_record_bytes(&bytes, domain.clone(), &mut || false).unwrap();
    let published = resident.clone();
    let LegacyRecord::Analysis(legacy) = postcard::from_bytes::<LegacyRecord>(&bytes).unwrap()
    else {
        panic!("baseline record type");
    };
    let first = metadata["baseline_first_frame"].as_u64().unwrap() as usize;
    let mut pair = Pair {
        resident,
        legacy,
        buffers: 0,
        profile_buffers: 0,
        memory_buffers: 0,
        changed_dense_prefix_elements: 0,
    };
    pair.assert_wire();
    let seed_stats = domain.statistics();
    let mut decoder = Decoder::default();
    let mut builder = None;
    let path =
        PathBuf::from(std::env::var_os("PVD_RESIDENT_CAPTURE").expect("original SDK capture"));
    let mut input = std::fs::File::open(path).unwrap();
    let mut hash = blake3::Hasher::new();
    let mut input_bytes = 0usize;
    let mut chunk = vec![0u8; 64 << 10];
    let mut checked_frames = Vec::new();
    loop {
        let count = input.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        input_bytes += count;
        hash.update(&chunk[..count]);
        let groups = decoder.feed(&chunk[..count]).unwrap();
        if builder.is_none()
            && let Some(header) = decoder.header()
        {
            let mut value = RecordingBuilder::new(header.clone());
            value.validation = Some(crate::clip::ValidationState {
                skip_analysis: true,
                ..Default::default()
            });
            builder = Some(value);
        }
        for group in groups {
            let builder = builder.as_mut().unwrap();
            for event in group.events {
                let before = builder.validation.as_ref().unwrap().frames;
                if before > first {
                    pair.event(builder, &event, group.stream_id);
                }
                builder
                    .push(EventGroup {
                        stream_id: group.stream_id,
                        timestamp: group.timestamp,
                        offset: group.offset,
                        events: vec![event],
                    })
                    .unwrap();
                let after = builder.validation.as_ref().unwrap().frames;
                if before != after && [first + 2, 151, 501, 970].contains(&after) {
                    pair.assert_wire();
                    checked_frames.push(after - 1);
                }
            }
        }
    }
    decoder.finish().unwrap();
    assert_eq!(
        input_bytes as u64,
        metadata["input_bytes"].as_u64().unwrap()
    );
    assert_eq!(
        hash.finalize().to_hex().as_str(),
        metadata["input_blake3"].as_str().unwrap()
    );
    let frames = builder
        .as_ref()
        .unwrap()
        .validation
        .as_ref()
        .unwrap()
        .frames;
    assert_eq!(frames, 970);
    assert_eq!(checked_frames, [121, 150, 500, 969]);
    assert!(pair.buffers > 1000 && pair.profile_buffers > 0 && pair.memory_buffers > 0);
    pair.assert_wire();
    assert_eq!(
        postcard::to_stdvec(&published.legacy_record()).unwrap(),
        bytes
    );
    let changed = domain.statistics();
    let copied = changed.buffer_copied_elements - seed_stats.buffer_copied_elements;
    assert!(copied < pair.changed_dense_prefix_elements / 100);
    let before_retain = pair.resident.clone();
    let before_retain_hash = blake3::hash(&postcard::to_stdvec(&before_retain).unwrap());
    let retained = pair.resident.try_retain_recent(849, &mut || false);
    let retain_supported = retained.is_ok();
    match retained {
        Ok(()) => {
            pair.legacy.retain_recent(849);
            pair.assert_wire();
        }
        Err(failure) => {
            assert!(failure.downcast_ref::<ResidentBudgetExceeded>().is_some());
            assert!(pair.resident.same(&before_retain));
            assert_eq!(pair.resident.revision(), before_retain.revision());
            assert_eq!(domain.statistics().logical_used, changed.logical_used);
            pair.assert_wire();
        }
    }
    println!(
        "resident continued Retain: first=849 supported={retain_supported} used_before={} used_after={} peak={} limit={} failure_keeps_old_frame=true",
        changed.logical_used,
        domain.statistics().logical_used,
        domain.statistics().logical_peak,
        changed.limit
    );
    assert!(
        retain_supported,
        "continued real fixture must retain within its same domain"
    );
    assert_eq!(
        blake3::hash(&postcard::to_stdvec(&before_retain).unwrap()),
        before_retain_hash
    );
    let current = pair.resident.clone();
    let current_stats = domain.statistics();
    let hold = domain
        .reserve(current_stats.limit - current_stats.logical_used)
        .unwrap();
    assert!(
        pair.resident
            .add_zone(u64::MAX, "budget", &mut || false)
            .is_err()
    );
    assert!(pair.resident.same(&current));
    assert!(
        pair.resident
            .add_zone(u64::MAX, "cancel", &mut || true)
            .is_err()
    );
    assert!(pair.resident.same(&current));
    drop(hold);
    println!(
        "resident SDK continuation: frames={frames} buffers={} profile={} memory={} copied_elements={copied} dense_changed_prefix_reference={} buffer_nodes={} tree_copied_entries={} used={} peak={}",
        pair.buffers,
        pair.profile_buffers,
        pair.memory_buffers,
        pair.changed_dense_prefix_elements,
        changed.buffer_allocated_nodes - seed_stats.buffer_allocated_nodes,
        changed.tree_copied_entries - seed_stats.tree_copied_entries,
        changed.logical_used,
        changed.logical_peak
    );
    drop((pair, current, before_retain, published));
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(domain.statistics().live_strong_cells, 0);
}

#[test]
#[ignore = "requires PVD_BASELINE_FIXTURE pointing to the frozen Analysis history record"]
fn real_retention_recount_has_an_explicit_same_domain_resource_boundary() {
    let path = PathBuf::from(std::env::var_os("PVD_BASELINE_FIXTURE").expect("baseline record"));
    let bytes = std::fs::read(path).unwrap();
    for first in [0, 1, 121] {
        let domain = Domain::new(192 << 20);
        let mut actual =
            ResidentAnalysis::from_legacy_record_bytes(&bytes, domain.clone(), &mut || false)
                .unwrap();
        let LegacyRecord::Analysis(mut expected) =
            postcard::from_bytes::<LegacyRecord>(&bytes).unwrap()
        else {
            panic!("baseline record type");
        };
        let old = actual.clone();
        let before = domain.statistics();
        actual.try_retain_recent(first, &mut || false).unwrap();
        expected.retain_recent(first);
        assert_eq!(
            postcard::to_stdvec(&actual).unwrap(),
            postcard::to_stdvec(&expected).unwrap(),
            "Retain {first}"
        );
        assert_eq!(postcard::to_stdvec(&old.legacy_record()).unwrap(), bytes);
        for frame in [0, 1, 120, 121] {
            let state = actual
                .memory()
                .try_state_at_frame(frame, &mut || false)
                .unwrap();
            assert_eq!(
                postcard::to_stdvec(&state).unwrap(),
                postcard::to_stdvec(&expected.memory.state_at_frame(frame)).unwrap()
            );
        }
        let changed = domain.statistics();
        println!(
            "resident real Retain succeeded: first={first} seed_used={} retained_used={} peak={} limit={} old_wire_equal=true",
            before.logical_used, changed.logical_used, changed.logical_peak, changed.limit
        );
        drop((old, actual));
        assert_eq!(domain.statistics().logical_used, 0);
        assert_eq!(domain.statistics().live_strong_cells, 0);
    }
}
