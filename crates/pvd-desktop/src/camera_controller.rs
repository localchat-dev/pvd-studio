use std::time::Duration;

use bevy::prelude::Vec3;
use serde::{Deserialize, Serialize};

use crate::{i18n::Language, scene::Viewport};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraMode {
    Orbit,
    #[default]
    Fly,
}

impl CameraMode {
    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::Orbit => language.text("Orbit", "轨道"),
            Self::Fly => language.text("First person", "第一人称"),
        }
    }

    pub fn help(self, language: Language) -> &'static str {
        match self {
            Self::Orbit => language.text(
                "Right drag: orbit · Middle: pan · Wheel: zoom",
                "右键环绕 · 中键平移 · 滚轮缩放",
            ),
            Self::Fly => language.text(
                "Hold right: look + WASD / QE · Shift: 2× · Wheel: speed · Esc: release",
                "按住右键：转向 + WASD / QE · Shift：2倍 · 滚轮调速 · Esc 释放",
            ),
        }
    }
}

pub fn viewport_id() -> egui::Id {
    egui::Id::new("scene-camera-input")
}

#[derive(Clone, Default)]
struct InputState {
    frame: Option<u64>,
    applied_frame: Option<u64>,
    blocked_at_start: bool,
    ime_composing: bool,
    gesture: Option<CameraMode>,
}

fn state_id() -> egui::Id {
    viewport_id().with("state")
}

fn other_widget_focused(context: &egui::Context) -> bool {
    context.memory(|memory| memory.focused().is_some_and(|id| id != viewport_id()))
}

pub fn begin_frame(context: &egui::Context) {
    let mut state =
        context.data(|data| data.get_temp::<InputState>(state_id()).unwrap_or_default());
    let frame = context.cumulative_frame_nr();
    if state.frame == Some(frame) {
        return;
    }
    state.frame = Some(frame);
    if !context.input(|input| input.pointer.secondary_down()) {
        context.memory_mut(|memory| memory.surrender_focus(viewport_id()));
    }
    state.blocked_at_start = other_widget_focused(context);
    context.input(|input| {
        for event in &input.events {
            if let egui::Event::Ime(event) = event {
                state.blocked_at_start = true;
                state.ime_composing =
                    matches!(event, egui::ImeEvent::Enabled | egui::ImeEvent::Preedit(_));
            }
        }
        if !input.focused {
            state.ime_composing = false;
        }
        if !input.focused
            || state.blocked_at_start
            || state.ime_composing
            || !input.pointer.secondary_down()
            || input.key_pressed(egui::Key::Escape)
        {
            state.gesture = None;
        }
    });
    context.data_mut(|data| data.insert_temp(state_id(), state));
}

pub fn keyboard_input_blocked(context: &egui::Context) -> bool {
    !context.input(|input| input.focused)
        || other_widget_focused(context)
        || context.data(|data| {
            data.get_temp::<InputState>(state_id())
                .is_some_and(|state| state.blocked_at_start || state.ime_composing)
        })
}

pub fn cancel_gesture(context: &egui::Context) {
    let frame = context.cumulative_frame_nr();
    context.data_mut(|data| {
        let mut state = data.get_temp::<InputState>(state_id()).unwrap_or_default();
        state.gesture = None;
        state.frame = Some(frame);
        state.applied_frame = Some(frame);
        state.blocked_at_start = true;
        data.insert_temp(state_id(), state);
    });
    context.memory_mut(|memory| memory.surrender_focus(viewport_id()));
}

pub fn fly_speed(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.001, 1e9)
    } else {
        10.0
    }
}

#[derive(Default)]
struct Motion {
    look: egui::Vec2,
    pan: egui::Vec2,
    scroll: f32,
    movement: Vec3,
    seconds: f32,
    fast: bool,
}

