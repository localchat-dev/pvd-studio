use super::*;
use bevy_egui::{EguiContext, EguiFullOutput, EguiInput, EguiInputSet, PrimaryEguiContext};

const CASES: [&str; 5] = ["next", "play", "reverse", "return_live", "close"];

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Step {
    #[default]
    Fixture,
    Positioning,
    Locate,
    Move,
    Press,
    Release,
    Accepted,
    Completion,
}

#[derive(Resource, Default)]
struct Actions {
    step: Step,
    case: usize,
    latest_frame: usize,
    point: Option<egui::Pos2>,
    label: String,
    before: Stamp,
    before_state: Option<pvd_core::SceneState>,
    move_tick: u64,
    press_tick: u64,
    release_tick: u64,
    release: Option<Stamp>,
    release_passes: u64,
    next_prepare: Option<Stamp>,
    final_stamp: Option<Stamp>,
    completion: Option<u64>,
}

pub(super) fn install(app: &mut App) {
    app.init_resource::<Actions>()
        .add_systems(PreUpdate, inject.after(EguiInputSet::WriteEguiEvents))
        .add_systems(
            Update,
            prepared
                .after(crate::ui::prepare)
                .before(crate::scene::sync_scene),
        )
        .add_systems(EguiPrimaryContextPass, released.after(super::after_ui))
        .add_systems(
            PostUpdate,
            locate
                .after(EguiPostUpdateSet::EndPass)
                .before(EguiPostUpdateSet::ProcessOutput),
        )
        .add_systems(Last, drive_actions.after(super::drive));
}

fn inject(
    probe: Res<Probe>,
    mut actions: ResMut<Actions>,
    mut input: Query<&mut EguiInput, With<PrimaryEguiContext>>,
) {
    if probe.phase != Phase::Actions
        || !matches!(actions.step, Step::Move | Step::Press | Step::Release)
    {
        return;
    }
    let Ok(mut input) = input.single_mut() else {
        probe
            .trace
            .fail("action probe requires exactly one primary input");
        return;
    };
    let Some(point) = actions.point else {
        probe
            .trace
            .fail("action probe lost its actual button position");
        return;
    };
    input.0.focused = true;
    let tick = probe.tick + 1;
    let event = match actions.step {
        Step::Move => {
            actions.move_tick = tick;
            egui::Event::PointerMoved(point)
        }
        Step::Press | Step::Release => {
            let pressed = actions.step == Step::Press;
            if pressed {
                actions.press_tick = tick;
            } else {
                actions.release_tick = tick;
            }
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }
        }
        _ => unreachable!(),
    };
    input.0.events.push(event);
}

fn prepared(probe: Res<Probe>, studio: Res<Studio>, mut actions: ResMut<Actions>) {
    if probe.phase == Phase::Actions
        && actions.step == Step::Accepted
        && actions.next_prepare.is_none()
    {
        actions.next_prepare = Some(stamp(&studio, probe.tick));
    }
}

fn same_model(a: Stamp, b: Stamp) -> bool {
    a.frame == b.frame
        && a.revision == b.revision
        && a.request == b.request
        && a.analysis_ready == b.analysis_ready
}

fn released(
    probe: Res<Probe>,
    studio: Res<Studio>,
    mut actions: ResMut<Actions>,
    mut contexts: Query<&mut EguiContext, With<PrimaryEguiContext>>,
) {
    if probe.phase != Phase::Actions
        || actions.step != Step::Release
        || actions.release_tick != probe.tick
    {
        return;
    }
    let current = stamp(&studio, probe.tick);
    if !same_model(actions.before, current) || actions.before_state.as_ref() != Some(&studio.state)
    {
        probe
            .trace
            .fail("button release changed the displayed model after Scene synchronization");
        return;
    }
    actions.release = Some(current);
    actions.release_passes += 1;
    let Ok(mut context) = contexts.single_mut() else {
        probe
            .trace
            .fail("action probe requires one primary UI context");
        return;
    };
    if context.get_mut().current_pass_index() == 0 {
        context
            .get_mut()
            .request_discard("verify capture button across a layout retry");
    }
}

fn text_position(output: &egui::FullOutput, label: &str) -> Option<egui::Pos2> {
    output.shapes.iter().find_map(|shape| {
        if let egui::epaint::Shape::Text(text) = &shape.shape
            && text.galley.text() == label
        {
            let center = text.galley.rect.translate(text.pos.to_vec2()).center();
            return shape.clip_rect.contains(center).then_some(center);
        }
        None
    })
}

