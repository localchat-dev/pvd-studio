#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod activity;
#[cfg(target_os = "windows")]
mod app_icon;
mod atomic_output;
mod background_jobs;
mod camera_controller;
mod camera_projection;
mod captured_camera;
mod clip_store;
mod clips;
mod coordinates;
mod crash_report;
mod curve;
mod curve_panel;
mod display_probe;
mod egui_output;
mod file_picker;
mod full_profile;
mod grid_probe;
mod i18n;
mod infinite_grid;
mod lifecycle_probe;
mod line_render;
mod listen_settings;
mod memory;
mod navmesh;
mod navmesh_probe;
mod navmesh_render;
mod network;
mod point_clouds;
mod profile;
mod profile_cache;
mod property_copy;
mod recording_export;
mod room_scope;
mod scene;
mod studio;
mod tree;
mod ui;

use bevy::prelude::*;
use bevy::winit::WinitSettings;
use bevy::{app::ScheduleRunnerPlugin, window::ExitCondition, winit::WinitPlugin};
use bevy_egui::{EguiPlugin, EguiPrimaryContextPass};

fn main() -> AppExit {
    crash_report::install();
    pvd_core::live::enable_background_stale_history_cleanup();
    let mut args: Vec<String> = std::env::args().collect();
    let language = match take_language_option(&mut args) {
        Ok(language) => language,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let live_profile = take_flag_option(&mut args, "--live-profile");
    let smooth_lines = !take_flag_option(&mut args, "--no-line-aa");
    let navmesh_path = match take_path_option(&mut args, "--navmesh") {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return AppExit::error();
        }
    };
    let navmesh_second_path = match take_path_option(&mut args, "--navmesh-second") {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return AppExit::error();
        }
    };
    let lifecycle_check = if args
        .get(1)
        .is_some_and(|arg| arg == "--render-lifecycle-check")
    {
        match lifecycle_probe::Config::parse(&args) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("{error}");
                return AppExit::error();
            }
        }
    } else {
        None
    };
    let playback_check = if args.get(1).is_some_and(|arg| {
        matches!(
            arg.as_str(),
            "--render-playback-check" | "--render-ui-actions-check"
        )
    }) {
        match display_probe::Config::parse(&args) {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("{error}");
                return AppExit::error();
            }
        }
    } else {
        None
    };
    let grid_check = args.get(1).is_some_and(|arg| arg == "--render-check")
        && args.get(5).is_some_and(|arg| arg == "grid-motion");
    let navmesh_scene_check = args.get(1).is_some_and(|arg| arg == "--render-check")
        && args.get(5).is_some_and(|arg| arg == "navmesh-scenes");
    if navmesh_scene_check && (navmesh_path.is_none() || navmesh_second_path.is_none()) {
        eprintln!("navmesh-scenes requires --navmesh and --navmesh-second file paths");
        return AppExit::error();
    }
    if !navmesh_scene_check && navmesh_second_path.is_some() {
        eprintln!("--navmesh-second is only supported by the navmesh-scenes render check");
        return AppExit::error();
    }
    let render_check = lifecycle_check.is_some()
        || playback_check.is_some()
        || args.get(1).is_some_and(|arg| arg == "--render-check");
    let mut studio = studio::Studio::default();
    pvd_core::live::queue_stale_history_cleanup(std::path::Path::new(
        &studio.prefs.history_directory,
    ));
    studio.live_profile_enabled = live_profile;
    if let Some(language) = language {
        studio.prefs.language = language;
    }
    if !render_check {
        studio.path_input = args.get(1).cloned().unwrap_or_default();
    }
    let title = window_title(studio.prefs.language).to_owned();
    let mut app = App::new();
    app.insert_resource(WinitSettings::desktop_app())
        .insert_resource(ClearColor(Color::srgb(0.035, 0.045, 0.06)))
        .insert_resource(studio)
        .init_resource::<scene::Viewport>();
    let plugins = DefaultPlugins.set(WindowPlugin {
        close_when_requested: false,
        primary_window: (!render_check).then(|| {
            let mut window = Window {
                title,
                resolution: (1440, 960).into(),
                resize_constraints: WindowResizeConstraints {
                    min_width: 1024.0,
                    min_height: 720.0,
                    ..default()
                },
                ..default()
            };
            window.set_maximized(true);
            window
        }),
        exit_condition: if render_check {
            ExitCondition::DontExit
        } else {
            ExitCondition::OnAllClosed
        },
        ..default()
    });
    if render_check {
        let capture = args
            .get(2)
            .expect("--render-check requires a capture path and output PNG");
        let output = args.get(3).expect("--render-check requires an output PNG");
        app.world_mut().resource_mut::<studio::Studio>().path_input = if lifecycle_check.is_some() {
            String::new()
        } else {
            capture.clone()
        };
        app.insert_resource(scene::RenderCheck {
            output: output.clone(),
            target: Handle::default(),
            frames: 0,
            requested: false,
            desired_frame: (playback_check.is_none() && lifecycle_check.is_none())
                .then(|| args.get(4).and_then(|arg| arg.parse().ok()))
                .flatten(),
            panel: (playback_check.is_none() && lifecycle_check.is_none())
                .then(|| args.get(5).cloned())
                .flatten(),
            prepared: false,
            seek_requested: false,
            completion: None,
            exit_sent: false,
            size: if playback_check.is_some() || lifecycle_check.is_some() {
                UVec2::new(1440, 960)
            } else {
                UVec2::new(
                    args.get(6)
                        .and_then(|arg| arg.parse().ok())
                        .unwrap_or(1440)
                        .clamp(640, 4096),
                    args.get(7)
                        .and_then(|arg| arg.parse().ok())
                        .unwrap_or(960)
                        .clamp(480, 4096),
                )
            },
        });
        app.add_plugins(plugins.disable::<WinitPlugin>())
            .add_plugins(ScheduleRunnerPlugin::run_loop(
                std::time::Duration::from_millis(16),
            ));
        if playback_check.is_none()
            && lifecycle_check.is_none()
            && !grid_check
            && !navmesh_scene_check
        {
            app.add_systems(Update, scene::render_check);
        }
    } else {
        app.add_plugins(plugins);
        #[cfg(target_os = "windows")]
        app.add_systems(Update, app_icon::apply_window_icons);
    }
    app.add_plugins((
        EguiPlugin::default(),
        egui_output::Plugin,
        line_render::Plugin { smooth_lines },
        infinite_grid::Plugin { smooth_lines },
        navmesh_render::Plugin,
    ))
    .add_systems(Startup, scene::setup)
    .add_systems(EguiPrimaryContextPass, ui::draw)
    .add_systems(Update, ui::prepare.before(scene::sync_scene))
    .add_systems(
        Update,
        (scene::sync_scene, scene::update_camera, scene::draw_grid).chain(),
    )
    .add_systems(Update, sync_window_title)
    .add_systems(Update, clips::handle_window_close);
    if let Some(config) = playback_check {
        app.add_plugins(display_probe::Plugin(config));
    }
    if let Some(config) = lifecycle_check {
        app.add_plugins(lifecycle_probe::Plugin(config));
    }
    if grid_check {
        app.add_plugins(grid_probe::Plugin);
    }
    if navmesh_scene_check {
        app.add_plugins(navmesh_probe::Plugin {
            paths: [navmesh_path.unwrap(), navmesh_second_path.unwrap()],
        });
    } else if let Some(path) = navmesh_path {
        app.world_mut()
            .resource_mut::<navmesh::NavMeshOverlay>()
            .queue_import(path);
    }
    app.run()
}

