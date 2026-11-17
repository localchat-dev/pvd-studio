use super::*;
use bevy::{ecs::system::RunSystemOnce, prelude::*};
use bevy_egui::{EguiContext, EguiUserTextures, PrimaryEguiContext};
use pvd_core::{Recording, SceneState};
use std::sync::Arc;

fn capture() -> Vec<u8> {
    let mut bytes = crate::full_profile::tests::timeline_capture(2, "UI actions");
    let mut names = Vec::new();
    for (id, text) in [(7u32, "Counter"), (8, "PvdU32")] {
        names.push(1);
        names.extend((text.len() as u32 + 1).to_le_bytes());
        names.extend(text.as_bytes());
        names.push(0);
        names.extend(id.to_le_bytes());
    }
    fn group(bytes: &mut Vec<u8>, events: &[u8], count: u32) {
        bytes.extend((events.len() as u32).to_le_bytes());
        bytes.extend(count.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(events);
    }
    group(&mut bytes, &names, 2);
    for counter in 0..6u32 {
        let mut events = vec![7];
        events.extend(9u64.to_le_bytes());
        for value in [7u32, 1, 8, 1, 4, counter] {
            events.extend(value.to_le_bytes());
        }
        events.push(19);
        events.extend(9u64.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend((1000 + u64::from(counter) * 100).to_le_bytes());
        group(&mut bytes, &events, 2);
    }
    bytes
}

fn studio() -> Studio {
    let bytes = Arc::new(capture());
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_count(), 8);
    let mut studio = Studio::default();
    studio.prefs.language = crate::i18n::Language::English;
    studio.initialized = true;
    studio.set_recording(recording);
    studio.bytes = Some(bytes);
    studio.seek(2);
    assert!(!studio.state.objects.is_empty());
    studio
}

fn app(context: &egui::Context) -> App {
    crate::ui::configure(context);
    let mut app = App::new();
    app.insert_resource(studio())
        .init_resource::<crate::navmesh::NavMeshOverlay>()
        .init_resource::<crate::scene::Viewport>()
        .init_resource::<Assets<Mesh>>()
        .init_resource::<Assets<StandardMaterial>>()
        .init_resource::<Assets<Image>>()
        .init_resource::<EguiUserTextures>()
        .add_systems(Update, crate::ui::prepare.before(crate::scene::sync_scene))
        .add_systems(Update, crate::scene::sync_scene);
    let mut entity = app.world_mut().spawn(PrimaryEguiContext);
    *entity.get_mut::<EguiContext>().unwrap().get_mut() = context.clone();
    app.update();
    app
}

fn draw(
    app: &mut App,
    context: &egui::Context,
    events: Vec<egui::Event>,
    retry: bool,
) -> egui::FullOutput {
    context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 960.0),
            )),
            focused: true,
            events,
            ..Default::default()
        },
        |context| {
            let result: bevy::prelude::Result =
                app.world_mut().run_system_once(crate::ui::draw).unwrap();
            result.unwrap();
            if retry && context.current_pass_index() == 0 {
                context.request_discard("exercise capture action layout retry");
            }
        },
    )
}

fn button(app: &mut App, context: &egui::Context, label: &str) -> egui::Pos2 {
    let _ = draw(app, context, vec![], false);
    let output = draw(app, context, vec![], false);
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text() == label => {
                Some(text.galley.rect.translate(text.pos.to_vec2()).center())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("button {label:?} missing"))
}

fn click(app: &mut App, context: &egui::Context, label: &str, retry: bool) -> egui::FullOutput {
    let pos = button(app, context, label);
    let _ = draw(app, context, vec![egui::Event::PointerMoved(pos)], false);
    let event = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let _ = draw(app, context, vec![event(true)], false);
    draw(app, context, vec![event(false)], retry)
}

fn accepted(app: &App) -> (usize, u64, SceneState) {
    let studio = app.world().resource::<Studio>();
    assert_eq!(
        app.world()
            .resource::<crate::scene::Viewport>()
            .display_probe_stats()
            .0,
        studio.revision
    );
    (studio.frame, studio.revision, studio.state.clone())
}

fn publish_live_head(studio: &mut Studio) {
    let recording = studio.recording.as_ref().unwrap().clone();
    let frame = recording.frame_count() - 1;
    let state = recording.state_at(frame).unwrap();
    let source_frame = recording
        .frame_sources
        .frame(frame)
        .map(|(source, frame)| (source, *frame));
    assert!(source_frame.is_some());
    studio.live_session = Some(1);
    studio.live_connected = true;
    studio.accept_live_snapshot(crate::network::LiveSnapshot {
        session: 1,
        frame,
        source_frame,
        recording: (*recording).clone(),
        update: pvd_core::live::SceneUpdate::full(&state),
        display_updated: true,
    });
    assert_eq!(studio.live_frame, frame);
    assert_eq!(studio.live_frame_source, source_frame);
    assert_eq!(studio.live_state, state);
}

