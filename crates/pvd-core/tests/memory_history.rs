#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::legacy_fixture;
use pvd_core::{
    Recording,
    analysis::{Analysis, MemoryCursor, MemoryEventKind, MemoryState},
};
use pvd_protocol::Endian;
use std::{collections::BTreeMap, path::PathBuf};

fn string(handle: u32, text: &str) -> Vec<u8> {
    let mut bytes = 1u16.to_le_bytes().to_vec();
    bytes.extend((text.len() as u32 + 1).to_le_bytes());
    bytes.extend(text.bytes());
    bytes.push(0);
    bytes.extend(handle.to_le_bytes());
    bytes
}

fn allocate(size: u8, address: u8) -> Vec<u8> {
    let mut bytes = 2u16.to_le_bytes().to_vec();
    bytes.extend([size, 1, 2, 17, address]);
    bytes
}

fn free(address: u8) -> Vec<u8> {
    let mut bytes = 3u16.to_le_bytes().to_vec();
    bytes.push(address);
    bytes
}

fn ingest(analysis: &mut Analysis, buffer: u64, frame: usize, parts: &[Vec<u8>]) {
    analysis
        .ingest(
            buffer,
            "physx::PxProfileMemoryEventBuffer",
            &parts.concat(),
            Endian::Little,
            frame,
        )
        .unwrap();
}

fn fixture() -> Analysis {
    let mut analysis = Analysis::default();
    ingest(
        &mut analysis,
        7,
        0,
        &[
            string(1, "original"),
            string(2, "sample.cpp"),
            allocate(64, 10),
        ],
    );
    ingest(
        &mut analysis,
        9,
        0,
        &[
            string(1, "other buffer"),
            string(2, "sample.cpp"),
            allocate(40, 10),
        ],
    );
    ingest(
        &mut analysis,
        7,
        1,
        &[free(10), allocate(96, 10), allocate(8, 11), free(11)],
    );
    ingest(
        &mut analysis,
        7,
        2,
        &[string(1, "new handle value"), allocate(32, 10)],
    );
    ingest(&mut analysis, 7, 3, &[free(99)]);
    ingest(&mut analysis, 9, 3, &[free(10)]);
    ingest(&mut analysis, 7, 4, &[free(10)]);
    analysis
}

#[test]
fn memory_replay_preserves_allocation_lifetimes_unknown_frees_and_churn() {
    let analysis = fixture();
    let memory = &analysis.memory;
    let mut cursor = MemoryCursor::default();
    for frame in [0, 1, 2, 3, 4, 3, 2, 1, 0, 4, 2, 4, 0, 3, 1, 2] {
        let state = memory.seek(&mut cursor, frame);
        assert_eq!(state, &memory.state_at_frame(frame));
        assert_eq!(state.live_bytes, [104, 136, 72, 32, 0][frame]);
        assert_eq!(state.allocations, [2, 4, 5, 5, 5][frame]);
        assert_eq!(state.frees, [0, 2, 2, 4, 5][frame]);
        assert_eq!(state.total_freed_bytes, [0, 72, 72, 112, 144][frame]);
        assert_eq!(state.unmatched_frees, u64::from(frame >= 3));
        assert_eq!(state.replaced_addresses, u64::from(frame >= 2));
        if frame < 4 {
            let allocation = &state.live[&(7, 10)];
            assert_eq!(allocation.size, [64, 96, 32, 32][frame]);
            assert_eq!(
                &*allocation.type_name,
                if frame < 2 {
                    "original"
                } else {
                    "new handle value"
                }
            );
            assert_eq!(allocation.received_frame, frame.min(2));
        }
    }
    let one = memory.churn_at_frame(1);
    assert_eq!(
        (
            one.allocations,
            one.frees,
            one.allocated_bytes,
            one.freed_bytes
        ),
        (2, 2, 104, 72)
    );
    assert_eq!(one.live_byte_change(), 32);
    let two = memory.churn_at_frame(2);
    assert_eq!(two.replaced_addresses, 1);
    assert_eq!(two.replaced_bytes, 96);
    assert_eq!(two.live_byte_change(), -64);
    assert_eq!(memory.events_at_frame(1).len(), 4);
    assert!(memory.events_at_frame(100).is_empty());
    assert_eq!(memory.state_at_frame(100).live, memory.live);
    assert_eq!(memory.state_at_frame(100).total_allocated_bytes, 240);
    assert_eq!(memory.churn_at_frame(3).unmatched_frees, 1);
}

