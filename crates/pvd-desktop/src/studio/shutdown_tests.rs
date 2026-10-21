use super::*;
use bevy::{
    app::AppExit,
    ecs::message::Messages,
    prelude::{App, Update, Window},
    window::{PrimaryWindow, WindowCloseRequested},
};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

fn close_app(studio: Studio) -> App {
    let mut app = App::new();
    app.add_message::<WindowCloseRequested>()
        .add_message::<AppExit>()
        .insert_resource(studio)
        .add_systems(Update, crate::clips::handle_window_close);
    let window = app
        .world_mut()
        .spawn((Window::default(), PrimaryWindow))
        .id();
    app.world_mut()
        .write_message(WindowCloseRequested { window });
    app
}

fn exited(app: &App) -> bool {
    !app.world().resource::<Messages<AppExit>>().is_empty()
}

#[test]
fn window_exit_waits_for_retired_background_resources_and_rejects_new_work() {
    struct Released(Arc<AtomicBool>);
    impl Drop for Released {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let mut studio = Studio::default();
    studio.persist_layout = false;
    let cancelled = Arc::new(AtomicBool::new(false));
    let released = Arc::new(AtomicBool::new(false));
    let owned = Released(released.clone());
    let (release, wait) = mpsc::channel();
    drop(
        studio
            .background_jobs
            .spawn("retired-view", cancelled.clone(), move || {
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
                let _ = &owned;
            })
            .unwrap(),
    );
    let mut app = close_app(studio);
    app.update();
    assert!(!exited(&app));
    assert!(cancelled.load(Ordering::Acquire));
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.open(
            PathBuf::from("shutdown-must-not-open.pvdstream"),
            egui::Context::default(),
        );
        studio.start_listener(egui::Context::default());
        assert!(studio.job.is_none());
        assert!(studio.server.is_none());
    }
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !exited(&app) {
        assert!(Instant::now() < deadline);
        app.update();
        std::thread::yield_now();
    }
    assert!(released.load(Ordering::Acquire));
    assert!(app.world().resource::<Studio>().exit_requested);
}

fn file_totals(path: &Path) -> (usize, u64) {
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, 0);
    };
    entries
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                file_totals(&entry.path())
            } else {
                (1, metadata.len())
            }
        })
        .fold((0, 0), |sum, value| (sum.0 + value.0, sum.1 + value.1))
}

#[test]
#[ignore = "Requires PVD_EXIT_PROBE_CAPTURE official SDK raw capture and PVD_EXIT_PROBE_OUTPUT; starts hidden child processes"]
fn real_tcp_process_exit_releases_history_and_receive_files() {
    let input = PathBuf::from(
        std::env::var_os("PVD_EXIT_PROBE_CAPTURE").expect("PVD_EXIT_PROBE_CAPTURE is required"),
    );
    assert!(std::fs::metadata(&input).unwrap().len() > 1 << 20);
    let output = PathBuf::from(
        std::env::var_os("PVD_EXIT_PROBE_OUTPUT").expect("PVD_EXIT_PROBE_OUTPUT is required"),
    );
    std::fs::create_dir_all(&output).unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut cases = Vec::new();
    for scenario in ["listener", "backlog", "seek", "retired", "load"] {
        let directory = output.join(scenario);
        std::fs::create_dir(&directory).unwrap();
        let mut command = Command::new(&executable);
        command
            .args([
                "studio::shutdown_tests::process_exit_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("PVD_EXIT_CHILD_SCENARIO", scenario)
            .env("PVD_EXIT_CHILD_OUTPUT", &directory)
            .env("PVD_EXIT_PROBE_CAPTURE", &input)
            .stdout(Stdio::from(
                File::create(directory.join("stdout.log")).unwrap(),
            ))
            .stderr(Stdio::from(
                File::create(directory.join("stderr.log")).unwrap(),
            ));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("exit probe {scenario} timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            status.success(),
            "exit probe {scenario} failed; inspect {}",
            directory.display()
        );
        let remaining = file_totals(&directory.join("cache"));
        assert_eq!(
            remaining,
            (0, 0),
            "{scenario} left cache files after process exit"
        );
        let child: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("child.json")).unwrap()).unwrap();
        cases.push(serde_json::json!({ "scenario": scenario, "remaining_files": remaining.0, "remaining_bytes": remaining.1, "child": child }));
    }
    std::fs::write(output.join("summary.json"), serde_json::to_vec_pretty(&serde_json::json!({ "passed": true, "cases": cases, "scope": "Natural hidden process exit through the production window-close system; SDK raw bytes are replayed over localhost TCP, without a GPU or native window." })).unwrap()).unwrap();
}