#[test]
fn actual_next_click_and_layout_retry_wait_for_prepare_before_scene_sync() {
    let context = egui::Context::default();
    let mut app = app(&context);
    let before = accepted(&app);
    let recording = app
        .world()
        .resource::<Studio>()
        .recording
        .as_ref()
        .unwrap()
        .clone();
    let output = click(&mut app, &context, ">", true);
    assert_eq!(output.platform_output.num_completed_passes, 2);
    assert_eq!(accepted(&app), before);
    assert_eq!(app.world().resource::<Studio>().ui_actions.0.len(), 1);
    assert!(Arc::ptr_eq(
        &recording,
        app.world().resource::<Studio>().recording.as_ref().unwrap()
    ));
    app.update();
    let after = accepted(&app);
    assert_eq!(after.0, before.0 + 1);
    assert_eq!(after.2, recording.state_at(after.0).unwrap());
    assert_ne!(after.2, before.2);
    assert!(app.world().resource::<Studio>().ui_actions.0.is_empty());
    app.update();
    assert_eq!(accepted(&app), after);
}

#[test]
fn keyboard_steps_and_playback_toggles_preserve_the_current_ui_pass() {
    let context = egui::Context::default();
    let mut app = app(&context);
    for (key, delta) in [(egui::Key::ArrowRight, 1isize), (egui::Key::ArrowLeft, -1)] {
        let before = accepted(&app);
        let _ = draw(
            &mut app,
            &context,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        );
        assert_eq!(accepted(&app), before);
        app.update();
        assert_eq!(
            accepted(&app).0,
            before.0.checked_add_signed(delta).unwrap()
        );
    }
    for (label, direction) in [("▶ Play", 1), ("◀ Reverse", -1), ("◀ Reverse", 0)] {
        let before = accepted(&app);
        let old_direction = app.world().resource::<Studio>().playing;
        let _ = click(&mut app, &context, label, true);
        assert_eq!(accepted(&app), before);
        assert_eq!(app.world().resource::<Studio>().playing, old_direction);
        app.update();
        assert_eq!(app.world().resource::<Studio>().playing, direction);
        accepted(&app);
    }
}

#[test]
fn ordered_requests_coalesce_one_control_per_input_frame_and_bound_pending_paths() {
    let context = egui::Context::default();
    let mut studio = studio();
    studio.request_ui_action(&context, "slider", UiAction::PauseAndSeek(3));
    studio.request_ui_action(&context, "next", UiAction::Step(1));
    studio.request_ui_action(&context, "slider", UiAction::PauseAndSeek(5));
    assert_eq!(studio.ui_actions.0.len(), 2);
    assert_eq!(studio.frame, 2);
    studio.apply_ui_actions(&context);
    assert_eq!(studio.frame, 6);
    studio.seek(2);
    assert_eq!(studio.frame, 2, "direct/headless seek remains immediate");
    studio.request_ui_action(
        &context,
        "long",
        UiAction::Open(PathBuf::from("x".repeat(MAX_PATH_BYTES + 1))),
    );
    assert!(studio.ui_actions.0.is_empty());
    let mut oversized = PathBuf::from("short.pvdstream");
    oversized.reserve(MAX_PATH_BYTES + 1);
    studio.request_ui_action(&context, "oversized", UiAction::Open(oversized));
    assert!(studio.ui_actions.0.is_empty());
    for index in 0..MAX_ACTIONS + 2 {
        studio.request_ui_action(&context, index, UiAction::PauseAndSeek(index));
    }
    assert_eq!(studio.ui_actions.0.len(), MAX_ACTIONS);
    studio.ui_actions = UiActions::default();
    studio.request_ui_action(&context, "close", UiAction::Close);
    studio.request_ui_action(
        &context,
        "open",
        UiAction::Open(PathBuf::from("unread.pvdstream")),
    );
    studio.exit_requested = true;
    studio.apply_ui_actions(&context);
    assert!(studio.recording.is_some());
    assert!(studio.job.is_none());
    assert!(studio.ui_actions.0.is_empty());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !studio.poll_shutdown() || studio.background_jobs.active_count() != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "shutdown did not release the recording"
        );
        std::thread::yield_now();
    }
    studio.request_ui_action(
        &context,
        "reopen",
        UiAction::Open(PathBuf::from("unread.pvdstream")),
    );
    assert!(studio.ui_actions.0.is_empty());
    assert!(studio.recording.is_none());
}

