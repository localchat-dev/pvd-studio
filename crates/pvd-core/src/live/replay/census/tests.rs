use super::*;

fn object() -> Object {
    Object {
        id: 7,
        generation: 3,
        type_name: "physx3::PxScene".into(),
        properties: BTreeMap::new(),
    }
}

#[test]
fn counts_distinguish_local_reuse_cross_call_ids_and_shared_reference_clones() {
    let recorder = Recorder::start();
    let object = object();
    let array = Arc::new(vec![Value::Ref(1), Value::Ref(2)]);
    let value = Value::Array(array.clone());
    let owners = Arc::strong_count(&array);
    for frame in 0..2 {
        recorder
            .call("forward", frame, || {
                update(1);
                object_read();
                object_decoded(&object);
                decoded_value();
                super::value(&object, "Actors", RESOURCE_ID + 1, false, &value);
                super::value(&object, "ActorsAgain", RESOURCE_ID + 1, true, &value);
                decoded_references();
                references(&object, "Actors", RESOURCE_ID + 2, false, 2);
                decoded_value();
                super::value(
                    &object,
                    "legacy",
                    17,
                    false,
                    &Value::Bytes(Arc::from([1u8, 2])),
                );
                Ok::<_, ()>(())
            })
            .unwrap();
    }
    assert_eq!(Arc::strong_count(&array), owners);
    let report = recorder.finish();
    assert_eq!(report.calls.len(), 2);
    assert_eq!(report.calls[0].counts.repeated_high_ids_from_prior_calls, 0);
    assert_eq!(report.calls[1].counts.repeated_high_ids_from_prior_calls, 2);
    for call in report.calls {
        assert_eq!(call.counts.value_uses, 3);
        assert_eq!(call.counts.value_decodes, 2);
        assert_eq!(call.counts.value_local_hits, 1);
        assert_eq!(call.counts.reference_entries_cloned, 0);
        assert_eq!(call.counts.reference_arc_clones, 1);
        assert_eq!(call.counts.reference_entries_behind_cloned_arcs, 2);
        assert_eq!(call.counts.array_arc_clones, 2);
        assert_eq!(call.counts.array_items_behind_cloned_arcs, 4);
        assert_eq!(call.counts.low_file_offset_uses, 1);
    }
}

#[test]
fn detailed_rows_and_names_are_bounded_without_truncating_aggregate_counts() {
    let recorder = Recorder::start();
    let object = object();
    let property = "属性".repeat(200);
    recorder
        .call("bounded", 0, || {
            for id in 0..MAX_DETAILS + 3 {
                super::value(
                    &object,
                    &property,
                    RESOURCE_ID + id as u64,
                    false,
                    &Value::U64(1),
                );
            }
            Ok::<_, ()>(())
        })
        .unwrap();
    let report = recorder.finish();
    assert_eq!(report.details.len(), MAX_DETAILS);
    assert!(
        report
            .details
            .iter()
            .all(|detail| detail.property.len() <= NAME_BYTES)
    );
    assert_eq!(report.calls[0].counts.value_uses, (MAX_DETAILS + 3) as u64);
    assert_eq!(report.calls[0].counts.detail_rows_dropped, 3);
    assert_eq!(report.calls[0].counts.names_truncated, MAX_DETAILS as u64);
    assert_eq!(
        report.calls[0].counts.lookback_evictions,
        (MAX_DETAILS + 3 - LOOKBACK_ENTRIES) as u64
    );
}

