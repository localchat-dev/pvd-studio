use super::*;

fn prefix(bytes: &[u8], target: usize) -> Recording {
    use pvd_protocol::{Decoder, Event, EventGroup};
    let mut decoder = Decoder::default();
    let groups = decoder.feed(bytes).unwrap();
    decoder.finish().unwrap();
    let mut builder = pvd_core::RecordingBuilder::new(decoder.header().unwrap().clone());
    let mut strings = std::collections::BTreeMap::new();
    let mut frame = 0;
    for group in groups {
        for event in group.events {
            if let Event::StringHandle { handle, value } = &event {
                strings.insert(*handle, value.clone());
            }
            let boundary = matches!(&event, Event::EndSection { name, .. }
                if strings.get(name).is_some_and(|value| value == "frame"));
            builder
                .push(EventGroup {
                    stream_id: group.stream_id,
                    timestamp: group.timestamp,
                    offset: group.offset,
                    events: vec![event],
                })
                .unwrap();
            if boundary {
                if frame == target {
                    return builder.snapshot().unwrap();
                }
                frame += 1;
            }
        }
    }
    panic!("SDK source has no completed frame {target}");
}

#[test]
#[ignore = "requires PVD_RETAINED_SDK_DIRECTORY containing official PhysX 3.4 rich/all captures"]
fn actual_sdk_disk_save_reopen_and_display_match_independent_raw_prefixes() {
    let captures = PathBuf::from(
        std::env::var_os("PVD_RETAINED_SDK_DIRECTORY").expect("official SDK capture directory"),
    );
    for name in ["physx-3.4-rich.pvdstream", "physx-3.4-all.pvdstream"] {
        let path = captures.join(name);
        let bytes = std::fs::read(&path).unwrap();
        let digest = blake3::hash(&bytes);
        let root = std::env::temp_dir().join(format!(
            "pvd-retained-sdk-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let mut case = Case {
            studio: Studio::default(),
            live: None,
            root,
            context: Default::default(),
        };
        let loaded = read_recording_on_disk(
            &path,
            File::open(&path).unwrap(),
            &Arc::new(AtomicBool::new(false)),
            &AtomicU64::new(0),
            pvd_core::live::HistoryConfig {
                directory: case.root.join("history"),
                disk_budget_bytes: 128 << 20,
                segment_target_bytes: 64 << 20,
                segment_frames: 15,
                min_free_disk_bytes: 0,
            },
        )
        .unwrap();
        assert!(loaded.bytes.is_none());
        assert_eq!(loaded.recording.frame_count(), 120);
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(loaded)).unwrap();
        case.studio.job = Some(LoadJob {
            result: Mutex::new(receiver),
            cancelled: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU64::new(0)),
            label: path.display().to_string(),
            started: Instant::now(),
            live: false,
            final_snapshot: true,
        });
        case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
        assert!(case.studio.can_export_retained());
        case.studio.retained_first = 20;
        case.studio.retained_last = 60;
        let destination = case.root.join("SDK保留.pvdcapture");
        case.studio.retained_path = destination.display().to_string();
        let before = case.studio.frame;
        case.studio.export_retained(case.context.clone());
        case.wait(|studio| studio.recording_export.is_none());
        assert_eq!(case.studio.frame, before);
        assert!(destination.exists(), "{:?}", case.studio.notices);
        case.studio.clear_recording();
        let history = case.root.join("history");
        case.wait(|_| std::fs::read_dir(&history).unwrap().count() == 0);
        assert!(case.studio.recording.is_none());
        case.studio.open(destination.clone(), case.context.clone());
        case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
        assert!(case.studio.is_project_capture());
        assert_eq!(
            (
                case.studio.first_frame(),
                case.studio.count(),
                case.studio.frame
            ),
            (20, 61, 20)
        );
        for target in [20, 21, 39, 40, 59, 60, 40, 39, 20] {
            case.studio.seek(target);
            case.wait(|studio| studio.seeking.is_none());
            assert_eq!(case.studio.frame, target);
            assert!(case.studio.analysis_ready());
            let expected = prefix(&bytes, target);
            assert!(!expected.is_disk_backed());
            assert_eq!(
                case.studio.state,
                expected.state_at(target).unwrap(),
                "{name} Scene at {target}"
            );
            let displayed = &case.studio.recording.as_ref().unwrap().analysis;
            assert_eq!(
                postcard::to_allocvec(displayed).unwrap(),
                postcard::to_allocvec(&expected.analysis).unwrap(),
                "{name} displayed Analysis at {target}"
            );
            assert!(displayed.profile_summary().complete_spans > 0);
            assert!(displayed.memory_snapshot().sample_count() > 0);
        }
        assert!(
            case.studio
                .recording
                .as_ref()
                .unwrap()
                .state_at(19)
                .is_err()
        );
        assert!(!case.studio.can_save_capture());
        assert!(!case.studio.can_export_analysis());
        assert!(case.studio.complete_profile_source().is_none());
        let first_capture_bytes = std::fs::metadata(&destination).unwrap().len();
        let mut parent = destination;
        for (number, first, last, targets) in
            [(0, 30, 50, [30, 40, 50, 30]), (1, 40, 45, [45, 40, 42, 45])]
        {
            assert!(case.studio.can_export_retained());
            let parent_digest = blake3::hash(&std::fs::read(&parent).unwrap());
            let output = case.root.join(format!("SDK再次裁剪-{number}.pvdcapture"));
            case.studio.retained_first = first;
            case.studio.retained_last = last;
            case.studio.retained_path = output.display().to_string();
            let before = case.studio.frame;
            case.studio.export_retained(case.context.clone());
            case.wait(|studio| {
                studio.recording_export.is_none()
                    && studio.export_workers.cancel_all_and_check_finished()
            });
            assert!(output.exists(), "{:?}", case.studio.notices);
            assert_eq!(case.studio.frame, before);
            assert_eq!(
                blake3::hash(&std::fs::read(&parent).unwrap()),
                parent_digest
            );
            case.studio.clear_recording();
            super::reexport_tests::remove_closed_source(&mut case, &parent);
            case.studio.open(output.clone(), case.context.clone());
            case.wait(|studio| studio.job.is_none() && studio.seeking.is_none());
            assert_eq!(case.studio.first_frame(), first);
            assert_eq!(case.studio.count(), last + 1);
            assert_eq!(case.studio.frame, first);
            for target in targets {
                case.studio.seek(target);
                case.wait(|studio| studio.seeking.is_none());
                assert_eq!(case.studio.frame, target);
                assert!(case.studio.analysis_ready());
                let expected = prefix(&bytes, target);
                assert_eq!(case.studio.state, expected.state_at(target).unwrap());
                assert_eq!(
                    postcard::to_allocvec(&case.studio.recording.as_ref().unwrap().analysis)
                        .unwrap(),
                    postcard::to_allocvec(&expected.analysis).unwrap(),
                    "{name} recrop={number} Analysis at {target}"
                );
            }
            for hidden in [first - 1, last + 1] {
                let recording = case.studio.recording.as_ref().unwrap();
                assert!(recording.state_at(hidden).is_err());
                assert!(recording.analysis_at(hidden).is_err());
            }
            parent = output;
        }
        assert_eq!(blake3::hash(&std::fs::read(&path).unwrap()), digest);
        println!(
            "sdk={name} bytes={} blake3={digest} visible=20..60 targets=9 capture_bytes={first_capture_bytes} recrops=30..50,40..45 recrop_targets=8 deleted_parents=2 source_history_payloads=0",
            bytes.len(),
        );
    }
}
