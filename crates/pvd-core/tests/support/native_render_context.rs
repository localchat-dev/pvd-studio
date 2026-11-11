use super::*;

fn totals_match(actual: &pvd_core::NativeMemoryTotals, expected: &pvd_core::NativeMemoryTotals) {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.nonnegative_bytes(), expected.nonnegative_bytes());
    for index in 0..expected.len() {
        assert_eq!(actual.row(index), expected.row(index));
    }
}

#[test]
#[ignore = "requires the original limits container and its exact SDK stream and geometry truth"]
fn actual_limits_middle_restores_renderer_context_and_preserves_source_and_sdk_states() {
    let path = limit("-native.pxd2");
    let source = std::fs::read(&path).unwrap();
    let initial = native_initial::NativeInitialValues::parse(&source);
    let (original, _) = native::read_with_progress(&source, |_, _| true).unwrap();
    let raw_bytes = std::fs::read(limit(".pvdstream")).unwrap();
    let raw = Recording::from_bytes(&raw_bytes).unwrap();
    let truth = serde_json::from_slice(&std::fs::read(limit(".joints.json")).unwrap()).unwrap();
    sdk_joint_limits::verify_sdk_capture(&raw, &truth);
    assert_eq!((original.frame_count(), raw.frame_count()), (6, 6));
    let mut compared_properties = 0;
    for (first, last) in [(1, 2), (2, 4), (4, 5)] {
        let mut bytes = Vec::new();
        let report = trim_scene_frames(&source, first, last, &mut bytes, || true).unwrap();
        let (clipped, info) = native::read_with_progress(&bytes, |_, _| true).unwrap();
        let sdk_interval = pvd_core::clip::trim_scene_frames(&raw_bytes, first, last).unwrap();
        let sdk_interval = Recording::from_bytes(&sdk_interval).unwrap();
        assert_eq!(clipped.frame_count(), last - first + 1);
        totals_match(
            &clipped.native_initial_memory_totals().unwrap().unwrap(),
            &original
                .native_memory_totals_at(&original.state_at(first - 1).unwrap())
                .unwrap()
                .unwrap(),
        );
        for frame in (0..clipped.frame_count())
            .chain((0..clipped.frame_count()).rev())
            .chain([last - first, 0, last - first, 0])
        {
            let actual = clipped.state_at(frame).unwrap();
            let expected = original.state_at(first + frame).unwrap();
            assert_eq!(actual.objects, expected.objects);
            assert_eq!(actual.cameras, expected.cameras);
            assert_eq!(actual.errors, expected.errors);
            assert_eq!(actual.debug_draw, expected.debug_draw);
            assert_eq!(actual.contacts, expected.contacts);
            assert_eq!(
                clipped.frames[frame].timestamp,
                original.frames[first + frame].timestamp
            );
            assert_eq!(
                clipped.frames[frame].scene_id,
                original.frames[first + frame].scene_id
            );
            assert_eq!(
                actual
                    .debug_draw
                    .values()
                    .flat_map(|batch| batch.iter())
                    .filter(|primitive| matches!(primitive, Primitive::Triangle { .. }))
                    .count(),
                1
            );
            compared_properties +=
                check_frame_state(&clipped, &sdk_interval, frame, &info.source_ids, &initial);
            totals_match(
                &clipped.native_memory_totals_at(&actual).unwrap().unwrap(),
                &original
                    .native_memory_totals_at(&expected)
                    .unwrap()
                    .unwrap(),
            );
        }
        native_memory_interval::verify_partial_interval(
            &clipped,
            &raw,
            first,
            last,
            &info.source_ids,
        );
        let expected_spans: Vec<_> = analysis(&original)
            .spans
            .iter()
            .filter(|span| (first..=last).contains(&span.received_frame))
            .map(|span| {
                (
                    span.zone,
                    span.event,
                    span.thread,
                    span.context,
                    span.start,
                    span.end,
                    span.received_frame - first,
                )
            })
            .collect();
        let spans: Vec<_> = analysis(&clipped)
            .spans
            .iter()
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
            .collect();
        assert_eq!(spans, expected_spans);
        let expected_values: Vec<_> = analysis(&original)
            .values
            .iter()
            .filter(|value| (first..=last).contains(&value.received_frame))
            .map(|value| {
                (
                    value.zone,
                    value.event,
                    value.thread,
                    value.context,
                    value.raw_value,
                    value.received_frame - first,
                )
            })
            .collect();
        let values: Vec<_> = analysis(&clipped)
            .values
            .iter()
            .map(|value| {
                (
                    value.zone,
                    value.event,
                    value.thread,
                    value.context,
                    value.raw_value,
                    value.received_frame,
                )
            })
            .collect();
        assert_eq!(values, expected_values);
        clipped.verify_replay().unwrap();
        let mut rewritten = Vec::new();
        rewrite(&bytes, &mut rewritten, || true).unwrap();
        assert_eq!(rewritten, bytes);
        if let Some(directory) = std::env::var_os("PVD_NATIVE_RENDER_CONTEXT_DIRECTORY") {
            let directory = PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join(format!("limits-{first}-{last}-middle.pxd2")),
                &bytes,
            )
            .unwrap();
        }
        eprintln!("limits renderer context {first}..{last}: {report:?}");
    }
    assert!(compared_properties > 1000);
    assert_eq!(std::fs::read(path).unwrap(), source);
}
