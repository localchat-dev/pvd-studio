use super::*;
use crate::{captured_camera, coordinates::Coordinates, scene::Viewport};
use pvd_core::{SourceCamera, live::HistoryConfig};
use pvd_protocol::{Decoder, Endian, Event};
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
struct Camera {
    name: String,
    #[serde(flatten)]
    pose: SourceCamera,
}
#[derive(serde::Deserialize)]
struct Frame {
    frame: usize,
    cameras: Vec<Camera>,
}
#[derive(serde::Deserialize)]
struct Truth {
    frames: Vec<Frame>,
}

fn wait_loaded(studio: &mut Studio, context: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while studio.job.is_some() && Instant::now() < deadline {
        studio.poll(context);
        std::thread::park_timeout(Duration::from_millis(2));
    }
    assert!(studio.job.is_none(), "load timed out");
    assert!(studio.recording.is_some(), "{:?}", studio.notices);
    studio.focus_selected = false;
}

fn seek(studio: &mut Studio, context: &egui::Context, frame: usize) {
    studio.pause_display();
    studio.seek(frame);
    let deadline = Instant::now() + Duration::from_secs(10);
    while studio.seeking.is_some() && Instant::now() < deadline {
        studio.poll(context);
        std::thread::park_timeout(Duration::from_millis(2));
    }
    assert!(studio.seeking.is_none(), "seek timed out");
    assert_eq!(studio.frame, frame, "{:?}", studio.notices);
}

fn assert_pose(
    studio: &Studio,
    viewport: &mut Viewport,
    expected: &BTreeMap<String, SourceCamera>,
) {
    assert_eq!(&studio.state.cameras, expected);
    captured_camera::sync(studio, viewport);
    let expected = studio
        .captured_camera
        .as_ref()
        .and_then(|name| expected.get(name))
        .and_then(|camera| captured_camera::Pose::from_source(camera, viewport.coordinates));
    assert_eq!(
        viewport
            .captured_pose
            .map(|pose| (pose.position, pose.up, pose.target)),
        expected.map(|pose| (pose.position, pose.up, pose.target))
    );
}

#[test]
#[ignore = "requires tools/physx-cameras official SDK capture and independent truth JSON"]
fn official_sdk_camera_selection_tracks_displayed_frames_and_paused_live_history() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../captures/cameras");
    let path = directory.join("cameras.pvdstream");
    let bytes = std::fs::read(&path).expect("run the SDK camera producer");
    let truth: Truth =
        serde_json::from_slice(&std::fs::read(directory.join("truth.json")).unwrap()).unwrap();
    let expected: Vec<BTreeMap<String, SourceCamera>> = truth
        .frames
        .into_iter()
        .enumerate()
        .map(|(index, frame)| {
            assert_eq!(frame.frame, index);
            frame
                .cameras
                .into_iter()
                .map(|camera| (camera.name, camera.pose))
                .collect()
        })
        .collect();
    assert!(expected.len() > 8 && expected[0].is_empty());
    let name = expected[1].keys().next().expect("frame 1 camera").clone();
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    let mut viewport = Viewport::default();
    studio.open(path.clone(), context.clone());
    wait_loaded(&mut studio, &context);
    assert_eq!(studio.count(), expected.len());
    studio.captured_camera = Some(name.clone());
    for frame in [1, 0, 1, expected.len() - 1, 2, 0] {
        seek(&mut studio, &context, frame);
        assert_pose(&studio, &mut viewport, &expected[frame]);
    }
    studio.recorded_timing = false;
    studio.playback_fps = 1.0;
    studio.toggle_playback();
    studio.last_step = Instant::now() - Duration::from_millis(1100);
    studio.poll(&context);
    assert_eq!(studio.frame, 1);
    assert_pose(&studio, &mut viewport, &expected[1]);
    studio.pause_display();
    studio.toggle_reverse();
    studio.last_step = Instant::now() - Duration::from_millis(1100);
    studio.poll(&context);
    assert_eq!(studio.frame, 0);
    assert_pose(&studio, &mut viewport, &expected[0]);
    studio.pause_display();
    studio.close();
    captured_camera::sync(&studio, &mut viewport);
    assert!(studio.captured_camera.is_none() && viewport.captured_pose.is_none());
    studio.open(path, context.clone());
    wait_loaded(&mut studio, &context);
    assert!(studio.captured_camera.is_none());
    assert_pose(&studio, &mut viewport, &expected[0]);
    studio.close();

    let cache = std::env::temp_dir().join(format!(
        "pvd-camera-desktop-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache.clone(),
        segment_frames: 3,
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut decoder = Decoder::default();
    let groups = decoder.feed(&bytes).unwrap();
    decoder.finish().unwrap();
    assert_eq!(decoder.header().unwrap().endian, Endian::Little);
    let mut names = BTreeMap::new();
    let mut boundaries = Vec::new();
    for group in groups {
        for event in group.events {
            match event {
                Event::StringHandle { handle, value } => {
                    names.insert(handle, value);
                }
                Event::EndSection { name, .. }
                    if names.get(&name).is_some_and(|name| name == "frame") =>
                {
                    let offset = group.offset as usize;
                    let length =
                        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
                    boundaries.push(offset + 24 + length);
                }
                _ => (),
            }
        }
    }
    assert_eq!(boundaries.len(), expected.len());
    studio.connect_live_session(901);
    live.feed(&bytes[..boundaries[1]]).unwrap();
    let (frame, update) = live.take_update().unwrap();
    assert_eq!(frame, 1);
    studio.accept_live_snapshot(LiveSnapshot {
        source_frame: None,
        display_updated: true,
        session: 901,
        frame,
        update,
        recording: live.snapshot().unwrap(),
    });
    studio.captured_camera = Some(name.clone());
    assert_pose(&studio, &mut viewport, &expected[1]);
    studio.pause_display();
    live.feed(&bytes[boundaries[1]..]).unwrap();
    let (frame, update) = live.take_update().unwrap();
    assert_eq!(frame, expected.len() - 1);
    studio.accept_live_snapshot(LiveSnapshot {
        source_frame: None,
        display_updated: true,
        session: 901,
        frame,
        update,
        recording: live.snapshot().unwrap(),
    });
    assert_eq!(studio.live_state.cameras, expected[frame]);
    assert_eq!(studio.frame, 1);
    assert_pose(&studio, &mut viewport, &expected[1]);
    for frame in [0, 1, expected.len() - 1, 2, 0] {
        seek(&mut studio, &context, frame);
        assert_pose(&studio, &mut viewport, &expected[frame]);
    }
    studio.return_to_live();
    assert_pose(&studio, &mut viewport, expected.last().unwrap());
    viewport.coordinates = Coordinates::new(
        crate::coordinates::UpAxis::Z,
        crate::coordinates::Handedness::Left,
        0.001,
    );
    assert_pose(&studio, &mut viewport, expected.last().unwrap());
    studio.close();
    captured_camera::sync(&studio, &mut viewport);
    assert!(studio.captured_camera.is_none() && viewport.captured_pose.is_none());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !studio.poll_shutdown() {
        assert!(Instant::now() < deadline, "workers did not release history");
        std::thread::park_timeout(Duration::from_millis(2));
    }
    drop(studio);
    drop(live);
    std::fs::remove_dir(cache).unwrap();
}
