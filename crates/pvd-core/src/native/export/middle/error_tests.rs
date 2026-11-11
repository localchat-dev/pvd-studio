use super::*;

#[derive(Clone, Copy)]
enum Step<'a> {
    Create(u32),
    Append(u32, u32, u32),
    Destroy(u32, &'a [u32]),
    Begin(u64),
    End(u64),
}

fn record(index: u32) -> Vec<u8> {
    [4u32, 25, 26, 42, index]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

fn push(encoded: &mut Vec<Vec<u8>>, event: HistoryEvent<'_>) {
    let mut bytes = Vec::new();
    event.encode(&mut bytes).unwrap();
    encoded.push(bytes);
}

fn fixture(steps: &[Step<'_>]) -> Vec<u8> {
    let source = history_fixture("UserCustomedEvent", &record(0), &[], &[]);
    let mut layout = Layout::parse(&source, &mut || true).unwrap();
    let mut encoded = Vec::new();
    push(&mut encoded, HistoryEvent::Marker);
    push(&mut encoded, HistoryEvent::Create(1, 0));
    let mut containers = BTreeSet::from([1]);
    let mut rows = Vec::new();
    let mut begin = None;
    let mut saved_position = 4;
    layout.saved.clear();
    for step in steps {
        match *step {
            Step::Create(id) => {
                assert!(containers.insert(id));
                push(&mut encoded, HistoryEvent::Create(id, 6));
            }
            Step::Append(id, slot, index) => {
                let bytes = record(index);
                push(
                    &mut encoded,
                    HistoryEvent::Array {
                        kind: 5,
                        id,
                        property: 4,
                        index: slot,
                        data: Some(&bytes),
                        undo: vec![],
                    },
                );
            }
            Step::Destroy(id, indices) => {
                let bytes = indices
                    .iter()
                    .flat_map(|&index| record(index))
                    .collect::<Vec<_>>();
                let mut instance = RawInstance::create(&layout.metadata, 6).unwrap();
                instance.set(&layout.metadata, 4, &bytes).unwrap();
                let body = instance.body(&layout.metadata).unwrap();
                let body_len = body.len() as u64;
                layout.saved.push(Saved {
                    id,
                    position: saved_position,
                    class: 6,
                    body: body.into(),
                    deleted: true,
                });
                push(
                    &mut encoded,
                    HistoryEvent::Saved {
                        id,
                        position: saved_position,
                    },
                );
                saved_position += body_len + 12;
                push(&mut encoded, HistoryEvent::Destroy(id));
            }
            Step::Begin(ticks) => {
                assert!(begin.replace((ticks, encoded.len() as u64)).is_none());
                encoded.push(section(ticks, true));
            }
            Step::End(ticks) => {
                let (start_ticks, start) = begin.take().unwrap();
                rows.push([start_ticks, start, ticks, encoded.len() as u64]);
                encoded.push(section(ticks, false));
            }
        }
    }
    assert!(begin.is_none());
    layout.events = encoded
        .iter()
        .enumerate()
        .map(|(ordinal, bytes)| {
            HistoryEvent::parse(bytes, ordinal as u64, &layout.metadata, &mut 0).unwrap()
        })
        .collect();
    layout.communication.sections[0].rows = rows;
    layout.communication.instances = containers
        .iter()
        .map(|&id| Instance {
            id,
            source: u64::from(id) * 0x111,
            flags: [0; 10],
        })
        .collect();
    layout.communication.reverse = containers
        .iter()
        .map(|&id| (u64::from(id) * 0x111, id))
        .collect();
    encode_modified(&mut layout)
}

fn persist(label: &str, source: &[u8], output: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_CONSTRUCTED_EXPORT_DIRECTORY") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join(format!("error-baseline-{label}-source.pxd2")),
            source,
        )
        .unwrap();
        std::fs::write(
            directory.join(format!("error-baseline-{label}-trimmed.pxd2")),
            output,
        )
        .unwrap();
    }
}

fn trim(label: &str, source: &[u8], first: usize, last: usize) -> (Vec<u8>, crate::Recording) {
    trim_with_omitted_generations(label, source, first, last, &[])
}