fn locate(
    probe: Res<Probe>,
    mut actions: ResMut<Actions>,
    output: Query<&EguiFullOutput, With<PrimaryEguiContext>>,
) {
    if probe.phase != Phase::Actions || actions.step != Step::Locate {
        return;
    }
    if let Ok(output) = output.single()
        && let Some(output) = output.0.as_ref()
    {
        actions.point = text_position(output, &actions.label);
    }
}

fn target(actions: &Actions, first: usize) -> usize {
    match actions.case {
        0 | 2 => first + 1,
        1 => first + 3,
        3 => actions.latest_frame,
        4 => 0,
        _ => unreachable!(),
    }
}

fn revision_delta(case: usize) -> u64 {
    if matches!(case, 1 | 2) { 2 } else { 1 }
}

fn completed(trace: &Trace, accepted: Stamp) -> Option<u64> {
    trace.with_records(|records| {
        records.iter().find_map(|record| {
            let submission = record.submission?;
            (record.stage == "gpu_completed"
                && submission.phase == 8
                && submission.tick >= accepted.tick
                && same_model(submission.scene, accepted)
                && same_model(submission.ui, accepted))
            .then_some(submission.serial)
        })
    })
}

fn start_case(actions: &mut Actions, studio: &Studio, probe: &Probe) {
    let language = studio.prefs.language;
    actions.label = match actions.case {
        0 => ">".into(),
        1 => language.text("▶ Play", "▶ 播放").into(),
        2 => language.text("◀ Reverse", "◀ 倒放").into(),
        3 => studio.live_follow_label(language).into(),
        4 => language.text("Close", "关闭").into(),
        _ => unreachable!(),
    };
    actions.point = None;
    actions.release = None;
    actions.release_passes = 0;
    actions.next_prepare = None;
    actions.final_stamp = None;
    actions.completion = None;
    actions.step = Step::Locate;
    probe.trace.set_stop(if matches!(actions.case, 1 | 2) {
        target(actions, probe.config.first)
    } else {
        usize::MAX
    });
}