#[test]
fn actual_close_and_clip_decisions_are_deferred_and_modal_blocks_other_actions() {
    let context = egui::Context::default();
    let mut app = app(&context);
    app.world_mut().resource_mut::<Studio>().clip_set.add(2, 5);
    let before = accepted(&app);
    let _ = click(&mut app, &context, "Close", true);
    assert_eq!(accepted(&app), before);
    assert!(
        app.world()
            .resource::<Studio>()
            .pending_clip_action
            .is_none()
    );
    app.update();
    assert!(matches!(
        app.world().resource::<Studio>().pending_clip_action,
        Some(PendingAction::Close)
    ));
    assert_eq!(accepted(&app), before);
    let _ = click(&mut app, &context, "Keep editing", true);
    assert!(
        app.world()
            .resource::<Studio>()
            .pending_clip_action
            .is_some()
    );
    app.world_mut().resource_mut::<Studio>().request_ui_action(
        &context,
        "blocked-step",
        UiAction::Step(1),
    );
    app.update();
    assert!(
        app.world()
            .resource::<Studio>()
            .pending_clip_action
            .is_none()
    );
    assert_eq!(accepted(&app), before);
    assert_eq!(app.world().resource::<Studio>().clip_set.clips.len(), 1);
    let _ = click(&mut app, &context, "Close", false);
    app.update();
    let _ = click(&mut app, &context, "Discard and continue", true);
    assert_eq!(accepted(&app), before);
    assert_eq!(app.world().resource::<Studio>().clip_set.clips.len(), 1);
    app.update();
    let studio = app.world().resource::<Studio>();
    assert!(studio.recording.is_none());
    assert!(studio.clip_set.clips.is_empty());
    assert!(studio.pending_clip_action.is_none());
    accepted(&app);
}

#[test]
fn return_live_and_clip_jump_keep_scene_until_the_next_prepare() {
    let context = egui::Context::default();
    let mut app = app(&context);
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        publish_live_head(&mut studio);
    }
    let before = accepted(&app);
    let label = app
        .world()
        .resource::<Studio>()
        .live_follow_label(crate::i18n::Language::English)
        .to_owned();
    let _ = click(&mut app, &context, &label, true);
    assert_eq!(accepted(&app), before);
    app.update();
    assert_eq!(accepted(&app).0, 7);
    assert!(app.world().resource::<Studio>().follow_live);
    let before = accepted(&app);
    app.world_mut().resource_mut::<Studio>().request_ui_action(
        &context,
        "clip-jump",
        UiAction::PauseAndSeek(3),
    );
    assert_eq!(accepted(&app), before);
    app.update();
    assert_eq!(accepted(&app).0, 3);
    assert!(!app.world().resource::<Studio>().follow_live);
}

#[test]
fn actual_next_click_at_live_head_keeps_live_and_pause_click_shows_its_reason() {
    let context = egui::Context::default();
    let mut app = app(&context);
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        publish_live_head(&mut studio);
        studio.return_to_live();
    }
    app.update();
    let before = accepted(&app);
    let _ = click(&mut app, &context, ">", true);
    app.update();
    assert_eq!(accepted(&app), before);
    assert!(app.world().resource::<Studio>().follow_live);
    let _ = click(&mut app, &context, "Ⅱ Pause", true);
    app.update();
    assert!(!app.world().resource::<Studio>().follow_live);
    let output = draw(&mut app, &context, vec![], false);
    for label in ["PAUSED", "Pause command"] {
        assert!(
            output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::epaint::Shape::Text(text) if text.galley.text() == label
            )),
            "missing visible state label: {label}"
        );
    }
}

#[test]
fn actual_open_and_dropped_file_start_only_at_prepare_and_shutdown_releases_jobs() {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let root = std::env::temp_dir().join(format!(
        "pvd-ui-actions-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("capture.pvdstream");
    std::fs::write(&path, capture()).unwrap();
    let context = egui::Context::default();
    let mut app = app(&context);
    app.world_mut().resource_mut::<Studio>().path_input = path.to_string_lossy().into_owned();
    let before = accepted(&app);
    let _ = click(&mut app, &context, "Open", true);
    assert_eq!(accepted(&app), before);
    assert!(app.world().resource::<Studio>().job.is_none());
    app.update();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.world().resource::<Studio>().job.is_some() {
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(1));
        app.update();
    }
    assert_eq!(
        app.world().resource::<Studio>().capture_path.as_ref(),
        Some(&path)
    );
    assert_eq!(accepted(&app).0, 0);
    let before = accepted(&app);
    let _ = context.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 960.0),
            )),
            dropped_files: vec![egui::DroppedFile {
                path: Some(path.clone()),
                ..Default::default()
            }],
            ..Default::default()
        },
        |context| {
            let result: bevy::prelude::Result =
                app.world_mut().run_system_once(crate::ui::draw).unwrap();
            result.unwrap();
            if context.current_pass_index() == 0 {
                context.request_discard("dropped capture layout retry");
            }
        },
    );
    assert_eq!(accepted(&app), before);
    assert!(app.world().resource::<Studio>().job.is_none());
    assert_eq!(app.world().resource::<Studio>().ui_actions.0.len(), 1);
    app.world_mut().resource_mut::<Studio>().exit_requested = true;
    while !app.world_mut().resource_mut::<Studio>().poll_shutdown() {
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(1));
    }
    assert!(app.world().resource::<Studio>().ui_actions.0.is_empty());
    assert!(app.world().resource::<Studio>().job.is_none());
    drop(app);
    let files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        files,
        vec![path.clone()],
        "loading creates no history or index payload"
    );
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(root).unwrap();
}