fn trim_with_omitted_generations(
    label: &str,
    source: &[u8],
    first: usize,
    last: usize,
    omitted_generations: &[u64],
) -> (Vec<u8>, crate::Recording) {
    let (original, _) = crate::native::read_with_progress(source, |_, _| true).unwrap();
    let mut output = Vec::new();
    trim_scene_frames(source, first, last, &mut output, || true).unwrap();
    let (restored, _) = crate::native::read_with_progress(&output, |_, _| true).unwrap();
    assert_eq!(restored.frame_count(), last - first + 1);
    for frame in (0..restored.frame_count())
        .chain((0..restored.frame_count()).rev())
        .chain([0, last - first, 0])
    {
        let actual = restored.state_at(frame).unwrap();
        let mut expected = original.state_at(frame + first).unwrap();
        for id in omitted_generations {
            assert!(!expected.objects.contains_key(id));
            assert!(!actual.objects.contains_key(id));
            assert_eq!(expected.generations.remove(id), Some(0));
            assert!(!actual.generations.contains_key(id));
        }
        assert_eq!(actual, expected, "{label} frame {frame}");
    }
    restored.verify_replay().unwrap();
    let mut rewritten = Vec::new();
    rewrite(&output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten, output);
    persist(label, source, &output);
    (output, restored)
}

fn initial(recording: &crate::Recording) -> Vec<(u64, u32, u32)> {
    recording
        .initial_untimed_errors()
        .iter()
        .map(|error| {
            assert_eq!(error.code, 4);
            assert_eq!(error.message, "Error");
            assert_eq!(error.file, "Source.cpp");
            assert_eq!(error.line, 42);
            (
                error.container_instance,
                error.container_generation,
                error.index,
            )
        })
        .collect()
}

#[test]
fn error_snapshot_uses_history_position_for_interframe_records_and_excludes_tail() {
    let source = fixture(&[
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Begin(100),
        Step::End(101),
        Step::Append(3, 1, 8),
        Step::Begin(200),
        Step::Append(3, 2, 9),
        Step::End(201),
        Step::Begin(300),
        Step::Append(3, 3, 10),
        Step::End(301),
        Step::Append(3, 4, 11),
    ]);
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    assert!(original.initial_untimed_errors().is_empty());
    assert_eq!(
        original
            .untimed_error_events()
            .iter()
            .map(|error| (error.index, error.received_frame, error.history_event))
            .collect::<Vec<_>>(),
        vec![(7, 0, 3), (8, 1, 6), (9, 1, 8), (10, 2, 11), (11, 3, 13)]
    );
    let (_, restored) = trim("interframe-tail", &source, 1, 2);
    assert_eq!(initial(&restored), vec![(3, 0, 7), (3, 0, 8)]);
    assert_eq!(
        restored
            .untimed_error_events()
            .iter()
            .map(|error| (error.index, error.received_frame, error.history_event))
            .collect::<Vec<_>>(),
        vec![(9, 0, 2), (10, 1, 5)]
    );
}

