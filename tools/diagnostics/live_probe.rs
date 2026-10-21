#![allow(dead_code)]
#[path = "../i18n.rs"]
mod i18n;
#[path = "../network.rs"]
mod network;
use network::{LiveEvent, NetworkOptions, NetworkSession};
use std::{
    collections::BTreeMap,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
fn main() {
    let root = std::env::var_os("PVD_PROBE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let output = std::env::var_os("PVD_PROBE_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("output/live-probe"));
    std::fs::create_dir_all(&output).unwrap();
    let sample = std::env::var_os("PVD_SDK34_SAMPLE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join(".deps/physx-sample-build/Release/pvd-physx-sample.exe"));
    let context = egui::Context::default();
    let wake = Arc::new(AtomicUsize::new(0));
    let w = wake.clone();
    context.set_request_repaint_callback(move |request| {
        if request.delay.is_zero() {
            w.fetch_add(1, Ordering::Relaxed);
        }
    });
    let session = NetworkSession::start_with_options(
        pvd_transport::ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        },
        NetworkOptions {
            live_profile: std::env::var_os("PVD_PROBE_PROFILE").is_some(),
            history: pvd_core::live::HistoryConfig {
                directory: output.join("history"),
                disk_budget_bytes: if std::env::var_os("PVD_PROBE_NO_HISTORY").is_some() {
                    4096
                } else {
                    pvd_core::live::HistoryConfig::default().disk_budget_bytes
                },
                ..Default::default()
            },
            ..Default::default()
        },
        context.clone(),
    )
    .unwrap();
    let frames = std::env::var("PVD_PROBE_FRAMES").unwrap_or("180".into());
    let extra = std::env::var("PVD_PROBE_BOXES").unwrap_or("0".into());
    let interval = std::env::var("PVD_PROBE_INTERVAL").unwrap_or("16".into());
    let mode = std::env::var("PVD_PROBE_MODE").unwrap_or("network".into());
    let mut cmd = Command::new(sample);
    if mode == "file" {
        cmd.args([
            "--file",
            output.join("producer-baseline.pvdstream").to_str().unwrap(),
        ]);
    } else {
        cmd.args([
            "--host",
            "127.0.0.1",
            "--port",
            &session.address().port().to_string(),
        ]);
    }
    cmd.args([
        "--frames",
        &frames,
        "--flags",
        "all",
        "--rich",
        "--interval-ms",
        &interval,
        "--extra-boxes",
        &extra,
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    if std::env::var_os("PVD_PROBE_CONTINUOUS").is_some() {
        cmd.arg("--continuous-motion").args([
            "--stats-truth",
            output.join("producer-stats.csv").to_str().unwrap(),
        ]);
    }
    if let Ok(duration) = std::env::var("PVD_PROBE_DURATION_SECONDS") {
        cmd.args(["--duration-seconds", &duration]);
    }
    for (variable, argument) in [
        ("PVD_PROBE_STATIC_BOXES", "--extra-static-boxes"),
        ("PVD_PROBE_RESIDENT_ALLOCATIONS", "--resident-allocations"),
        ("PVD_PROBE_ANALYSIS_BATCHES", "--analysis-batches-per-frame"),
    ] {
        if let Ok(value) = std::env::var(variable) {
            cmd.args([argument, &value]);
        }
    }
    if std::env::var_os("PVD_PROBE_RESIDENT_ALLOCATIONS").is_some()
        || std::env::var_os("PVD_PROBE_ANALYSIS_BATCHES").is_some()
    {
        cmd.arg("--analysis-truth")
            .arg(output.join("producer-analysis.csv"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let started = Instant::now();
    let mut child = cmd.spawn().unwrap();
    let mut current = None;
    let mut state = pvd_core::SceneState::default();
    let mut displayed_recording = None;
    let mut max_analysis_bytes = 0;
    let mut previous = BTreeMap::new();
    let mut updates = 0;
    let mut moves = 0;
    let mut lastframe = 0;
    let mut lastlog = Instant::now();
    let mut finished = None;
    let mut completed = false;
    let mut decode_ok = false;
    let mut max_queued_bytes = 0u64;
    let timeout = std::env::var("PVD_PROBE_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let deadline = Instant::now() + Duration::from_secs(timeout);
    while Instant::now() < deadline {
        let _ = context.run(egui::RawInput::default(), |_| {
            for event in session
                .receiver
                .lock()
                .unwrap()
                .try_iter()
                .collect::<Vec<_>>()
            {
                match event {
                    LiveEvent::Connected(id) => current = Some(id),
                    LiveEvent::Complete {
                        session: id,
                        result,
                        received_bytes,
                        ..
                    } => {
                        if let Some(s) = session.take_snapshot_for(id) {
                            s.update.apply(&mut state);
                            lastframe = s.frame;
                            updates += 1;
                            max_analysis_bytes =
                                max_analysis_bytes.max(s.recording.analysis.accounted_bytes());
                            displayed_recording = Some(s.recording);
                        }
                        println!(
                            "complete received_bytes={received_bytes} result {:?}",
                            result.as_ref().as_ref().map(|r| r.frame_count())
                        );
                        decode_ok = result.is_ok();
                        completed = true;
                    }
                    LiveEvent::Error(m) => println!("error {}", m.text(i18n::Language::English)),
                    _ => {}
                }
            }
            if let Some(id) = current
                && let Some(s) = session.take_snapshot_for(id)
            {
                s.update.apply(&mut state);
                lastframe = s.frame;
                updates += 1;
                max_analysis_bytes = max_analysis_bytes.max(s.recording.analysis.accounted_bytes());
                displayed_recording = Some(s.recording);
                for (id, o) in &state.objects {
                    if let Some(p) = o.properties.get("GlobalPose") {
                        if previous.get(id).is_some_and(|v| v != p) {
                            moves += 1;
                        }
                        previous.insert(*id, p.clone());
                    }
                }
            }
        });
        if lastlog.elapsed() > Duration::from_secs(1) {
            println!(
                "t={:.3}s updates={updates} frame={lastframe} objects={} moves={moves}",
                started.elapsed().as_secs_f64(),
                state.objects.len()
            );
            println!("receive_status={:?}", session.receive_status());
            if let Some(profile) = session.profile_snapshot() {
                println!("profile={}", serde_json::to_string(&profile).unwrap());
            }
            lastlog = Instant::now();
        }
        if let Some(status) = session.receive_status() {
            max_queued_bytes = max_queued_bytes.max(status.queued_bytes);
        }
        if finished.is_none()
            && let Some(exit) = child.try_wait().unwrap()
        {
            finished = Some((started.elapsed(), exit));
            println!(
                "producer finished {:.3}s status={exit}",
                started.elapsed().as_secs_f64()
            );
        }
        if finished.is_some_and(|(_, s)| !s.success())
            || finished.is_some() && (completed || mode == "file")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    if finished.is_none() {
        println!("producer still running after {timeout}s: stopping listener");
        session.request_stop();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            if let Some(exit) = child.try_wait().unwrap() {
                finished = Some((started.elapsed(), exit));
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if finished.is_none() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    let _ = child.wait();
    println!(
        "summary mode={mode} frames={frames} boxes={extra} interval_ms={interval} elapsed={:.3}s finished={finished:?} updates={updates} lastframe={lastframe} moves={moves} wakeups={}",
        started.elapsed().as_secs_f64(),
        wake.load(Ordering::Relaxed)
    );
    let summary = serde_json::json!({
        "mode": mode, "frames": frames, "boxes": extra, "interval_ms": interval,
        "elapsed_seconds": started.elapsed().as_secs_f64(),
        "producer_seconds": finished.map(|(duration, _)| duration.as_secs_f64()),
        "producer_success": finished.is_some_and(|(_, status)| status.success()),
        "updates": updates, "last_frame": lastframe, "moves": moves,
        "max_queued_bytes": max_queued_bytes, "completed": completed, "decode_ok": decode_ok,
        "received_bytes": session.receive_status().map(|status| status.received_bytes),
        "queued_bytes_at_completion": session.receive_status().map(|status| status.queued_bytes),
        "holds_display_recording": displayed_recording.is_some(),
        "extra_static_boxes": std::env::var("PVD_PROBE_STATIC_BOXES").ok(),
        "display_objects": state.objects.len(),
        "display_static_actors": state.objects.values().filter(|object| object.type_name.ends_with("::PxRigidStatic")).count(),
        "display_shapes": state.objects.values().filter(|object| object.type_name.ends_with("::PxShape")).count(),
        "sampled_max_analysis_bytes": max_analysis_bytes,
        "display_analysis_diagnostics": displayed_recording.as_ref().map(|recording| &recording.diagnostics),
        "profile": session.profile_snapshot(),
    });
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
    let succeeded = finished.is_some_and(|(_, status)| status.success())
        && (mode == "file" || completed && decode_ok);
    drop(displayed_recording);
    drop(session);
    if !succeeded {
        std::process::exit(1);
    }
}
