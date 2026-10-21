use super::*;
use crate::{RecordingBuilder, clip::ValidationState};
use pvd_protocol::{Decoder, Event, EventGroup};

const FIRST: usize = 600;
const LAST: usize = 969;

struct DigestWriter(blake3::Hasher);

impl Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn scene_hash(scene: &SceneState) -> String {
    postcard::to_io(
        &(
            &scene.objects,
            &scene.cameras,
            &scene.errors,
            &scene.debug_draw,
            &scene.contacts,
            &scene.reference_lifetimes,
            &scene.generations,
        ),
        DigestWriter(blake3::Hasher::new()),
    )
    .unwrap()
    .0
    .finalize()
    .to_hex()
    .to_string()
}

fn raw_scene_oracle(bytes: &[u8]) -> BTreeMap<usize, (FrameInfo, String)> {
    let mut decoder = Decoder::default();
    let mut builder = None;
    let mut frames = 0;
    let mut expected = BTreeMap::new();
    for chunk in bytes.chunks(64 << 10) {
        let groups = decoder.feed(chunk).unwrap();
        if builder.is_none()
            && let Some(header) = decoder.header()
        {
            let mut value = RecordingBuilder::new(header.clone());
            value.validation = Some(ValidationState {
                skip_analysis: true,
                ..Default::default()
            });
            builder = Some(value);
        }
        let Some(builder) = &mut builder else {
            assert!(groups.is_empty());
            continue;
        };
        for group in groups {
            for event in group.events {
                if let Event::EndSection {
                    id,
                    name,
                    timestamp,
                } = &event
                    && builder
                        .strings
                        .get(name)
                        .is_some_and(|name| name == "frame")
                {
                    // EndSection clears transient debug drawing after the displayed frame.
                    if (FIRST..=LAST).contains(&frames) {
                        let info = FrameInfo {
                            index: frames,
                            timestamp: *timestamp,
                            scene_id: *id,
                        };
                        assert!(
                            expected
                                .insert(frames, (info, scene_hash(&builder.state)))
                                .is_none()
                        );
                    }
                    frames += 1;
                }
                builder
                    .push(EventGroup {
                        stream_id: group.stream_id,
                        timestamp: group.timestamp,
                        offset: group.offset,
                        events: vec![event],
                    })
                    .unwrap();
            }
        }
    }
    decoder.finish().unwrap();
    let builder = builder.unwrap();
    assert!(builder.partial.is_empty());
    assert!(builder.groups.is_empty());
    assert!(builder.streaming.is_none());
    assert!(builder.recording.actions.is_empty());
    assert_eq!(builder.validation.as_ref().unwrap().frames, frames);
    assert_eq!(frames, LAST + 1);
    assert_eq!(expected.len(), LAST - FIRST + 1);
    expected
}

