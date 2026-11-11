use super::*;
use crate::{FrameInfo, Object};

fn binding(section: FrameSection, source: u64) -> FrameBinding {
    FrameBinding {
        source: FrameSourceKey::Captured { section, source },
        source_object: None,
        scene: None,
    }
}

fn frames(indices: &[usize]) -> FrameIndex {
    indices
        .iter()
        .map(|index| FrameInfo {
            index: *index,
            timestamp: *index as u64 * 100,
            scene_id: 99,
        })
        .collect()
}

fn interleaved() -> FrameDirectory {
    let mut directory = FrameDirectory::default();
    for index in 0..6 {
        let section = if index % 2 == 0 {
            FrameSection::PhysX
        } else {
            FrameSection::Apex
        };
        directory.append(index, binding(section, 10)).unwrap();
    }
    directory
}

fn snapshot(directory: &FrameDirectory) -> Vec<(FrameSourceKey, Vec<SourceFrame>)> {
    directory
        .sources()
        .map(|source| (source.source(), source.frames().copied().collect()))
        .collect()
}

#[test]
fn sdk_bindings_keep_raw_source_and_only_associate_captured_pxscene() {
    let mut state = SceneState::default();
    for (id, type_name, generation) in [(10, "physx3::PxScene", 3), (20, "Apex::scene", 7)] {
        state.objects.insert(
            id,
            Object {
                id,
                generation,
                type_name: type_name.into(),
                properties: BTreeMap::new(),
            },
        );
    }
    let physx = FrameBinding::sdk("frame", 10, &state).unwrap();
    assert_eq!(physx.source, binding(FrameSection::PhysX, 10).source);
    assert_eq!(
        physx.scene,
        Some(FrameObject {
            id: 10,
            generation: 3
        })
    );
    let apex = FrameBinding::sdk("ApexFrame", 20, &state).unwrap();
    assert_eq!(
        apex.source_object,
        Some(FrameObject {
            id: 20,
            generation: 7
        })
    );
    assert_eq!(apex.scene, None);
    let absent = FrameBinding::sdk("ApexFrame", 0, &state).unwrap();
    assert_eq!(absent.source, binding(FrameSection::Apex, 0).source);
    assert_eq!(absent.source_object, None);
    assert_eq!(absent.scene, None);
    for name in ["Frame", "apexframe", "render", ""] {
        assert!(FrameBinding::sdk(name, 10, &state).is_none());
    }
}

#[test]
fn interleaved_clocks_navigate_real_endpoints_without_merging_names() {
    let directory = interleaved();
    let physx = directory
        .source(binding(FrameSection::PhysX, 10).source)
        .unwrap();
    let apex = directory
        .source(binding(FrameSection::Apex, 10).source)
        .unwrap();
    assert_eq!(
        physx.frames().map(|frame| frame.index).collect::<Vec<_>>(),
        [0, 2, 4]
    );
    assert_eq!(
        apex.frames().map(|frame| frame.index).collect::<Vec<_>>(),
        [1, 3, 5]
    );
    assert_eq!(apex.at_ordinal(1).unwrap().index, 3);
    assert_eq!(apex.at_index(3).unwrap().ordinal, 1);
    assert!(apex.at_index(2).is_none());
    assert_eq!(apex.next(2).unwrap().index, 3);
    assert_eq!(apex.next(3).unwrap().index, 5);
    assert_eq!(apex.previous(3).unwrap().index, 1);
    assert_eq!(apex.at_or_before(3).unwrap().index, 3);
    assert_eq!(apex.at_or_before(4).unwrap().index, 3);
    assert_eq!(apex.at_or_after(2).unwrap().index, 3);
    assert!(apex.previous(1).is_none());
    assert!(apex.next(5).is_none());
    assert!(apex.at_or_before(0).is_none());
    assert_eq!(apex.last().unwrap().index, 5);
}

#[test]
fn source_object_lifetimes_are_per_boundary() {
    let mut directory = FrameDirectory::default();
    for generation in [0, 1] {
        let mut frame_binding = binding(FrameSection::Apex, 50);
        frame_binding.source_object = Some(FrameObject { id: 50, generation });
        directory
            .append(generation as usize, frame_binding)
            .unwrap();
    }
    assert_eq!(directory.sources().count(), 1);
    let source = directory.sources().next().unwrap();
    assert_eq!(source.first().unwrap().source_object.unwrap().generation, 0);
    assert_eq!(source.last().unwrap().source_object.unwrap().generation, 1);
}