#[test]
fn error_snapshot_keeps_container_order_duplicates_and_source_indices() {
    let source = fixture(&[
        Step::Create(4),
        Step::Append(4, 0, 50),
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Append(4, 1, 50),
        Step::Append(3, 1, 9),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::Append(3, 2, 9),
        Step::Append(4, 2, 51),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let (_, restored) = trim("multiple-duplicates", &source, 1, 2);
    assert_eq!(
        initial(&restored),
        vec![(3, 0, 7), (3, 0, 9), (4, 0, 50), (4, 0, 50)]
    );
    assert_eq!(
        restored
            .untimed_error_events()
            .iter()
            .map(|error| (error.container_instance, error.index, error.received_frame))
            .collect::<Vec<_>>(),
        vec![(3, 9, 0), (4, 51, 0)]
    );
}

#[test]
fn error_snapshot_excludes_containers_destroyed_before_the_start() {
    let source = fixture(&[
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Destroy(3, &[7]),
        Step::Create(4),
        Step::Append(4, 0, 9),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::Append(4, 1, 11),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let (_, restored) = trim_with_omitted_generations("destroyed-before", &source, 1, 2, &[3]);
    assert_eq!(initial(&restored), vec![(4, 0, 9)]);
    assert_eq!(restored.untimed_error_events().len(), 1);
    assert_eq!(restored.untimed_error_events()[0].index, 11);
}

#[test]
fn error_snapshot_remains_available_after_a_retained_container_is_destroyed() {
    let source = fixture(&[
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::Append(3, 1, 9),
        Step::Destroy(3, &[7, 9]),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let (_, restored) = trim("destroyed-retained", &source, 1, 2);
    assert_eq!(initial(&restored), vec![(3, 0, 7)]);
    assert_eq!(restored.untimed_error_events().len(), 1);
    assert_eq!(restored.untimed_error_events()[0].index, 9);
    for frame in [1, 0, 1] {
        assert!(!restored.state_at(frame).unwrap().objects.contains_key(&3));
        assert_eq!(initial(&restored), vec![(3, 0, 7)]);
    }
}

#[test]
fn initial_errors_do_not_create_occurrences_with_or_without_completed_frames() {
    let source = fixture(&[
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let (output, restored) = trim("only-before", &source, 1, 2);
    assert_eq!(initial(&restored), vec![(3, 0, 7)]);
    assert!(restored.untimed_error_events().is_empty());
    let mut layout = Layout::parse(&output, &mut || true).unwrap();
    layout.events.truncate(1);
    layout.communication.sections[0].rows.clear();
    let snapshot_only = encode_modified(&mut layout);
    let (restored, _) = crate::native::read_with_progress(&snapshot_only, |_, _| true).unwrap();
    assert_eq!(restored.frame_count(), 0);
    assert_eq!(initial(&restored), vec![(3, 0, 7)]);
    assert!(restored.untimed_error_events().is_empty());
    persist("snapshot-only", &source, &snapshot_only);
}

#[test]
fn initial_error_proof_rejects_changed_snapshot_payloads() {
    let source = fixture(&[
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::Append(3, 1, 9),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&layout, 1, 2, &mut || true).unwrap();
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let (output, restored) = trim("proof-payload", &source, 1, 2);
    proof.verify(&restored, &original, 1, &mut || true).unwrap();
    let mut layout = Layout::parse(&output, &mut || true).unwrap();
    let index = layout
        .saved
        .iter()
        .position(|saved| saved.id == 3 && !saved.deleted)
        .unwrap();
    let mut body = RawInstance::restore(&layout.metadata, &layout.saved[index]).unwrap();
    body.values.get_mut(&4).unwrap()[16..20].copy_from_slice(&99u32.to_le_bytes());
    layout.saved[index].body = body.body(&layout.metadata).unwrap().into();
    let tampered = encode_modified(&mut layout);
    let (restored, _) = crate::native::read_with_progress(&tampered, |_, _| true).unwrap();
    let error = proof
        .verify(&restored, &original, 1, &mut || true)
        .unwrap_err();
    assert!(error.to_string().contains("initial error"), "{error}");
}

#[test]
fn initial_error_proof_rejects_missing_extra_and_reordered_snapshot_records() {
    let source = fixture(&[
        Step::Create(3),
        Step::Append(3, 0, 7),
        Step::Append(3, 1, 9),
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&layout, 1, 2, &mut || true).unwrap();
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let (output, _) = trim("proof-sequence", &source, 1, 2);
    for indices in [vec![7], vec![7, 9, 11], vec![9, 7]] {
        let mut layout = Layout::parse(&output, &mut || true).unwrap();
        let index = layout
            .saved
            .iter()
            .position(|saved| saved.id == 3 && !saved.deleted)
            .unwrap();
        let mut body = RawInstance::restore(&layout.metadata, &layout.saved[index]).unwrap();
        let bytes = indices
            .iter()
            .flat_map(|&index| record(index))
            .collect::<Vec<_>>();
        body.set(&layout.metadata, 4, &bytes).unwrap();
        layout.saved[index].body = body.body(&layout.metadata).unwrap().into();
        let tampered = encode_modified(&mut layout);
        let (restored, _) = crate::native::read_with_progress(&tampered, |_, _| true).unwrap();
        let error = proof
            .verify(&restored, &original, 1, &mut || true)
            .unwrap_err();
        assert!(
            error.to_string().contains("initial error"),
            "{indices:?}: {error}"
        );
    }
}

#[test]
fn initial_error_proof_checks_the_source_container_independently() {
    let steps = |id| {
        vec![
            Step::Create(id),
            Step::Append(id, 0, 7),
            Step::Begin(100),
            Step::End(101),
            Step::Begin(200),
            Step::End(201),
            Step::Begin(300),
            Step::End(301),
        ]
    };
    let source = fixture(&steps(3));
    let foreign = fixture(&steps(4));
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&layout, 1, 2, &mut || true).unwrap();
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let (_, restored) = trim("proof-container", &foreign, 1, 2);
    let error = proof
        .verify(&restored, &original, 1, &mut || true)
        .unwrap_err();
    assert!(error.to_string().contains("initial error"), "{error}");
}

#[test]
fn initial_error_proof_observes_cancellation_while_grouping_source_records() {
    let mut steps = vec![Step::Create(3)];
    for slot in 0..100 {
        steps.push(Step::Append(3, slot, 7));
    }
    steps.extend([
        Step::Begin(100),
        Step::End(101),
        Step::Begin(200),
        Step::End(201),
        Step::Begin(300),
        Step::End(301),
    ]);
    let source = fixture(&steps);
    let layout = Layout::parse(&source, &mut || true).unwrap();
    let proof = history::prepare(&layout, 1, 2, &mut || true).unwrap();
    let (original, _) = crate::native::read_with_progress(&source, |_, _| true).unwrap();
    let (_, restored) = trim("proof-cancel", &source, 1, 2);
    let mut checks = 0;
    let error = proof
        .verify(&restored, &original, 1, &mut || {
            checks += 1;
            checks < 8
        })
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert_eq!(checks, 8);
}