fn take_path_option(
    args: &mut Vec<String>,
    flag: &str,
) -> Result<Option<std::path::PathBuf>, String> {
    let mut path = None;
    while let Some(index) = args.iter().position(|arg| arg == flag) {
        let value = args
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or_else(|| format!("{flag} requires a file path / {flag} 需要指定文件路径"))?;
        if path.is_some() {
            return Err(format!(
                "{flag} may only be specified once / {flag} 只能指定一次"
            ));
        }
        path = Some(std::path::PathBuf::from(value));
        args.drain(index..=index + 1);
    }
    Ok(path)
}

fn take_flag_option(args: &mut Vec<String>, flag: &str) -> bool {
    let mut enabled = false;
    args.retain(|arg| {
        if arg == flag {
            enabled = true;
            false
        } else {
            true
        }
    });
    enabled
}

fn take_language_option(args: &mut Vec<String>) -> Result<Option<i18n::Language>, String> {
    let mut language = None;
    while let Some(index) = args.iter().position(|arg| arg == "--language") {
        let parsed = args
            .get(index + 1)
            .and_then(|value| i18n::Language::parse(value))
            .ok_or_else(|| {
                "--language requires en or zh-CN / --language 需要指定 en 或 zh-CN".to_owned()
            })?;
        language = Some(parsed);
        args.drain(index..=index + 1);
    }
    Ok(language)
}