#[test]
#[ignore = "Internal child entry point for the process-exit probe"]
fn process_exit_child() {
    let scenario = std::env::var("PVD_EXIT_CHILD_SCENARIO").expect("child scenario is required");
    let output = PathBuf::from(std::env::var_os("PVD_EXIT_CHILD_OUTPUT").unwrap());
    let input = PathBuf::from(std::env::var_os("PVD_EXIT_PROBE_CAPTURE").unwrap());
    let cache = output.join("cache");
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.initialized = true;
    studio.prefs.history_directory = cache.to_string_lossy().into_owned();
    let started = Instant::now();
    let deadline = started + Duration::from_secs(60);
    if scenario == "load" {
        studio.open(input.clone(), context.clone());
    } else {
        let server = NetworkSession::start_tracked(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            },
            NetworkOptions {
                history: pvd_core::live::HistoryConfig {
                    directory: cache.clone(),
                    disk_budget_bytes: 64 << 20,
                    segment_frames: 40,
                    min_free_disk_bytes: 0,
                    ..Default::default()
                },
                receive_budget_bytes: 128 << 20,
                ..Default::default()
            },
            context.clone(),
            &studio.background_jobs,
        )
        .unwrap();
        let address = server.address();
        studio.server = Some(server);
        if scenario != "listener" {
            let input = input.clone();
            std::thread::spawn(move || {
                let mut socket = std::net::TcpStream::connect(address).unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut file = File::open(input).unwrap();
                let mut buffer = vec![0; 64 << 10];
                loop {
                    let length = file.read(&mut buffer).unwrap();
                    if length == 0 || socket.write_all(&buffer[..length]).is_err() {
                        break;
                    }
                }
            });
        }
    }
    loop {
        let _ = context.run(egui::RawInput::default(), |context| studio.poll(context));
        let ready = match scenario.as_str() {
            "listener" => true,
            "load" => file_totals(&cache).1 > 4096 && studio.job.is_some(),
            "seek" => studio.count() > studio.first_frame() + 30,
            "backlog" | "retired" => {
                file_totals(&cache).1 > 4096
                    && studio
                        .receive_status
                        .as_ref()
                        .is_some_and(|stats| stats.queued_bytes > 1 << 20)
            }
            _ => panic!("unknown child scenario"),
        };
        if ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scenario {scenario} did not become ready"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let decoded = studio.count();
    let queued = studio
        .receive_status
        .as_ref()
        .map_or(0, |stats| stats.queued_bytes);
    let before = file_totals(&cache);
    if scenario == "seek" {
        studio.pause_display();
        studio.seek(studio.first_frame());
        assert!(studio.seeking.is_some());
    } else if scenario == "retired" {
        studio.open(input, context.clone());
        assert!(studio.server.is_none());
        assert!(studio.job.is_some());
    }
    let active_jobs = studio.background_jobs.active_count();
    let closing = Instant::now();
    let mut app = close_app(studio);
    let mut pending_updates = 0;
    loop {
        app.update();
        if exited(&app) {
            break;
        }
        pending_updates += 1;
        assert!(
            Instant::now() < deadline,
            "scenario {scenario} shutdown did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let shutdown_seconds = closing.elapsed().as_secs_f64();
    drop(app);
    std::fs::write(output.join("child.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "decoded_before_close": decoded, "queued_bytes_before_close": queued,
        "cache_files_before_close": before.0, "cache_bytes_before_close": before.1,
        "active_jobs_before_close": active_jobs, "pending_shutdown_updates": pending_updates,
        "shutdown_seconds": shutdown_seconds, "elapsed_seconds": started.elapsed().as_secs_f64(),
        "exit_path": "production handle_window_close -> poll_shutdown -> AppExit -> drop App -> immediate test return"
    })).unwrap()).unwrap();
}
