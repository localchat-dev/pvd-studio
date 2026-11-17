use super::*;
use crate::network::NetworkOptions;
use pvd_transport::ListenConfig;
use std::{
    fs::{self, File},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

const TIMEOUT: Duration = Duration::from_secs(30);

struct Case {
    studio: Studio,
    context: egui::Context,
    directory: PathBuf,
    child: Child,
    input: ChildStdin,
    progress: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl Case {
    fn start() -> Self {
        let producer =
            PathBuf::from(std::env::var_os("PVD_ROOMS_PRODUCER").expect("set PVD_ROOMS_PRODUCER"))
                .canonicalize()
                .unwrap();
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/room-validation")
            .join(format!("desktop-tcp-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let directory = directory.canonicalize().unwrap();
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.prefs.language = Language::English;
        studio.recorded_timing = false;
        studio.server = Some(
            NetworkSession::start_tracked(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(directory.join("received")),
                    ..Default::default()
                },
                NetworkOptions {
                    history: pvd_core::live::HistoryConfig {
                        directory: directory.join("history"),
                        disk_budget_bytes: 8 << 20,
                        segment_frames: 2,
                        min_free_disk_bytes: 0,
                        ..Default::default()
                    },
                    receive_budget_bytes: 128 << 20,
                    raw_limit_bytes: 8 << 20,
                    ..Default::default()
                },
                context.clone(),
                &studio.background_jobs,
            )
            .unwrap(),
        );
        let port = studio.server.as_ref().unwrap().address().port();
        let mut command = Command::new(producer);
        command
            .arg("--lifecycle")
            .arg(directory.join("lifecycle.pvdstream"))
            .arg(directory.join("lifecycle.json"))
            .args(["--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .arg("--step-control")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(directory.join("sdk.stderr.log")).unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let mut log = File::create(directory.join("sdk.stdout.log")).unwrap();
        let (sender, progress) = mpsc::sync_channel(16);
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let line = line.unwrap();
                writeln!(log, "{line}").unwrap();
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            studio,
            context,
            directory,
            child,
            input,
            progress,
            reader: Some(reader),
        }
    }

    fn wait(&mut self, description: &str, ready: impl Fn(&Studio) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.studio.poll(&self.context);
            if ready(&self.studio) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{description}; artifacts={} notices={:?}",
                self.directory.display(),
                self.studio.notices
            );
            thread::park_timeout(Duration::from_millis(2));
        }
    }

    fn checkpoint(&mut self, command: Option<&str>, expected: &str) -> serde_json::Value {
        if let Some(command) = command {
            writeln!(self.input, "{command}").unwrap();
            self.input.flush().unwrap();
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.studio.poll(&self.context);
            if let Ok(line) = self.progress.try_recv() {
                let fields: Vec<_> = line.split_ascii_whitespace().collect();
                assert_eq!(&fields[..2], &["PVD_ROOMS", expected]);
                let truth: serde_json::Value = serde_json::from_slice(
                    &fs::read(self.directory.join("lifecycle.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    truth["checkpoints"].as_array().unwrap().last().unwrap()["name"],
                    expected
                );
                return truth;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "SDK stopped; {}",
                self.directory.display()
            );
            assert!(Instant::now() < deadline, "checkpoint {expected} timed out");
            thread::park_timeout(Duration::from_millis(2));
        }
    }

    fn scene(truth: &serde_json::Value, number: usize) -> FrameObject {
        let entry = &truth["rooms"][number - 1];
        assert_eq!(entry["number"].as_u64(), Some(number as u64));
        let id = entry["id"].as_u64().unwrap();
        let generation = truth["rooms"].as_array().unwrap()[..number - 1]
            .iter()
            .filter(|previous| previous["id"].as_u64() == Some(id))
            .count() as u32;
        room(id, generation)
    }

    fn assert_pose(&self, truth: &serde_json::Value, number: usize) {
        let actor = truth["rooms"][number - 1]["actor"].as_u64().unwrap();
        let expected = &truth["frames"][self.studio.frame];
        assert_eq!(expected["room"].as_u64(), Some(number as u64));
        let pvd_core::Value::Transform { position, rotation } =
            self.studio.state.objects[&actor].properties["GlobalPose"]
        else {
            panic!("missing actor pose");
        };
        for (actual, expected) in position
            .into_iter()
            .chain(rotation)
            .zip(expected["pose"].as_array().unwrap())
        {
            assert!((actual as f64 - expected.as_f64().unwrap()).abs() < 0.00001);
        }
        assert!(self.studio.room_scope.contains(actor));
        for other in truth["rooms"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|other| other["number"].as_u64() != Some(number as u64))
        {
            let other_actor = other["actor"].as_u64().unwrap();
            if other_actor != 0 && other_actor != actor {
                assert!(!self.studio.room_scope.contains(other_actor));
            }
        }
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let deadline = Instant::now() + TIMEOUT;
        while !self.studio.poll_shutdown() && Instant::now() < deadline {
            thread::park_timeout(Duration::from_millis(2));
        }
    }
}

#[test]
#[ignore = "requires built tools/physx-multiscene lifecycle producer; starts a real controlled SDK TCP connection"]
fn actual_sdk_tcp_room_lifecycle_without_following_frames_and_room_replay() {
    let mut case = Case::start();
    let truth = case.checkpoint(None, "ready");
    let a = Case::scene(&truth, 1);
    let b = Case::scene(&truth, 2);
    let zero = Case::scene(&truth, 3);
    case.wait("zero-frame directory", |studio| {
        studio
            .room_directory()
            .is_some_and(|rooms| rooms.len() == 3)
    });
    assert_eq!(case.studio.count(), 0);
    assert!(!case.studio.room_has_history(zero));
    case.studio.select_room(Some(zero));
    assert!(case.studio.room_selection_pending());
    case.checkpoint(Some("destroy-zero"), "zero-destroyed");
    case.wait("zero-frame destruction", |studio| {
        studio
            .room_directory()
            .unwrap()
            .get(zero)
            .unwrap()
            .destroyed_at_frame
            == Some(0)
    });
    assert_eq!(case.studio.count(), 0);
    assert!(!case.studio.room_selection_pending());
    case.studio.return_to_live();
    case.studio.select_room(Some(a));
    let truth = case.checkpoint(Some("step"), "frames");
    case.wait("room A complete display", |studio| {
        studio.count() == 4 && studio.displayed_room() == Some(a) && studio.frame == 2
    });
    case.assert_pose(&truth, 1);
    let before = case.studio.state.clone();
    case.checkpoint(Some("destroy-b"), "b-destroyed");
    case.wait("room B destruction without next frame", |studio| {
        studio
            .room_directory()
            .unwrap()
            .get(b)
            .unwrap()
            .destroyed_at_frame
            == Some(4)
    });
    assert_eq!(case.studio.state, before);
    assert!(case.studio.follow_live);
    case.studio.select_room(Some(b));
    case.wait("destroyed room B last frame", |studio| {
        studio.seeking.is_none() && studio.displayed_room() == Some(b)
    });
    assert_eq!(case.studio.frame, 3);
    case.assert_pose(&truth, 2);
    case.studio.step_frame(-1);
    case.wait("B reverse step", |studio| studio.seeking.is_none());
    assert_eq!(case.studio.frame, 1);
    case.assert_pose(&truth, 2);
    assert!(
        case.studio
            .room_directory()
            .unwrap()
            .get(b)
            .unwrap()
            .destroyed_at_frame
            .is_some()
    );
    case.studio.select_room(Some(a));
    case.wait("A paused selection", |studio| studio.seeking.is_none());
    case.studio.return_to_live();
    let truth = case.checkpoint(Some("replace"), "replacement");
    let replacement = Case::scene(&truth, 4);
    case.wait("new room and continued A display", |studio| {
        studio
            .room_directory()
            .is_some_and(|rooms| rooms.len() == 4)
            && studio.count() == 8
            && studio.frame == 7
            && studio.displayed_room() == Some(a)
    });
    case.assert_pose(&truth, 1);
    let before = case.studio.state.clone();
    case.checkpoint(Some("destroy-a"), "tail-destroyed");
    case.wait("A final-tail destruction", |studio| {
        studio
            .room_directory()
            .unwrap()
            .get(a)
            .unwrap()
            .destroyed_at_frame
            == Some(8)
    });
    assert!(!case.studio.follow_live);
    assert!(case.studio.live_connected);
    assert_eq!(case.studio.state, before);
    assert_eq!(case.studio.frame, 7);
    case.studio.select_room(Some(replacement));
    case.wait("replacement room history", |studio| {
        studio.seeking.is_none() && studio.displayed_room() == Some(replacement)
    });
    assert_eq!(case.studio.frame, 6);
    case.assert_pose(&truth, 4);
    case.studio.return_to_live();
    case.wait("replacement returns live without another End", |studio| {
        studio.follow_live
            && studio.displayed_room() == Some(replacement)
            && !studio.room_selection_pending()
    });
    let truth = case.checkpoint(Some("disconnect"), "disconnected");
    case.wait("disconnection", |studio| !studio.live_connected);
    assert_eq!(case.studio.count(), 8);
    assert!(
        case.studio
            .room_directory()
            .unwrap()
            .get(replacement)
            .unwrap()
            .destroyed_at_frame
            .is_none()
    );
    assert_eq!(case.studio.displayed_room(), Some(replacement));
    assert_eq!(case.studio.frame, 6);
    writeln!(case.input, "finish").unwrap();
    case.input.flush().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = case.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::park_timeout(Duration::from_millis(2));
    }
    case.reader.take().unwrap().join().unwrap();
    assert!(
        fs::read(case.directory.join("sdk.stderr.log"))
            .unwrap()
            .is_empty()
    );
    let bytes = fs::read(case.directory.join("lifecycle.pvdstream")).unwrap();
    assert_eq!(
        bytes,
        fs::read(case.studio.capture_path.as_ref().unwrap()).unwrap()
    );
    let reopened = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(reopened.frame_count(), 8);
    assert_eq!(
        reopened.rooms.iter().collect::<Vec<_>>(),
        case.studio
            .room_directory()
            .unwrap()
            .iter()
            .collect::<Vec<_>>()
    );
    case.assert_pose(&truth, 4);
    println!(
        "room lifecycle TCP validation: {}",
        case.directory.display()
    );
}