fn window_title(language: i18n::Language) -> &'static str {
    language.text(
        "pvd-studio • PhysX capture & inspection",
        "pvd-studio • PhysX 录制与调试",
    )
}

fn sync_window_title(studio: Res<studio::Studio>, mut windows: Query<&mut Window>) {
    let title = window_title(studio.prefs.language);
    for mut window in &mut windows {
        if window.title != title {
            window.title = title.to_owned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navmesh_option_preserves_render_arguments_and_rejects_missing_or_duplicate_paths() {
        let mut args = [
            "pvd-desktop",
            "--render-check",
            "capture.pvdstream",
            "out.png",
            "0",
            "navmesh",
            "--navmesh",
            "地图.bin",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(
            take_path_option(&mut args, "--navmesh").unwrap(),
            Some("地图.bin".into())
        );
        assert_eq!(
            args,
            [
                "pvd-desktop",
                "--render-check",
                "capture.pvdstream",
                "out.png",
                "0",
                "navmesh"
            ]
        );
        for values in [
            vec!["pvd-desktop", "--navmesh"],
            vec!["pvd-desktop", "--navmesh", "--render-check"],
            vec!["pvd-desktop", "--navmesh", "a.bin", "--navmesh", "b.bin"],
        ] {
            assert!(
                take_path_option(
                    &mut values.into_iter().map(str::to_owned).collect(),
                    "--navmesh"
                )
                .is_err()
            );
        }
    }

    #[test]
    fn diagnostic_flags_preserve_capture_and_render_arguments() {
        for flag in ["--live-profile", "--no-line-aa"] {
            for original in [
                vec!["pvd-desktop"],
                vec!["pvd-desktop", "中文 capture.pvdstream"],
                vec![
                    "pvd-desktop",
                    "--render-check",
                    "capture.pvdstream",
                    "out.png",
                    "60",
                    "memory",
                ],
            ] {
                let original: Vec<String> = original.into_iter().map(str::to_owned).collect();
                let mut absent = original.clone();
                assert!(!take_flag_option(&mut absent, flag));
                assert_eq!(absent, original);
                for location in 1..=original.len() {
                    let mut args = original.clone();
                    args.insert(location, flag.into());
                    args.extend(["--language".into(), "en".into(), flag.into()]);
                    assert_eq!(
                        take_language_option(&mut args).unwrap(),
                        Some(i18n::Language::English)
                    );
                    assert!(take_flag_option(&mut args, flag));
                    assert_eq!(args, original);
                }
            }
        }
    }

    #[test]
    fn language_option_preserves_capture_and_render_arguments() {
        for location in [1, 6] {
            let mut args: Vec<String> = [
                "pvd-desktop",
                "--render-check",
                "中文 capture.pvdstream",
                "out.png",
                "60",
                "memory",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect();
            let original = args.clone();
            args.splice(location..location, ["--language".into(), "en".into()]);
            assert_eq!(
                take_language_option(&mut args).unwrap(),
                Some(i18n::Language::English)
            );
            assert_eq!(args, original);
        }
        for value in [None, Some("de"), Some("--render-check")] {
            let mut args = vec!["pvd-desktop".into(), "--language".into()];
            if let Some(value) = value {
                args.push(value.into());
            }
            assert!(take_language_option(&mut args).is_err());
        }
    }
}
