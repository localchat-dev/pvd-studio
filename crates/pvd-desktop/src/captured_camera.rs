use bevy::prelude::Vec3;

use crate::{coordinates::Coordinates, scene::Viewport, studio::Studio};

#[derive(Clone, Copy, Debug)]
pub struct Pose {
    pub position: Vec3,
    pub target: Vec3,
    pub up: Vec3,
}

impl Pose {
    pub fn from_source(camera: &pvd_core::SourceCamera, coordinates: Coordinates) -> Option<Self> {
        let position = coordinates.point(Vec3::from(camera.position));
        let target = coordinates.point(Vec3::from(camera.target));
        let source_up = coordinates.direction(Vec3::from(camera.up));
        if !position.is_finite() || !target.is_finite() || !source_up.is_finite() {
            return None;
        }
        let forward = (target - position).try_normalize()?;
        let right = forward.cross(source_up.try_normalize()?).try_normalize()?;
        Some(Self {
            position,
            target,
            up: right.cross(forward).try_normalize()?,
        })
    }
}

pub fn sync(studio: &Studio, viewport: &mut Viewport) {
    viewport.captured_pose = studio
        .captured_camera
        .as_ref()
        .and_then(|name| studio.state.cameras.get(name))
        .and_then(|camera| Pose::from_source(camera, viewport.coordinates));
}

pub fn show(ui: &mut egui::Ui, studio: &mut Studio, viewport: &mut Viewport) {
    let language = studio.prefs.language;
    let previous = studio.captured_camera.clone();
    ui.label(language.text("View", "视角"));
    egui::ComboBox::from_id_salt("captured-camera")
        .selected_text(
            studio
                .captured_camera
                .as_deref()
                .unwrap_or(language.text("Free camera", "自由视角")),
        )
        .width(ui.available_width().clamp(40.0, 180.0))
        .truncate()
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut studio.captured_camera,
                None,
                language.text("Free camera", "自由视角"),
            );
            for name in studio.state.cameras.keys() {
                ui.selectable_value(&mut studio.captured_camera, Some(name.clone()), name);
            }
        });
    if previous != studio.captured_camera {
        studio.focus_selected = false;
        studio.follow_selected = false;
        crate::camera_controller::cancel_gesture(ui.ctx());
        sync(studio, viewport);
        ui.ctx().request_repaint();
    }
    if let Some(name) = &studio.captured_camera {
        match studio.state.cameras.get(name) {
            None => {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    language.text(
                        "This camera is absent from the displayed frame; showing the free camera.",
                        "当前显示帧尚无此相机，暂显示自由视角。",
                    ),
                );
            }
            Some(camera) if Pose::from_source(camera, viewport.coordinates).is_none() => {
                ui.colored_label(egui::Color32::YELLOW, language.text(
                    "This camera has an invalid position, viewing direction or up vector; showing the free camera.",
                    "此相机的位置、观察方向或向上向量无效，暂显示自由视角。",
                ));
            }
            Some(_) => {
                ui.label(language.text(
                    "Follows camera data in the displayed frame. Field of view and clipping keep the current settings.",
                    "跟随当前显示帧的相机数据；视角和裁剪范围沿用当前设置。",
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinates::{Handedness, UpAxis};

    fn camera() -> pvd_core::SourceCamera {
        pvd_core::SourceCamera {
            position: [0.0, 0.0, 10.0],
            target: [0.0; 3],
            up: [1.0, 1.0, 0.0],
        }
    }

    #[test]
    fn tilted_source_cameras_preserve_projection_and_picking_in_every_coordinate_system() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 720.0));
        for up in UpAxis::ALL
            .into_iter()
            .map(UpAxis::vector)
            .chain([Vec3::new(1.0, 2.0, -3.0), Vec3::new(-3.0, 1.0, 2.0)])
        {
            for hand in [Handedness::Right, Handedness::Left] {
                for scale in [1e-6, 0.001, 1.0, 1000.0, 1e6] {
                    let coordinates = Coordinates::from_up_vector(up, hand, scale).unwrap();
                    let pose = Pose::from_source(&camera(), coordinates).unwrap();
                    let mut viewport = Viewport::default();
                    viewport.coordinates = coordinates;
                    viewport.captured_pose = Some(pose);
                    viewport.near = scale * 0.001;
                    viewport.far = scale * 100.0;
                    viewport.objects = vec![pvd_core::RenderObject {
                        id: 17,
                        shape_id: 18,
                        position: [0.0; 3],
                        rotation: [0.0, 0.0, 0.0, 1.0],
                        geometry: pvd_core::Geometry::Box {
                            half_extents: [0.5; 3],
                        },
                        dynamic: false,
                        sleeping: false,
                    }];
                    let (center, _) = viewport.project(Vec3::ZERO, rect).unwrap();
                    assert!(center.distance(rect.center()) < 0.001);
                    assert_eq!(viewport.select_at(center, rect), Some(17));
                    let up = coordinates.source_direction(pose.up);
                    assert!(viewport.project(up, rect).unwrap().0.y < center.y);
                    let right = (pose.target - pose.position).normalize().cross(pose.up);
                    assert!(
                        viewport
                            .project(coordinates.source_direction(right), rect)
                            .unwrap()
                            .0
                            .x
                            > center.x
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_or_absent_cameras_never_reuse_a_previously_displayed_pose() {
        let mut invalid = camera();
        invalid.position[0] = f32::NAN;
        assert!(Pose::from_source(&invalid, Coordinates::default()).is_none());
        invalid = camera();
        invalid.target = invalid.position;
        assert!(Pose::from_source(&invalid, Coordinates::default()).is_none());
        invalid = camera();
        invalid.up = [0.0, 0.0, 1.0];
        assert!(Pose::from_source(&invalid, Coordinates::default()).is_none());
        let mut studio = Studio::default();
        let mut viewport = Viewport::default();
        studio.captured_camera = Some("source".into());
        sync(&studio, &mut viewport);
        assert!(viewport.captured_pose.is_none());
        studio.state.cameras.insert("source".into(), camera());
        sync(&studio, &mut viewport);
        assert_eq!(viewport.position(), Vec3::new(0.0, 0.0, 10.0));
        studio.state.cameras.clear();
        sync(&studio, &mut viewport);
        assert!(viewport.captured_pose.is_none());
        studio.close();
        sync(&studio, &mut viewport);
        assert!(studio.captured_camera.is_none());
        assert!(viewport.captured_pose.is_none());
    }
}
