use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::i18n::Language;

mod up_axis;
pub(crate) use up_axis::UpResolver;
pub use up_axis::{UpMode, UpResolution, UpSettings};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpAxis {
    X,
    #[default]
    Y,
    Z,
    NegativeX,
    NegativeY,
    NegativeZ,
}

impl UpAxis {
    pub const ALL: [Self; 6] = [
        Self::X,
        Self::Y,
        Self::Z,
        Self::NegativeX,
        Self::NegativeY,
        Self::NegativeZ,
    ];

    pub fn vector(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
            Self::NegativeX => Vec3::NEG_X,
            Self::NegativeY => Vec3::NEG_Y,
            Self::NegativeZ => Vec3::NEG_Z,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::X => "+X",
            Self::Y => "+Y",
            Self::Z => "+Z",
            Self::NegativeX => "−X",
            Self::NegativeY => "−Y",
            Self::NegativeZ => "−Z",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Handedness {
    #[default]
    Right,
    Left,
}

impl Handedness {
    pub fn label(self, language: Language) -> &'static str {
        match self {
            Self::Right => language.text("Right handed", "右手坐标系"),
            Self::Left => language.text("Left handed", "左手坐标系"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinates {
    basis: Mat3,
    pub scale: f32,
    pub mirrored: bool,
}

impl Default for Coordinates {
    fn default() -> Self {
        Self::new(UpAxis::Y, Handedness::Right, 1.0)
    }
}

impl Coordinates {
    pub fn new(up: UpAxis, handedness: Handedness, meters_per_unit: f32) -> Self {
        Self::from_unit_up(up.vector(), handedness, meters_per_unit)
    }

    pub fn from_up_vector(up: Vec3, handedness: Handedness, meters_per_unit: f32) -> Option<Self> {
        Some(Self::from_unit_up(
            up_axis::unit_vector(up)?,
            handedness,
            meters_per_unit,
        ))
    }

    fn from_unit_up(up: Vec3, handedness: Handedness, meters_per_unit: f32) -> Self {
        let rotation = Mat3::from_quat(Quat::from_rotation_arc(up, Vec3::Y));
        let mirrored = handedness == Handedness::Left;
        let reflection = Mat3::from_diagonal(if mirrored {
            Vec3::new(-1.0, 1.0, 1.0)
        } else {
            Vec3::ONE
        });
        Self {
            basis: reflection * rotation,
            scale: if meters_per_unit.is_finite() {
                meters_per_unit.clamp(1e-6, 1e6)
            } else {
                1.0
            },
            mirrored,
        }
    }

    pub fn point(self, value: Vec3) -> Vec3 {
        self.basis * value * self.scale
    }
    pub fn source_point(self, value: Vec3) -> Vec3 {
        self.basis.transpose() * value / self.scale
    }
    pub fn source_direction(self, value: Vec3) -> Vec3 {
        self.basis.transpose() * value
    }
    pub fn direction(self, value: Vec3) -> Vec3 {
        self.basis * value
    }
    pub fn axial_direction(self, value: Vec3) -> Vec3 {
        self.basis * value * if self.mirrored { -1.0 } else { 1.0 }
    }
    pub fn transform(self, source: Transform) -> Transform {
        Transform::from_matrix(Mat4::from_mat3(self.basis * self.scale) * source.to_matrix())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_angular_velocity_matches_the_derivative_of_displayed_rotation() {
        let point = Vec3::new(1.0, -2.0, 3.0);
        let angular_velocity = Vec3::new(0.5, 2.0, -1.0);
        for up in UpAxis::ALL {
            for hand in [Handedness::Right, Handedness::Left] {
                let coordinates = Coordinates::new(up, hand, 0.001);
                let actual = coordinates
                    .axial_direction(angular_velocity)
                    .cross(coordinates.point(point));
                let expected = coordinates.point(angular_velocity.cross(point));
                assert!(actual.abs_diff_eq(expected, 1e-8));
            }
        }
    }

    #[test]
    fn every_basis_preserves_pose_composition_normals_and_round_trips() {
        let source = Transform::from_translation(Vec3::new(3.0, 2.0, -4.0))
            .with_rotation(Quat::from_euler(EulerRot::XYZ, 0.2, -0.6, 1.0));
        let triangle = [
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(0.0, 3.0, 0.0),
        ];
        for up in UpAxis::ALL {
            for hand in [Handedness::Left, Handedness::Right] {
                for scale in [0.001, 1.0, 1000.0] {
                    let coordinates = Coordinates::new(up, hand, scale);
                    assert!(
                        coordinates
                            .point(up.vector())
                            .abs_diff_eq(Vec3::Y * scale, scale * 1e-5)
                    );
                    let transform = coordinates.transform(source);
                    for point in triangle {
                        let raw = source.transform_point(point);
                        assert!(
                            coordinates
                                .source_point(coordinates.point(raw))
                                .abs_diff_eq(raw, 1e-5)
                        );
                        assert!(
                            transform
                                .transform_point(point)
                                .abs_diff_eq(coordinates.point(raw), scale * 1e-5)
                        );
                    }
                    let mut displayed =
                        triangle.map(|point| coordinates.point(source.transform_point(point)));
                    if coordinates.mirrored {
                        displayed.swap(1, 2);
                    }
                    let normal = (displayed[1] - displayed[0])
                        .cross(displayed[2] - displayed[0])
                        .normalize();
                    let expected = coordinates.basis * (source.rotation * Vec3::Z);
                    assert!(normal.abs_diff_eq(expected, 1e-5));
                }
            }
        }
    }

    #[test]
    fn arbitrary_up_vectors_preserve_geometry_and_axial_vector_transforms() {
        let point = Vec3::new(1.0, -2.0, 3.0);
        let angular = Vec3::new(0.5, 2.0, -1.0);
        let source = Transform::from_translation(Vec3::new(3.0, 2.0, -4.0))
            .with_rotation(Quat::from_euler(EulerRot::XYZ, 0.2, -0.6, 1.0));
        for up in [
            Vec3::new(1.0, 2.0, -3.0),
            Vec3::splat(f32::MAX),
            Vec3::new(1e-30, -2e-30, 3e-30),
        ] {
            let normalized = up_axis::unit_vector(up).unwrap();
            for hand in [Handedness::Left, Handedness::Right] {
                for scale in [1e-6, 0.001, 1.0, 1000.0, 1e6] {
                    let coordinates = Coordinates::from_up_vector(up, hand, scale).unwrap();
                    assert!(coordinates.direction(normalized).abs_diff_eq(Vec3::Y, 1e-5));
                    assert!(
                        coordinates
                            .source_point(coordinates.point(point))
                            .abs_diff_eq(point, 1e-5)
                    );
                    assert!(
                        coordinates
                            .source_direction(coordinates.direction(angular))
                            .abs_diff_eq(angular, 1e-5)
                    );
                    assert!(
                        coordinates
                            .transform(source)
                            .transform_point(point)
                            .abs_diff_eq(
                                coordinates.point(source.transform_point(point)),
                                1e-4 * scale
                            )
                    );
                    let angular_cross = coordinates
                        .axial_direction(angular)
                        .cross(coordinates.point(point));
                    assert!(
                        angular_cross
                            .abs_diff_eq(coordinates.point(angular.cross(point)), 1e-4 * scale)
                    );
                    let a = coordinates.direction(source.rotation * Vec3::X);
                    let b = coordinates.direction(source.rotation * Vec3::Y);
                    let normal =
                        a.cross(b).normalize() * if coordinates.mirrored { -1.0 } else { 1.0 };
                    assert!(
                        normal.abs_diff_eq(coordinates.direction(source.rotation * Vec3::Z), 1e-5)
                    );
                }
            }
        }
        for invalid in [
            Vec3::ZERO,
            Vec3::splat(f32::INFINITY),
            Vec3::new(f32::NAN, 0.0, 1.0),
        ] {
            assert!(Coordinates::from_up_vector(invalid, Handedness::Right, 1.0).is_none());
        }
    }
}