fn apply(
    viewport: &mut Viewport,
    mode: CameraMode,
    sensitivity: f32,
    speed: &mut f32,
    motion: Motion,
) -> bool {
    let before = (
        viewport.target,
        viewport.yaw,
        viewport.pitch,
        viewport.distance,
    );
    let old_speed = *speed;
    *speed = fly_speed(*speed);
    let sensitivity = if sensitivity.is_finite() {
        sensitivity.clamp(0.05, 20.0)
    } else {
        1.0
    };
    if motion.look.is_finite() && motion.look != egui::Vec2::ZERO {
        let eye = viewport.position();
        viewport.yaw =
            (viewport.yaw - motion.look.x * 0.006 * sensitivity).rem_euclid(std::f32::consts::TAU);
        viewport.pitch = (viewport.pitch + motion.look.y * 0.006 * sensitivity).clamp(-1.54, 1.54);
        if mode == CameraMode::Fly {
            viewport.target += eye - viewport.position();
        }
    }
    let forward = (viewport.target - viewport.position()).normalize_or_zero();
    let right = forward.cross(Vec3::Y).normalize_or_zero();
    if motion.pan.is_finite() {
        let up = right.cross(forward).normalize_or_zero();
        let target = viewport.target
            + (-right * motion.pan.x + up * motion.pan.y)
                * viewport.distance
                * 0.0015
                * sensitivity;
        if target.is_finite() {
            viewport.target = target;
        }
    }
    if motion.scroll.is_finite() && motion.scroll != 0.0 {
        match mode {
            CameraMode::Orbit => {
                viewport.distance = (viewport.distance
                    * (-motion.scroll * 0.002 * sensitivity)
                        .clamp(-20.0, 20.0)
                        .exp())
                .clamp(
                    0.01 * viewport.coordinates.scale,
                    1e12 * viewport.coordinates.scale,
                );
            }
            CameraMode::Fly => {
                *speed = fly_speed(*speed * (motion.scroll * 0.002).clamp(-20.0, 20.0).exp());
            }
        }
    }
    if mode == CameraMode::Fly && motion.seconds.is_finite() && motion.movement.is_finite() {
        let direction =
            (right * motion.movement.x + Vec3::Y * motion.movement.y + forward * motion.movement.z)
                .normalize_or_zero();
        let delta = direction
            * *speed
            * viewport.coordinates.scale
            * motion.seconds.clamp(0.0, 0.05)
            * if motion.fast { 2.0 } else { 1.0 };
        if (viewport.target + delta).is_finite() {
            viewport.target += delta;
        }
    }
    before
        != (
            viewport.target,
            viewport.yaw,
            viewport.pitch,
            viewport.distance,
        )
        || old_speed != *speed
}