#[test]
fn default_uses_first_physx_clock_by_capture_order() {
    let mut directory = FrameDirectory::default();
    directory.append(0, binding(FrameSection::Apex, 1)).unwrap();
    assert_eq!(
        directory.default_source(),
        Some(binding(FrameSection::Apex, 1).source)
    );
    directory
        .append(1, binding(FrameSection::PhysX, 90))
        .unwrap();
    directory
        .append(2, binding(FrameSection::PhysX, 10))
        .unwrap();
    assert_eq!(
        directory.default_source(),
        Some(binding(FrameSection::PhysX, 90).source)
    );
    directory.discard_before(2);
    assert_eq!(
        directory.default_source(),
        Some(binding(FrameSection::PhysX, 90).source)
    );
    let bytes = wire::encode(&directory, 1024).unwrap();
    let restored = wire::decode(&bytes, &frames(&[2]), FrameDirectoryLimits::default()).unwrap();
    assert_eq!(restored.default_source(), directory.default_source());
    let clipped = directory.clipped(2, 2).unwrap();
    assert_eq!(
        clipped.default_source(),
        Some(binding(FrameSection::PhysX, 10).source)
    );
}

#[test]
fn unretained_observations_preserve_source_ordinals_and_sparse_seek_set() {
    let key = binding(FrameSection::Apex, 10).source;
    let mut directory = FrameDirectory::default();
    for index in 0..5 {
        directory
            .observe(index, binding(FrameSection::Apex, 10), index % 2 == 0)
            .unwrap();
    }
    let source = directory.source(key).unwrap();
    assert_eq!(source.observed_frames(), 5);
    assert_eq!(
        source
            .frames()
            .map(|frame| (frame.index, frame.ordinal))
            .collect::<Vec<_>>(),
        [(0, 0), (2, 2), (4, 4)]
    );
    assert!(source.at_ordinal(1).is_none());
    assert_eq!(source.nearest_ordinal(1).unwrap().ordinal, 0);
    assert_eq!(source.nearest_ordinal(2).unwrap().ordinal, 2);
    assert_eq!(source.nearest_ordinal(3).unwrap().ordinal, 2);
    assert_eq!(source.nearest_ordinal(usize::MAX).unwrap().ordinal, 4);
    assert_eq!(source.at_ordinal(2).unwrap().index, 2);
    assert_eq!(source.next(0).unwrap().index, 2);
    directory.validate_frames(&frames(&[0, 2, 4])).unwrap();
    assert!(directory.validate_frames(&frames(&[0, 1, 4])).is_err());
    assert!(directory.validate_frames(&frames(&[0, 2, 2])).is_err());
    assert!(directory.validate_frames(&frames(&[0, 2])).is_err());
}

#[test]
fn rejected_observation_does_not_consume_an_ordinal_or_mutate_directory() {
    let mut directory = interleaved();
    let before = wire::encode(&directory, 1024 * 1024).unwrap();
    for index in [5, 7] {
        assert!(
            directory
                .append(index, binding(FrameSection::Apex, 10))
                .is_err()
        );
        assert_eq!(wire::encode(&directory, 1024 * 1024).unwrap(), before);
    }
    directory
        .append(6, binding(FrameSection::Apex, 10))
        .unwrap();
    assert_eq!(directory.frame(6).unwrap().1.ordinal, 3);
}

#[test]
fn rolling_retention_reclaims_index_blocks_and_preserves_snapshot_and_ordinals() {
    let mut directory = FrameDirectory::default();
    let frame_binding = binding(FrameSection::PhysX, 10);
    for index in 0..1600 {
        directory.append(index, frame_binding).unwrap();
    }
    let snapshot = directory.clone();
    let before = directory.index_bytes();
    directory.discard_before(1500);
    assert!(directory.index_bytes() < before);
    assert_eq!(
        directory
            .source(frame_binding.source)
            .unwrap()
            .first()
            .unwrap()
            .ordinal,
        1500
    );
    assert_eq!(snapshot.retained_frames(), 1600);
    assert_eq!(
        snapshot
            .source(frame_binding.source)
            .unwrap()
            .first()
            .unwrap()
            .index,
        0
    );
    directory.discard_before(1600);
    assert_eq!(directory.retained_frames(), 0);
    assert!(
        directory
            .source(frame_binding.source)
            .unwrap()
            .nearest_ordinal(10)
            .is_none()
    );
    assert_eq!(directory.default_source(), Some(frame_binding.source));
    assert!(directory.index_bytes() < 4096);
    directory.append(1600, frame_binding).unwrap();
    assert_eq!(directory.frame(1600).unwrap().1.ordinal, 1600);
}

