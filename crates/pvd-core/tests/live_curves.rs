use pvd_core::{LiveRecording, Recording, frame_metrics, live::HistoryConfig, profile_curves};
use std::{fs, path::Path, sync::atomic::AtomicBool};

#[test]
#[ignore = "requires generated official PhysX 3.4 eALL stream"]
fn disk_curves_use_retained_global_frames_and_match_sdk_recording() {
    let bytes = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4-all.pvdstream"),
    )
    .unwrap();
    let expected = Recording::from_bytes(&bytes).unwrap();
    let mut live = LiveRecording::with_history(HistoryConfig {
        disk_budget_bytes: 2 << 20,
        segment_frames: 9,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    for bytes in bytes.chunks(4096) {
        live.feed(bytes).unwrap();
    }
    let recording = live.finish().unwrap();
    let first = recording.first_frame();
    assert!(first > 0, "fixture must force eviction");
    assert_eq!(recording.frame_count(), expected.frame_count());
    recording.verify_replay().unwrap();
    let scene_id = recording.frames[0].scene_id;
    let generation = expected.state_at(first).unwrap().objects[&scene_id].generation;
    let scene = profile_curves::ReferenceScene {
        id: scene_id,
        generation,
    };
    let cancelled = AtomicBool::new(false);
    let sampled = profile_curves::sampling_frames(&recording, scene, &cancelled).unwrap();
    assert_eq!(sampled.frames[0], first);
    let frames = &sampled.frames[1..];
    let actual = frame_metrics::sample(
        &recording,
        scene_id,
        generation,
        frame_metrics::Metric::FrameInterval,
        frames,
        &cancelled,
    )
    .unwrap();
    let reference = frame_metrics::sample(
        &expected,
        scene_id,
        generation,
        frame_metrics::Metric::FrameInterval,
        frames,
        &cancelled,
    )
    .unwrap();
    for (actual, reference) in actual.iter().zip(&reference) {
        assert_eq!(actual.frame, reference.frame);
        assert_eq!(actual.status, reference.status);
        assert_eq!(actual.milliseconds, reference.milliseconds);
    }
    for entry in profile_curves::catalog(&recording, &cancelled)
        .unwrap()
        .into_iter()
        .take(5)
    {
        let actual =
            profile_curves::sample(&recording, entry.key, scene, frames, &cancelled).unwrap();
        let reference =
            profile_curves::sample(&expected, entry.key, scene, frames, &cancelled).unwrap();
        assert_eq!(actual, reference);
    }
}