pub fn update(
    viewport: &mut Viewport,
    response: &egui::Response,
    mode: CameraMode,
    sensitivity: f32,
    speed: &mut f32,
) -> bool {
    let context = &response.ctx;
    begin_frame(context);
    let mut state =
        context.data(|data| data.get_temp::<InputState>(state_id()).unwrap_or_default());
    let frame = context.cumulative_frame_nr();
    if state.applied_frame == Some(frame) {
        return false;
    }
    state.applied_frame = Some(frame);
    if state.gesture.is_some_and(|armed_mode| armed_mode != mode) {
        state.gesture = None;
    }
    if context.input(|input| input.key_pressed(egui::Key::Escape)) {
        response.surrender_focus();
    }
    let blocked = keyboard_input_blocked(context);
    let motion = if blocked {
        state.gesture = None;
        Motion::default()
    } else {
        let pressed =
            context.input(|input| input.pointer.button_pressed(egui::PointerButton::Secondary));
        if pressed && response.is_pointer_button_down_on() && response.contains_pointer() {
            state.gesture = Some(mode);
            response.request_focus();
        }
        let active = state.gesture == Some(mode)
            && response.has_focus()
            && response.is_pointer_button_down_on()
            && context.input(|input| input.pointer.secondary_down());
        let panning = !active && response.dragged_by(egui::PointerButton::Middle);
        context.input(|input| {
            let keys = active
                && mode == CameraMode::Fly
                && !input.modifiers.command
                && !input.modifiers.ctrl
                && !input.modifiers.alt;
            let axis = |positive, negative| {
                i32::from(keys && input.key_down(positive)) as f32
                    - i32::from(keys && input.key_down(negative)) as f32
            };
            Motion {
                look: if active && !pressed {
                    input.pointer.delta()
                } else {
                    egui::Vec2::ZERO
                },
                pan: if panning {
                    input.pointer.delta()
                } else {
                    egui::Vec2::ZERO
                },
                scroll: if response.hovered() {
                    input.smooth_scroll_delta.y
                } else {
                    0.0
                },
                movement: Vec3::new(
                    axis(egui::Key::D, egui::Key::A),
                    axis(egui::Key::E, egui::Key::Q),
                    axis(egui::Key::W, egui::Key::S),
                ),
                seconds: if pressed { 0.0 } else { input.unstable_dt },
                fast: input.modifiers.shift,
            }
        })
    };
    let moving = motion.movement != Vec3::ZERO;
    let camera_before = (
        viewport.target,
        viewport.yaw,
        viewport.pitch,
        viewport.distance,
    );
    let changed = apply(viewport, mode, sensitivity, speed, motion);
    context.data_mut(|data| data.insert_temp(state_id(), state));
    if moving {
        context.request_repaint_after(Duration::from_millis(16));
    } else if changed {
        context.request_repaint();
    }
    camera_before
        != (
            viewport.target,
            viewport.yaw,
            viewport.pitch,
            viewport.distance,
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::{Coordinates, Handedness, UpAxis};

    #[test]
    fn mode_changes_keep_pose_and_rotation_uses_the_correct_pivot() {
        let mut viewport = Viewport::default();
        let mut speed = 10.0;
        let target = viewport.target;
        let eye = viewport.position();
        for mode in [CameraMode::Fly, CameraMode::Orbit, CameraMode::Fly] {
            assert!(!apply(
                &mut viewport,
                mode,
                1.0,
                &mut speed,
                Motion::default()
            ));
            assert_eq!(viewport.target, target);
            assert_eq!(viewport.position(), eye);
        }
        apply(
            &mut viewport,
            CameraMode::Fly,
            1.0,
            &mut speed,
            Motion {
                look: egui::vec2(35.0, -16.0),
                ..Default::default()
            },
        );
        assert!(viewport.position().abs_diff_eq(eye, 1e-5));
        assert!(!viewport.target.abs_diff_eq(target, 0.01));
        let fly_target = viewport.target;
        let fly_eye = viewport.position();
        assert!(!apply(
            &mut viewport,
            CameraMode::Orbit,
            1.0,
            &mut speed,
            Motion::default()
        ));
        assert_eq!(viewport.target, fly_target);
        assert_eq!(viewport.position(), fly_eye);
        apply(
            &mut viewport,
            CameraMode::Orbit,
            1.0,
            &mut speed,
            Motion {
                look: egui::vec2(-25.0, 8.0),
                ..Default::default()
            },
        );
        assert_eq!(viewport.target, fly_target);
        assert!(!viewport.position().abs_diff_eq(fly_eye, 0.01));
    }

    #[test]
    fn flight_respects_source_units_up_axis_and_bounded_elapsed_time() {
        for up in UpAxis::ALL {
            for hand in [Handedness::Left, Handedness::Right] {
                for scale in [1e-6, 0.001, 1.0, 1000.0, 1e6] {
                    let mut viewport = Viewport::default();
                    viewport.coordinates = Coordinates::new(up, hand, scale);
                    viewport.distance *= scale;
                    let mut speed = 10.0;
                    apply(
                        &mut viewport,
                        CameraMode::Fly,
                        1.0,
                        &mut speed,
                        Motion {
                            movement: Vec3::Y,
                            seconds: 0.05,
                            ..Default::default()
                        },
                    );
                    assert!(
                        viewport
                            .coordinates
                            .source_point(viewport.target)
                            .abs_diff_eq(up.vector() * 0.5, 1e-5)
                    );
                    viewport.target = Vec3::ZERO;
                    let eye = viewport.position();
                    apply(
                        &mut viewport,
                        CameraMode::Fly,
                        1.0,
                        &mut speed,
                        Motion {
                            movement: Vec3::new(1.0, 1.0, 1.0),
                            seconds: 1000.0,
                            fast: true,
                            ..Default::default()
                        },
                    );
                    assert!((viewport.target.length() / scale - 1.0).abs() < 1e-5);
                    assert!((viewport.position() - eye).abs_diff_eq(viewport.target, scale * 1e-5));
                }
            }
        }
        let mut viewport = Viewport::default();
        let mut speed = f32::NAN;
        apply(
            &mut viewport,
            CameraMode::Fly,
            1.0,
            &mut speed,
            Motion {
                seconds: f32::INFINITY,
                movement: Vec3::Z,
                ..Default::default()
            },
        );
        assert_eq!(speed, 10.0);
        assert_eq!(viewport.target, Vec3::ZERO);
    }

    #[test]
    fn orbit_ignores_flight_keys_and_wheel_behavior_is_mode_specific() {
        let mut viewport = Viewport::default();
        let mut speed = 10.0;
        assert!(!apply(
            &mut viewport,
            CameraMode::Orbit,
            1.0,
            &mut speed,
            Motion {
                movement: Vec3::ONE,
                seconds: 0.05,
                ..Default::default()
            }
        ));
        let distance = viewport.distance;
        apply(
            &mut viewport,
            CameraMode::Orbit,
            1.0,
            &mut speed,
            Motion {
                scroll: 20.0,
                ..Default::default()
            },
        );
        assert!(viewport.distance < distance);
        assert_eq!(speed, 10.0);
        let eye = viewport.position();
        apply(
            &mut viewport,
            CameraMode::Fly,
            1.0,
            &mut speed,
            Motion {
                scroll: 20.0,
                ..Default::default()
            },
        );
        assert_eq!(viewport.position(), eye);
        assert!(speed > 10.0);
    }

    struct Rig {
        context: egui::Context,
        viewport: Viewport,
        speed: f32,
        time: f64,
        text: String,
        modifiers: egui::Modifiers,
        repaint_delay: Duration,
    }

    impl Default for Rig {
        fn default() -> Self {
            Self {
                context: egui::Context::default(),
                viewport: Viewport::default(),
                speed: 10.0,
                time: 0.0,
                text: "物理场景".into(),
                modifiers: egui::Modifiers::NONE,
                repaint_delay: Duration::MAX,
            }
        }
    }

    impl Rig {
        fn frame(
            &mut self,
            events: Vec<egui::Event>,
            focused: bool,
            edit: bool,
            mode: CameraMode,
        ) -> bool {
            self.time += 1.0 / 60.0;
            let context = self.context.clone();
            let mut changed = false;
            let output = context.run(
                egui::RawInput {
                    time: Some(self.time),
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 480.0),
                    )),
                    focused,
                    modifiers: self.modifiers,
                    events,
                    ..Default::default()
                },
                |context| {
                    begin_frame(context);
                    egui::CentralPanel::default().show(context, |ui| {
                        let editor = ui.add(
                            egui::TextEdit::singleline(&mut self.text).id(egui::Id::new("editor")),
                        );
                        if edit {
                            editor.request_focus();
                        }
                        let response = ui.interact(
                            egui::Rect::from_min_max(
                                egui::pos2(20.0, 70.0),
                                egui::pos2(580.0, 450.0),
                            ),
                            viewport_id(),
                            egui::Sense::click_and_drag(),
                        );
                        changed |=
                            update(&mut self.viewport, &response, mode, 1.0, &mut self.speed);
                    });
                },
            );
            self.repaint_delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            changed
        }

        fn arm(&mut self) {
            self.frame(
                vec![egui::Event::PointerMoved(egui::pos2(200.0, 200.0))],
                true,
                false,
                CameraMode::Fly,
            );
            assert!(!self.frame(
                vec![
                    button(true, egui::pos2(200.0, 200.0)),
                    key(egui::Key::W, true)
                ],
                true,
                false,
                CameraMode::Fly
            ));
            assert!(
                self.context
                    .memory(|memory| memory.has_focus(viewport_id()))
            );
        }
    }

    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn button(pressed: bool, pos: egui::Pos2) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn egui_flight_requires_a_viewport_press_and_stops_on_release_or_lost_focus() {
        let mut rig = Rig::default();
        rig.arm();
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
        let moved = rig.viewport.target;
        assert!(!rig.frame(
            vec![button(false, egui::pos2(200.0, 200.0))],
            true,
            false,
            CameraMode::Fly
        ));
        assert_eq!(rig.viewport.target, moved);
        assert!(!rig.context.wants_keyboard_input());
        rig.arm();
        assert!(!rig.frame(vec![], false, false, CameraMode::Fly));
        assert!(!rig.frame(vec![key(egui::Key::W, true)], true, false, CameraMode::Fly));

        let mut outside = Rig::default();
        outside.frame(
            vec![egui::Event::PointerMoved(egui::pos2(610.0, 20.0))],
            true,
            false,
            CameraMode::Fly,
        );
        outside.frame(
            vec![
                button(true, egui::pos2(610.0, 20.0)),
                key(egui::Key::W, true),
            ],
            true,
            false,
            CameraMode::Fly,
        );
        assert!(!outside.frame(
            vec![egui::Event::PointerMoved(egui::pos2(200.0, 200.0))],
            true,
            false,
            CameraMode::Fly
        ));
        assert_eq!(outside.viewport.target, Vec3::ZERO);
    }

    #[test]
    fn egui_text_and_ime_cancel_flight_until_a_fresh_viewport_press() {
        let mut rig = Rig::default();
        rig.arm();
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
        let moved = rig.viewport.target;
        assert!(!rig.frame(vec![], true, true, CameraMode::Fly));
        assert!(keyboard_input_blocked(&rig.context));
        assert!(!rig.frame(
            vec![egui::Event::Ime(egui::ImeEvent::Preedit("场景".into()))],
            true,
            true,
            CameraMode::Fly
        ));
        rig.context
            .memory_mut(|memory| memory.surrender_focus(egui::Id::new("editor")));
        assert!(!rig.frame(
            vec![egui::Event::Ime(egui::ImeEvent::Commit("场景".into()))],
            true,
            false,
            CameraMode::Fly
        ));
        assert!(!rig.frame(vec![], true, false, CameraMode::Fly));
        assert_eq!(rig.viewport.target, moved);
        rig.frame(
            vec![button(false, egui::pos2(200.0, 200.0))],
            true,
            false,
            CameraMode::Fly,
        );
        rig.arm();
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
    }

    #[test]
    fn egui_mode_switch_and_escape_do_not_continue_a_held_gesture() {
        let mut rig = Rig::default();
        rig.arm();
        let eye = rig.viewport.position();
        let target = rig.viewport.target;
        assert!(!rig.frame(vec![], true, false, CameraMode::Orbit));
        assert!(!rig.frame(vec![], true, false, CameraMode::Fly));
        assert_eq!(rig.viewport.position(), eye);
        assert_eq!(rig.viewport.target, target);
        rig.frame(
            vec![button(false, egui::pos2(200.0, 200.0))],
            true,
            false,
            CameraMode::Fly,
        );
        rig.arm();
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
        assert!(!rig.frame(
            vec![key(egui::Key::Escape, true)],
            true,
            false,
            CameraMode::Fly
        ));
        assert!(!rig.frame(vec![], true, false, CameraMode::Fly));
    }

    #[test]
    fn modal_cancellation_stops_held_flight_until_a_fresh_press() {
        let mut rig = Rig::default();
        rig.arm();
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
        let moved = rig.viewport.target;
        cancel_gesture(&rig.context);
        for _ in 0..3 {
            assert!(!rig.frame(vec![], true, false, CameraMode::Fly));
        }
        assert_eq!(rig.viewport.target, moved);
        rig.frame(
            vec![button(false, egui::pos2(200.0, 200.0))],
            true,
            false,
            CameraMode::Fly,
        );
        rig.arm();
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
    }

    #[test]
    fn egui_shift_doubles_speed_and_modifier_shortcuts_do_not_move_the_camera() {
        let mut rig = Rig::default();
        rig.viewport.yaw = 0.0;
        rig.viewport.pitch = 0.0;
        rig.arm();
        assert!(!keyboard_input_blocked(&rig.context));
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
        let normal = rig.viewport.target;
        assert!(normal.abs_diff_eq(Vec3::NEG_Z * (10.0 / 60.0), 1e-5));
        assert!(rig.repaint_delay <= Duration::from_millis(16));
        rig.modifiers.shift = true;
        assert!(rig.frame(vec![], true, false, CameraMode::Fly));
        assert!((rig.viewport.target - normal).abs_diff_eq(normal * 2.0, 1e-5));
        let shifted = rig.viewport.target;
        rig.modifiers.ctrl = true;
        assert!(!rig.frame(vec![], true, false, CameraMode::Fly));
        assert_eq!(rig.viewport.target, shifted);
        rig.modifiers = egui::Modifiers::NONE;
        assert!(rig.frame(
            vec![
                key(egui::Key::W, false),
                key(egui::Key::D, true),
                key(egui::Key::E, true)
            ],
            true,
            false,
            CameraMode::Fly,
        ));
        assert!(
            (rig.viewport.target - shifted)
                .abs_diff_eq(Vec3::new(1.0, 1.0, 0.0).normalize() * (10.0 / 60.0), 1e-5)
        );
    }

    #[test]
    fn egui_middle_drag_pans_without_rotating_in_both_modes() {
        for mode in [CameraMode::Orbit, CameraMode::Fly] {
            let mut rig = Rig::default();
            rig.frame(
                vec![egui::Event::PointerMoved(egui::pos2(200.0, 200.0))],
                true,
                false,
                mode,
            );
            rig.frame(
                vec![egui::Event::PointerButton {
                    pos: egui::pos2(200.0, 200.0),
                    button: egui::PointerButton::Middle,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                true,
                false,
                mode,
            );
            let angles = (rig.viewport.yaw, rig.viewport.pitch, rig.viewport.distance);
            let eye = rig.viewport.position();
            assert!(rig.frame(
                vec![egui::Event::PointerMoved(egui::pos2(240.0, 220.0))],
                true,
                false,
                mode
            ));
            assert_eq!(
                (rig.viewport.yaw, rig.viewport.pitch, rig.viewport.distance),
                angles
            );
            assert!((rig.viewport.position() - eye).abs_diff_eq(rig.viewport.target, 1e-5));
        }
    }

    #[test]
    fn camera_preferences_default_to_fly_and_preserve_selected_mode() {
        let mut stored = serde_json::to_value(crate::studio::Preferences::default()).unwrap();
        stored.as_object_mut().unwrap().remove("camera_mode");
        stored.as_object_mut().unwrap().remove("fly_speed");
        stored.as_object_mut().unwrap().remove("show_viewport_help");
        let mut prefs: crate::studio::Preferences = serde_json::from_value(stored).unwrap();
        assert_eq!(prefs.camera_mode, CameraMode::Fly);
        assert_eq!(prefs.fly_speed, 10.0);
        assert!(prefs.show_viewport_help);
        prefs.fly_speed = 123.5;
        prefs.show_viewport_help = false;
        for mode in [CameraMode::Orbit, CameraMode::Fly] {
            prefs.camera_mode = mode;
            let restored: crate::studio::Preferences =
                serde_json::from_value(serde_json::to_value(&prefs).unwrap()).unwrap();
            assert_eq!(restored.camera_mode, mode);
            assert_eq!(restored.fly_speed, 123.5);
            assert!(!restored.show_viewport_help);
        }
        assert_ne!(
            CameraMode::Fly.help(Language::Chinese),
            CameraMode::Fly.help(Language::English)
        );
    }
}
