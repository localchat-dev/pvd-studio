use super::*;
use crate::analysis::resident::full_fixture_inputs;
use std::{fs, sync::atomic::Ordering};

fn populated_scene(generation: u32) -> SceneState {
    let mut scene = SceneState::default();
    for id in [7, 8] {
        Action::Create(Object {
            id,
            generation,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut scene);
    }
    Action::Set(7, "target".into(), Value::Ref(8)).apply(&mut scene);
    scene.generations.insert(99, 6);
    scene.debug_draw.insert(
        7,
        Arc::new(vec![Primitive::Point {
            instance: 7,
            position: [1.0; 3],
            color: 42,
        }]),
    );
    scene.contacts.insert(
        7,
        vec![Contact {
            position: [1.0; 3],
            normal: [0.0, 1.0, 0.0],
            shapes: [7, 8],
            separation: -0.1,
            normal_force: 1.0,
            faces: [0, 1],
            normal_force_available: true,
        }]
        .into(),
    );
    scene.errors.push(ErrorEvent {
        code: 2,
        message: "captured error".into(),
        file: "fixture".into(),
        line: 1,
        timestamp: 10,
    });
    scene.cameras.insert(
        format!("camera-{generation}"),
        SourceCamera {
            position: [generation as f32, 1.0, 2.0],
            up: [0.0, 1.0, 0.0],
            target: [0.0; 3],
        },
    );
    scene
}

fn assert_history_replacements(paged: bool, remove_generation: bool) {
    for (segment_frames, close_before_replacement) in [(2, false), (8, false), (8, true)] {
        let directory = std::env::temp_dir().join(format!(
            "pvd-scene-replacement-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let config = HistoryConfig {
            directory: directory.clone(),
            segment_frames,
            min_free_disk_bytes: 0,
            ..Default::default()
        };
        let mut history = if paged {
            HistoryWriter::new_paged(config)
        } else {
            HistoryWriter::new(config)
        }
        .unwrap();
        let domain = history
            .analysis_pages
            .as_ref()
            .map(|pages| pages.current().domain().clone());
        let mut scene = populated_scene(3);
        let mut expected = Vec::new();
        let mut analysis = Analysis::default();
        let mut expected_analysis = Vec::new();
        let buffers = full_fixture_inputs(pvd_protocol::Endian::Little);
        for (index, (id, kind, bytes)) in buffers.iter().take(6).enumerate() {
            if index == 2 && close_before_replacement {
                history.close_segment();
            }
            let patch = match index {
                0 => SceneUpdate::full(&scene),
                2 if remove_generation => {
                    scene.generations.remove(&99);
                    SceneUpdate {
                        removed_generations: [99].into(),
                        ..Default::default()
                    }
                }
                2 => {
                    scene = SceneState::default();
                    SceneUpdate::full(&scene)
                }
                4 => {
                    scene = populated_scene(9);
                    SceneUpdate::full(&scene)
                }
                _ => {
                    if scene.objects.contains_key(&7) {
                        Action::Set(7, "value".into(), Value::U64(index as u64)).apply(&mut scene);
                    }
                    SceneUpdate::capture(&scene, &[7].into(), false)
                }
            };
            let mut inputs = Vec::new();
            if index == 0 {
                inputs.extend([
                    AnalysisInput::Zone(3, "Physics".into()),
                    AnalysisInput::Event(3, 4, "simulate".into()),
                ]);
            }
            inputs.push(AnalysisInput::Buffer {
                id: *id,
                kind: (*kind).into(),
                bytes: bytes.clone().into(),
                little_endian: true,
                frame: index,
            });
            for input in &inputs {
                input.apply(&mut analysis).unwrap();
            }
            history.append(
                &FrameInfo {
                    index,
                    timestamp: index as u64,
                    scene_id: 1,
                },
                &scene,
                &patch,
                &analysis,
                inputs,
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            expected.push(scene.clone());
            expected_analysis.push(postcard::to_stdvec(&analysis).unwrap());
        }
        assert!(!analysis.memory.events().is_empty());
        assert!(!analysis.spans.is_empty());
        let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: pvd_protocol::Endian::Little,
        })
        .recording;
        recording.disk = Some(history.snapshot());
        recording.frames = recording.disk.as_ref().unwrap().frames();
        for (frame, state) in expected.iter().enumerate() {
            assert_eq!(
                recording.disk.as_ref().unwrap().state_at(frame).unwrap(),
                *state,
                "direct frame {frame}, paged {paged}, segment {segment_frames}"
            );
            let actual = recording.analysis_at(frame).unwrap();
            assert_eq!(actual.resident().is_some(), paged);
            assert_eq!(
                postcard::to_stdvec(&actual).unwrap(),
                expected_analysis[frame],
                "direct analysis frame {frame}, paged {paged}, segment {segment_frames}"
            );
        }
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        for frame in (0..6).chain((0..5).rev()).chain([4, 1, 2, 5, 0]) {
            let replay = cursor.seek(&recording, frame, || false).unwrap();
            replay.update.apply(&mut displayed);
            assert_eq!(replay.analysis.resident().is_some(), paged);
            assert_eq!(
                postcard::to_stdvec(&replay.analysis).unwrap(),
                expected_analysis[frame],
                "cursor analysis frame {frame}, paged {paged}, segment {segment_frames}"
            );
            assert_eq!(displayed, expected[frame], "cursor frame {frame}");
        }
        drop((cursor, recording, history));
        if let Some(domain) = domain {
            assert_eq!(domain.statistics().logical_used, 0);
        }
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(directory).unwrap();
    }
}

#[test]
fn legacy_history_replaces_complete_scenes() {
    assert_history_replacements(false, false);
}

#[test]
fn paged_history_replaces_complete_scenes() {
    assert_history_replacements(true, false);
}

#[test]
fn legacy_history_removes_generation_tombstones() {
    assert_history_replacements(false, true);
}

#[test]
fn paged_history_removes_generation_tombstones() {
    assert_history_replacements(true, true);
}