#[test]
fn clipping_keeps_sparse_actual_boundaries_and_source_ordinals() {
    let mut directory = interleaved();
    directory
        .observe(6, binding(FrameSection::PhysX, 10), false)
        .unwrap();
    directory
        .append(7, binding(FrameSection::Apex, 10))
        .unwrap();
    directory
        .append(8, binding(FrameSection::PhysX, 10))
        .unwrap();
    directory.discard_before(2);
    let clipped = directory.clipped(3, 7).unwrap();
    assert_eq!(
        clipped.index_bytes(),
        directory.clipped_index_bytes(3, 7).unwrap()
    );
    clipped.validate_frames(&frames(&[3, 4, 5, 7])).unwrap();
    assert_eq!(clipped.frame(3).unwrap().1.ordinal, 1);
    assert_eq!(clipped.frame(7).unwrap().1.ordinal, 3);
    assert!(clipped.frame(6).is_none());
    assert!(directory.clipped(7, 3).is_err());
    assert_eq!(directory.clipped(9, 99).unwrap().retained_frames(), 0);
    let apex_only = directory.clipped(7, 7).unwrap();
    assert_eq!(apex_only.sources().count(), 1);
    assert_eq!(
        apex_only.default_source(),
        Some(binding(FrameSection::Apex, 10).source)
    );
}

#[test]
fn legacy_frames_keep_the_existing_axis_without_fabricating_raw_sources() {
    let old_frames = frames(&[50, 52, 53]);
    let mut directory =
        FrameDirectory::from_legacy(&old_frames, FrameDirectoryLimits::default()).unwrap();
    assert_eq!(directory.default_source(), Some(FrameSourceKey::Legacy));
    assert_eq!(directory.frame(52).unwrap().1.ordinal, 52);
    assert_eq!(directory.frame(52).unwrap().1.source_object, None);
    assert_eq!(directory.frame(52).unwrap().1.scene, None);
    assert!(
        directory
            .append(54, binding(FrameSection::PhysX, 99))
            .is_err()
    );
    assert!(interleaved().append(6, FrameBinding::legacy()).is_err());
    let info = FrameInfo {
        index: 1,
        timestamp: 2,
        scene_id: 3,
    };
    assert_eq!(postcard::to_stdvec(&info).unwrap(), [1, 2, 3]);
    let restored: FrameInfo = postcard::from_bytes(&[1, 2, 3]).unwrap();
    assert_eq!(
        (restored.index, restored.timestamp, restored.scene_id),
        (1, 2, 3)
    );
}

#[test]
fn byte_preflight_and_limit_failures_are_transactional() {
    let frame_binding = binding(FrameSection::PhysX, 10);
    let mut measure = FrameDirectory::default();
    let first_bytes = measure
        .index_bytes_after_observe(0, frame_binding, true)
        .unwrap();
    assert_eq!(measure.retained_frames(), 0);
    measure.append(0, frame_binding).unwrap();
    assert_eq!(measure.index_bytes(), first_bytes);
    let mut directory = FrameDirectory::new(FrameDirectoryLimits {
        index_bytes: first_bytes,
        ..FrameDirectoryLimits::default()
    });
    directory.append(0, frame_binding).unwrap();
    assert!(directory.append(1, frame_binding).is_err());
    assert_eq!(directory.retained_frames(), 1);
    assert_eq!(directory.index_bytes(), first_bytes);
    directory.observe(1, frame_binding, false).unwrap();
    assert_eq!(directory.retained_frames(), 1);
    let mut count_limited = FrameDirectory::new(FrameDirectoryLimits {
        sources: 1,
        frames: 1,
        ..FrameDirectoryLimits::default()
    });
    count_limited.append(0, frame_binding).unwrap();
    assert!(count_limited.append(1, frame_binding).is_err());
    assert!(
        count_limited
            .observe(1, binding(FrameSection::Apex, 10), false)
            .is_err()
    );
    count_limited.observe(1, frame_binding, false).unwrap();
}

#[test]
fn directory_roundtrip_preserves_sparse_rows_lifetimes_and_unretained_tail() {
    let mut directory = FrameDirectory::default();
    for index in 0..8 {
        let mut frame_binding = binding(
            if index % 2 == 0 {
                FrameSection::PhysX
            } else {
                FrameSection::Apex
            },
            0,
        );
        frame_binding.source_object = Some(FrameObject {
            id: index as u64 + 100,
            generation: 2,
        });
        if index % 2 == 0 {
            frame_binding.scene = frame_binding.source_object;
        }
        directory
            .observe(index, frame_binding, ![2, 7].contains(&index))
            .unwrap();
    }
    directory.discard_before(1);
    let actual_frames = frames(&[1, 3, 4, 5, 6]);
    let bytes = wire::encode(&directory, 1024 * 1024).unwrap();
    let mut restored =
        wire::decode(&bytes, &actual_frames, FrameDirectoryLimits::default()).unwrap();
    assert_eq!(snapshot(&restored), snapshot(&directory));
    assert_eq!(wire::encode(&restored, 1024 * 1024).unwrap(), bytes);
    restored.append(8, binding(FrameSection::Apex, 0)).unwrap();
    assert_eq!(restored.frame(8).unwrap().1.ordinal, 4);
    let clipped = directory.clipped(3, 5).unwrap();
    let bytes = wire::encode(&clipped, 1024 * 1024).unwrap();
    let restored =
        wire::decode(&bytes, &frames(&[3, 4, 5]), FrameDirectoryLimits::default()).unwrap();
    assert_eq!(snapshot(&clipped), snapshot(&restored));
}