#[test]
#[ignore = "requires the fixed SDK 970-frame input and a new PVD_SCENE_UNDO_OUTPUT directory"]
fn actual_970_cold_then_long_reverse_matches_raw_scene_frames() {
    let input = std::env::var_os("PVD_SHARED_HISTORY_RAW").expect("fixed real SDK input");
    let output = PathBuf::from(
        std::env::var_os("PVD_SCENE_UNDO_OUTPUT").expect("new scene undo output directory"),
    );
    let bytes = std::fs::read(&input).unwrap();
    assert_eq!(bytes.len(), 35_817_266);
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60"
    );
    std::fs::create_dir(&output).unwrap();
    let expected = raw_scene_oracle(&bytes);
    let history = output.join("history");
    let mut live = crate::LiveRecording::with_history(HistoryConfig {
        directory: history.clone(),
        disk_budget_bytes: 8 << 30,
        segment_target_bytes: 64 << 20,
        segment_frames: 40,
        min_free_disk_bytes: 512 << 20,
    })
    .unwrap();
    for chunk in bytes.chunks(64 << 10) {
        live.feed(chunk).unwrap();
    }
    let recording = live.finish().unwrap();
    assert_eq!(recording.frame_count(), LAST + 1);
    let usage = recording.disk_usage().unwrap();
    assert!(usage.warning.is_none(), "{usage:?}");
    assert_eq!(usage.first_frame, 0);
    assert_eq!(usage.retained_frames, LAST + 1);
    recording.verify_replay().unwrap();
    let disk = recording.disk.as_ref().unwrap();
    let resources = disk.segments.front().unwrap().resources.clone();
    let crossed_segments = disk
        .segments
        .iter()
        .map(Arc::as_ref)
        .chain(&disk.active)
        .filter(|segment| {
            let first = segment.frames.first().unwrap().info.index;
            FIRST < first && first <= LAST
        })
        .count();
    assert!(crossed_segments >= 8);
    let mut cursor = ReplayCursor::default();
    let mut displayed = SceneState::default();
    let mut rows = Vec::new();
    let mut paused = None;
    for frame in (FIRST..=LAST).rev() {
        let before = cursor.scene_route_counts();
        let started = std::time::Instant::now();
        let replay = cursor.seek_scene(&recording, frame, || false).unwrap();
        let seek_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(replay.frame, frame);
        replay.update.apply_owned(&mut displayed);
        let (info, expected_hash) = &expected[&frame];
        let actual_info = recording.frame_info(frame).unwrap();
        assert_eq!(actual_info.index, info.index);
        assert_eq!(actual_info.timestamp, info.timestamp);
        assert_eq!(actual_info.scene_id, info.scene_id);
        let hash = scene_hash(&displayed);
        assert_eq!(&hash, expected_hash, "full raw Scene at frame {frame}");
        if frame == LAST {
            paused = Some(displayed.clone());
        }
        let counts = cursor.scene_route_counts();
        assert_eq!(counts.index, 1, "index rebuild at frame {frame}");
        assert_eq!(counts.adjacent_forward, 0);
        let route = if counts.index == before.index + 1 {
            "index"
        } else if counts.memory_undo == before.memory_undo + 1 {
            "memory_undo"
        } else {
            assert_eq!(counts.persistent_undo, before.persistent_undo + 1);
            "persistent_undo"
        };
        rows.push(serde_json::json!({
            "frame": frame,
            "seek_ms": seek_ms,
            "route": route,
            "scene_hash": hash,
            "index_reads": counts.index,
            "memory_undo": counts.memory_undo,
            "persistent_undo": counts.persistent_undo,
        }));
    }
    let counts = cursor.scene_route_counts();
    assert_eq!(counts.memory_undo + counts.persistent_undo, LAST - FIRST);
    assert!(counts.persistent_undo > 240);
    assert_eq!(scene_hash(paused.as_ref().unwrap()), expected[&LAST].1);
    let summary = serde_json::json!({
        "scope": "Scene correctness and actual cursor routes; no complete Analysis comparison",
        "timing_scope": "Wall time of seek_scene only, excluding update application, hashing and oracle construction. verify_replay runs first, so file and resource caches may be warm. Development timings are diagnostic; performance claims require a separately controlled Release run.",
        "input_bytes": bytes.len(),
        "input_blake3": blake3::hash(&bytes).to_hex().to_string(),
        "frames": recording.frame_count(),
        "cold_frame": LAST,
        "reverse_steps": LAST - FIRST,
        "crossed_segments": crossed_segments,
        "disk_bytes": usage.disk_bytes,
        "resource_bytes": resources.physical_bytes(),
        "index_reads": counts.index,
        "memory_undo": counts.memory_undo,
        "persistent_undo": counts.persistent_undo,
        "oracle": "SDK events applied by bounded validation builder before EndSection; no disk or Undo reader",
        "scene_fields": ["objects", "cameras", "errors", "debug_draw", "contacts", "reference_lifetimes", "generations"],
        "rows": rows,
        "cleanup_payload_files": 0,
    });
    drop((cursor, paused, displayed, recording));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    drop(resources);
    assert_eq!(std::fs::read_dir(&history).unwrap().count(), 0);
    std::fs::remove_dir(&history).unwrap();
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
}