#[test]
fn memory_cursor_handles_growing_cloned_and_unrelated_recordings() {
    let mut original = Analysis::default();
    ingest(
        &mut original,
        7,
        0,
        &[
            string(1, "original"),
            string(2, "sample.cpp"),
            allocate(64, 10),
        ],
    );
    let mut cursor = MemoryCursor::default();
    original.memory.seek(&mut cursor, 10);
    let snapshot = original.clone();
    ingest(&mut original, 7, 1, &[free(10)]);
    assert_eq!(original.memory.seek(&mut cursor, 1).live_bytes, 0);
    assert_eq!(snapshot.memory.seek(&mut cursor, 1).live_bytes, 64);
    let mut branch = snapshot.clone();
    ingest(&mut branch, 7, 1, &[allocate(16, 10)]);
    assert_eq!(branch.memory.seek(&mut cursor, 1).live_bytes, 16);
    assert_eq!(original.memory.seek(&mut cursor, 1).live_bytes, 0);
    let unrelated = fixture();
    assert_eq!(unrelated.memory.seek(&mut cursor, 0).live_bytes, 104);
    assert_eq!(
        Analysis::default().memory.seek(&mut cursor, 0),
        &MemoryState::default()
    );
    assert!(
        original
            .ingest(
                7,
                "PxProfileMemoryEventBuffer",
                &allocate(1, 1),
                Endian::Little,
                0
            )
            .is_err()
    );
}

#[test]
#[ignore = "requires a real SDK capture with eMEMORY/eALL; defaults to the heightfield producer"]
fn real_sdk_memory_states_match_flush_samples_and_independent_event_replay() {
    let capture = std::env::var_os("PVD_MEMORY_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-heightfield.pvdstream")
        });
    let recording = Recording::from_path(capture).unwrap();
    let memory = &legacy_fixture(&recording).memory;
    assert!(!memory.events().is_empty());
    assert!(memory.allocations > 100);
    assert!(memory.frees > 100);
    let mut last_samples = BTreeMap::new();
    for sample in &memory.samples {
        last_samples.insert(sample.received_frame, sample);
    }
    let mut live = BTreeMap::new();
    let mut reference = BTreeMap::new();
    let mut seen = BTreeMap::new();
    let mut reused = 0;
    let mut position = 0;
    for (&frame, sample) in &last_samples {
        while let Some(event) = memory
            .events()
            .get(position)
            .filter(|event| event.received_frame <= frame)
        {
            let key = (event.buffer, event.address);
            match &event.kind {
                MemoryEventKind::Allocate {
                    allocation,
                    replaced,
                } => {
                    assert_eq!(live.get(&key), replaced.as_ref());
                    reused += usize::from(seen.insert(key, ()).is_some());
                    live.insert(key, allocation.clone());
                }
                MemoryEventKind::Free { allocation } => {
                    assert_eq!(live.remove(&key).as_ref(), allocation.as_ref());
                }
            }
            position += 1;
        }
        assert_eq!(
            sample.live_bytes,
            live.values().map(|allocation| allocation.size).sum::<u64>()
        );
        assert_eq!(sample.live_allocations, live.len());
        reference.insert(frame, live.clone());
    }
    assert!(
        reused > 0,
        "fixture must contain real allocator address reuse"
    );
    let mut cursor = MemoryCursor::default();
    let frames: Vec<_> = last_samples.keys().copied().collect();
    let random = (0..frames.len() * 3).map(|i| frames[(i * 53 + 17) % frames.len()]);
    for frame in frames
        .iter()
        .copied()
        .chain(frames.iter().rev().copied())
        .chain(random)
    {
        let state = memory.seek(&mut cursor, frame);
        assert_eq!(&state.live, &reference[&frame]);
        assert_eq!(state, &memory.state_at_frame(frame));
        let sample = last_samples[&frame];
        assert_eq!(state.live_bytes, sample.live_bytes);
        assert_eq!(state.allocations, sample.allocations);
        assert_eq!(state.frees, sample.frees);
    }
    let final_state = memory.state_at_frame(usize::MAX);
    assert_eq!(final_state.live, memory.live);
    assert_eq!(final_state.live_bytes, memory.live_bytes);
    assert_eq!(final_state.unmatched_frees, memory.unmatched_frees);
    assert_eq!(final_state.replaced_addresses, memory.replaced_addresses);
    assert_eq!(
        final_state.total_allocated_bytes,
        memory.total_allocated_bytes
    );
    println!(
        "{} flush frames, {} allocations, {} frees, {reused} reused addresses",
        frames.len(),
        memory.allocations,
        memory.frees
    );
}