#[test]
fn empty_retained_directory_roundtrip_preserves_observed_count() {
    let mut directory = FrameDirectory::default();
    directory
        .observe(0, binding(FrameSection::Apex, 4), false)
        .unwrap();
    let bytes = wire::encode(&directory, 1024).unwrap();
    let mut restored = wire::decode(
        &bytes,
        &FrameIndex::default(),
        FrameDirectoryLimits::default(),
    )
    .unwrap();
    restored.append(1, binding(FrameSection::Apex, 4)).unwrap();
    assert_eq!(restored.frame(1).unwrap().1.ordinal, 1);
}

#[test]
fn directory_decode_rejects_unknown_versions_duplicates_missing_rows_and_bad_ordinals() {
    let mut directory = FrameDirectory::default();
    for index in 0..3 {
        directory
            .append(index, binding(FrameSection::PhysX, 5))
            .unwrap();
    }
    let valid_frames = frames(&[0, 1, 2]);
    let bytes = wire::encode(&directory, 1024).unwrap();
    for (offset, value) in [(0, 2u8), (25, 3), (58, 9), (78, 4)] {
        let mut corrupted = bytes.clone();
        corrupted[offset] = value;
        assert!(wire::decode(&corrupted, &valid_frames, FrameDirectoryLimits::default()).is_err());
    }
    let mut duplicate_index = bytes.clone();
    duplicate_index[83..91].copy_from_slice(&0u64.to_le_bytes());
    assert!(
        wire::decode(
            &duplicate_index,
            &valid_frames,
            FrameDirectoryLimits::default()
        )
        .is_err()
    );
    let mut duplicate_ordinal = bytes.clone();
    duplicate_ordinal[91..99].copy_from_slice(&0u64.to_le_bytes());
    assert!(
        wire::decode(
            &duplicate_ordinal,
            &valid_frames,
            FrameDirectoryLimits::default()
        )
        .is_err()
    );
    assert!(wire::decode(&bytes, &frames(&[0, 1, 3]), FrameDirectoryLimits::default()).is_err());
    assert!(wire::decode(&bytes, &frames(&[0, 1]), FrameDirectoryLimits::default()).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(wire::decode(&trailing, &valid_frames, FrameDirectoryLimits::default()).is_err());
    for length in 0..bytes.len() {
        assert!(
            wire::decode(
                &bytes[..length],
                &valid_frames,
                FrameDirectoryLimits::default()
            )
            .is_err()
        );
    }
    let both = interleaved();
    let mut duplicate_source = wire::encode(&both, 1024).unwrap();
    let duplicate_key = duplicate_source[25..34].to_vec();
    duplicate_source[58..67].copy_from_slice(&duplicate_key);
    assert!(
        wire::decode(
            &duplicate_source,
            &frames(&[0, 1, 2, 3, 4, 5]),
            FrameDirectoryLimits::default()
        )
        .is_err()
    );
}

#[test]
fn encode_decode_and_clipping_honor_cancellation_and_byte_limits() {
    let directory = interleaved();
    let bytes = wire::encode(&directory, 1024).unwrap();
    assert!(wire::encode(&directory, bytes.len() - 1).is_err());
    assert!(wire::encode_with_cancel(&directory, 1024, || true).is_err());
    let mut polls = 0;
    assert!(
        wire::encode_with_cancel(&directory, 1024, || {
            polls += 1;
            polls == 5
        })
        .is_err()
    );
    assert!(
        wire::decode_with_cancel(
            &bytes,
            &frames(&[0, 1, 2, 3, 4, 5]),
            FrameDirectoryLimits::default(),
            || true
        )
        .is_err()
    );
    assert!(
        wire::decode(
            &bytes,
            &frames(&[0, 1, 2, 3, 4, 5]),
            FrameDirectoryLimits {
                index_bytes: 64,
                ..FrameDirectoryLimits::default()
            }
        )
        .is_err()
    );
    assert!(directory.clipped_with_cancel(1, 5, || true).is_err());
    assert!(
        FrameDirectory::from_legacy_with_cancel(
            &frames(&[0, 1, 2]),
            FrameDirectoryLimits::default(),
            || true
        )
        .is_err()
    );
    let mut polls = 0;
    assert!(
        directory
            .clipped_with_cancel(1, 5, || {
                polls += 1;
                polls == 3
            })
            .is_err()
    );
    assert_eq!(directory.retained_frames(), 6);
}