fn drive_actions(
    mut commands: Commands,
    mut probe: ResMut<Probe>,
    mut studio: ResMut<Studio>,
    mut actions: ResMut<Actions>,
    display: DisplayWorld,
) {
    if probe.phase != Phase::Actions || probe.trace.failure().is_some() {
        return;
    }
    let synced = same_model(probe.scene, stamp(&studio, probe.tick))
        && same_model(probe.ui, stamp(&studio, probe.tick));
    match actions.step {
        Step::Fixture => {
            match studio.prepare_action_probe_live_fixture() {
                Ok(false) => return,
                Err(error) => {
                    probe.trace.fail(error);
                    return;
                }
                Ok(true) => (),
            }
            actions.latest_frame = studio.frame;
            if let Err(error) = validate(
                &mut probe,
                &studio,
                &display.viewport,
                &display.transforms,
                &display.meshes,
                "fixture-latest",
                true,
            ) {
                probe.trace.fail(error);
                return;
            }
            probe.action_fixture = Some(
                serde_json::json!({"name":"finite-live-snapshot", "latest_frame":studio.frame,"frame_count":studio.count(),"analysis_ready":true,"scope":"file capture snapshot; no TCP producer"}),
            );
            studio.seek(probe.config.first);
            actions.step = Step::Positioning;
        }
        Step::Positioning
            if synced && studio.seeking.is_none() && studio.frame == probe.config.first =>
        {
            start_case(&mut actions, &studio, &probe);
        }
        Step::Locate if synced && actions.point.is_some() => {
            actions.before = stamp(&studio, probe.tick);
            actions.before_state = Some(studio.state.clone());
            actions.step = Step::Move;
        }
        Step::Move if actions.move_tick == probe.tick => {
            actions.step = Step::Press;
        }
        Step::Press if actions.press_tick == probe.tick => {
            actions.step = Step::Release;
        }
        Step::Release if actions.release_tick == probe.tick => {
            if actions.release_passes < 2 || actions.release.is_none() || !synced {
                probe
                    .trace
                    .fail("real button release did not retain one model through two UI passes");
                return;
            }
            actions.before_state = None;
            actions.step = Step::Accepted;
        }
        Step::Accepted
            if synced
                && studio.seeking.is_none()
                && studio.playing == 0
                && studio.frame == target(&actions, probe.config.first) =>
        {
            if actions.next_prepare.is_none()
                || probe.tick <= actions.release_tick
                || studio.revision != actions.before.revision + revision_delta(actions.case)
            {
                probe
                    .trace
                    .fail("button action was omitted, duplicated, or accepted outside preparation");
                return;
            }
            let name = format!("action-{}", CASES[actions.case]);
            if actions.case == 4 {
                if studio.recording.is_some()
                    || studio.state != pvd_core::SceneState::default()
                    || !display.viewport.objects.is_empty()
                {
                    probe
                        .trace
                        .fail("Close retained a capture or scene objects");
                    return;
                }
                if let Err(error) =
                    display
                        .viewport
                        .validate_display(&studio, &display.transforms, &display.meshes)
                {
                    probe.trace.fail(error);
                    return;
                }
                probe.snapshots.push(serde_json::json!({"name":name,"frame":studio.frame,"revision":studio.revision,"empty_scene_equal":true,"objects":0,"rendered_shapes":0}));
            } else {
                if actions.case == 3 && !studio.follow_live {
                    probe.trace.fail("Return to live did not enable following");
                    return;
                }
                if let Err(error) = validate(
                    &mut probe,
                    &studio,
                    &display.viewport,
                    &display.transforms,
                    &display.meshes,
                    &name,
                    false,
                ) {
                    probe.trace.fail(error);
                    return;
                }
            }
            actions.final_stamp = Some(stamp(&studio, probe.tick));
            screenshot(
                &mut commands,
                &mut probe,
                &display.check,
                &format!("{name}.png"),
            );
            actions.step = Step::Completion;
        }
        Step::Completion => {
            let accepted = actions.final_stamp.unwrap();
            if !same_model(accepted, stamp(&studio, probe.tick)) || !synced {
                probe
                    .trace
                    .fail("accepted button frame changed before its screenshot and GPU completion");
                return;
            }
            actions.completion = completed(&probe.trace, accepted);
            if probe.screenshot_pending || actions.completion.is_none() {
                return;
            }
            probe.actions.push(serde_json::json!({
                "name":CASES[actions.case],"label":actions.label,
                "input":{"move_tick":actions.move_tick,"press_tick":actions.press_tick,"release_tick":actions.release_tick},
                "before":actions.before,"release":actions.release,"release_passes":actions.release_passes,
                "next_prepare":actions.next_prepare,"final_stamp":accepted,
                "expected_revision_delta":revision_delta(actions.case),
                "full_scene_state_equal":actions.case != 4,"empty_scene_equal":actions.case == 4,
                "screenshot":format!("action-{}.png",CASES[actions.case]),"gpu_completion_serial":actions.completion
            }));
            actions.case += 1;
            if actions.case == CASES.len() {
                probe.trace.set_phase(6);
                probe.phase = Phase::Drain;
                probe.phase_tick = probe.tick;
            } else {
                start_case(&mut actions, &studio, &probe);
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_text_geometry_uses_the_final_output_and_respects_clipping() {
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let _ = ui.button("Close");
            });
        });
        assert!(text_position(&output, "Close").is_some());
        assert!(text_position(&output, "unknown").is_none());
        let mut clipped = output;
        for shape in &mut clipped.shapes {
            shape.clip_rect = egui::Rect::NOTHING;
        }
        assert!(text_position(&clipped, "Close").is_none());
    }

    #[test]
    fn gpu_case_completion_requires_the_actual_accepted_ui_and_scene_token() {
        let trace = Trace::default();
        let expected = Stamp {
            frame: 7,
            revision: 20,
            request: 3,
            tick: 100,
            analysis_ready: true,
        };
        let token = |serial, tick, scene, ui| Submission {
            serial,
            tick,
            phase: 8,
            ui_passes: 2,
            scene,
            ui,
        };
        for rejected in [
            token(1, 99, expected, expected),
            token(
                2,
                100,
                Stamp {
                    revision: 19,
                    ..expected
                },
                expected,
            ),
            token(
                3,
                100,
                expected,
                Stamp {
                    frame: 8,
                    ..expected
                },
            ),
        ] {
            assert!(trace.submitted(rejected));
            trace.completed(rejected);
        }
        assert_eq!(completed(&trace, expected), None);
        let accepted = token(
            4,
            101,
            Stamp {
                tick: 101,
                ..expected
            },
            Stamp {
                tick: 101,
                ..expected
            },
        );
        assert!(trace.submitted(accepted));
        assert_eq!(completed(&trace, expected), None);
        trace.completed(accepted);
        assert_eq!(completed(&trace, expected), Some(4));
    }
}