#[test]
#[ignore = "requires fixed SDK 970-frame input and an exclusive new output directory; counts only"]
fn actual_970_scene_external_record_census() {
    let input =
        PathBuf::from(std::env::var_os("PVD_SHARED_HISTORY_RAW").expect("fixed SDK raw path"));
    let output = PathBuf::from(
        std::env::var_os("PVD_SCENE_CENSUS_OUTPUT").expect("new census output directory"),
    );
    let bytes = std::fs::read(&input).unwrap();
    assert_eq!(bytes.len(), 35_817_266);
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60"
    );
    std::fs::create_dir(&output).unwrap();
    let configuration = |name: &str| HistoryConfig {
        directory: output.join(name),
        disk_budget_bytes: 8 << 30,
        segment_target_bytes: 64 << 20,
        segment_frames: 40,
        min_free_disk_bytes: 512 << 20,
    };
    let mut candidate = crate::LiveRecording::with_history(configuration("history")).unwrap();
    let mut reference = crate::LiveRecording::with_legacy_history(configuration("legacy")).unwrap();
    for chunk in bytes.chunks(64 << 10) {
        candidate.feed(chunk).unwrap();
        reference.feed(chunk).unwrap();
    }
    let candidate = candidate.finish().unwrap();
    let reference = reference.finish().unwrap();
    assert_eq!(candidate.frame_count(), 970);
    assert_eq!(reference.frame_count(), 970);
    for recording in [&candidate, &reference] {
        let stats = recording.disk_usage().unwrap();
        assert!(stats.warning.is_none(), "{:?}", stats.warning);
        assert_eq!(stats.first_frame, 0);
        assert_eq!(stats.retained_frames, 970);
    }
    let disk = candidate.disk.as_ref().unwrap();
    let boundary = disk
        .segments
        .iter()
        .map(|segment| segment.frames[0].info.index)
        .find(|&first| first > 150 && first + 2 < 969)
        .expect("interior actual segment boundary");
    let resources = disk.segments.front().unwrap().resources.clone();
    let recorder = Recorder::start();
    let mut targets = Vec::new();
    for seed in [150, 229, 800, boundary - 2] {
        recorder.reset_lookback();
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        cursor
            .seek_scene(&candidate, seed - 1, || false)
            .unwrap()
            .update
            .apply_owned(&mut displayed);
        for (direction, frame) in (seed..seed + 5)
            .map(|frame| ("forward", frame))
            .chain((seed..seed + 4).rev().map(|frame| ("reverse", frame)))
        {
            let actual = recorder
                .call(direction, frame, || {
                    cursor.seek_scene(&candidate, frame, || false)
                })
                .unwrap();
            actual.update.apply_owned(&mut displayed);
            assert_eq!(
                displayed,
                reference.state_at(frame).unwrap(),
                "scene {direction} {frame}"
            );
            targets.push((direction, frame));
        }
        let full = cursor.seek(&candidate, seed, || false).unwrap();
        full.update.apply_owned(&mut displayed);
        assert_eq!(displayed, reference.state_at(seed).unwrap());
        assert_eq!(
            postcard::to_stdvec(&full.analysis).unwrap(),
            postcard::to_stdvec(&reference.analysis_at(seed).unwrap()).unwrap()
        );
    }
    let report = recorder.finish();
    assert_eq!(report.calls.len(), 36);
    assert!(report.calls.iter().all(|call| {
        call.counts.reference_entries_cloned == 0
            && call.counts.reference_arc_clones == call.counts.reference_uses
    }));
    assert!(
        report
            .calls
            .iter()
            .all(|call| call.success && call.counts.update_calls == 1)
    );
    assert!(
        report
            .calls
            .iter()
            .any(|call| call.segment_first == Some(boundary))
    );
    assert!(
        report
            .calls
            .iter()
            .all(|call| call.route == "adjacent_forward" || call.route == "undo")
    );
    assert!(
        report
            .calls
            .iter()
            .map(|call| call.counts.external_value_hits + call.counts.external_reference_hits)
            .sum::<u64>()
            > 0
    );
    for call in &report.calls {
        assert_eq!(
            call.counts.value_uses,
            call.counts.value_local_hits
                + call.counts.value_decodes
                + call.counts.external_value_hits
        );
        assert_eq!(
            call.counts.reference_uses,
            call.counts.reference_local_hits
                + call.counts.reference_decodes
                + call.counts.external_reference_hits
        );
        assert_eq!(
            call.counts.external_validated_reads,
            call.counts.external_misses
                + call.counts.external_value_hits
                + call.counts.external_reference_hits
        );
        assert_eq!(call.counts.reference_entries_cloned, 0);
        assert!(call.counts.external_entries <= 64);
        assert!(call.counts.external_allowance_bytes <= 8 << 20);
    }
    let result = serde_json::json!({
        "input_bytes": bytes.len(), "source_frames": 970, "actual_boundary": boundary,
        "targets": targets, "complete_scene_comparisons": 36, "scene_to_full_wire_comparisons": 4,
        "independent_sdk_oracle": false, "census": report,
        "scope": "Same input Legacy comparator; source/resource identities remain fixed. No timing. Bounded decoded-external cache preserves full resource validation. Warm-up and integrity comparisons are outside the census calls.",
    });
    drop((candidate, reference));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    drop(resources);
    for name in ["history", "legacy"] {
        assert_eq!(std::fs::read_dir(output.join(name)).unwrap().count(), 0);
    }
    std::fs::write(
        output.join("census.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!(
        "PVD_SCENE_CENSUS calls=36 complete_scenes=36 full_upgrades=4 remaining_history_entries=0"
    );
}
