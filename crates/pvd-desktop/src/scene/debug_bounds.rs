use super::{display_scale, drawable_debug, valid_position};
use bevy::prelude::*;
use pvd_core::{debug_draw::Primitive, joint_geometry::Line};
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct Bounds {
    min: Vec3,
    max: Vec3,
}

impl Bounds {
    fn merge(&mut self, other: Self) {
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }
    fn corners(self) -> [Vec3; 8] {
        std::array::from_fn(|index| {
            Vec3::new(
                if index & 1 == 0 {
                    self.min.x
                } else {
                    self.max.x
                },
                if index & 2 == 0 {
                    self.min.y
                } else {
                    self.max.y
                },
                if index & 4 == 0 {
                    self.min.z
                } else {
                    self.max.z
                },
            )
        })
    }
}

#[derive(Default)]
pub(super) struct DebugBounds {
    all: Option<Bounds>,
    selected: HashMap<u64, Bounds>,
}

impl DebugBounds {
    pub fn clear(&mut self) {
        self.all = None;
        self.selected.clear();
    }
    pub fn corners(&self, selected: Option<u64>) -> impl Iterator<Item = Vec3> {
        selected
            .map_or(self.all, |id| self.selected.get(&id).copied())
            .into_iter()
            .flat_map(Bounds::corners)
    }
    fn include(&mut self, owner: u64, instance: u64, positions: impl IntoIterator<Item = Vec3>) {
        let mut bounds: Option<Bounds> = None;
        for position in positions {
            if !valid_position(position.to_array()) {
                continue;
            }
            let point = Bounds {
                min: position,
                max: position,
            };
            if let Some(bounds) = &mut bounds {
                bounds.merge(point);
            } else {
                bounds = Some(point);
            }
        }
        let Some(bounds) = bounds else {
            return;
        };
        if let Some(all) = &mut self.all {
            all.merge(bounds);
        } else {
            self.all = Some(bounds);
        }
        self.selected
            .entry(owner)
            .and_modify(|value| value.merge(bounds))
            .or_insert(bounds);
        if instance != 0 && instance != owner {
            self.selected
                .entry(instance)
                .and_modify(|value| value.merge(bounds))
                .or_insert(bounds);
        }
    }
    pub fn lines(&mut self, owner: u64, lines: &[Line]) {
        let Some(first) = lines.first() else {
            return;
        };
        self.include(
            owner,
            first.instance,
            lines.iter().flat_map(|line| line.positions.map(Vec3::from)),
        );
    }
    pub fn primitive(&mut self, owner: u64, primitive: &Primitive, joint_scale: f32) {
        if !drawable_debug(primitive) {
            return;
        }
        match primitive {
            Primitive::Point {
                instance, position, ..
            }
            | Primitive::Text {
                instance, position, ..
            } => {
                // Screen-sized markers and labels are framed by their captured world anchor.
                self.include(owner, *instance, [Vec3::from(*position)]);
            }
            Primitive::Line {
                instance,
                positions,
                ..
            } => self.include(owner, *instance, positions.map(Vec3::from)),
            Primitive::Triangle {
                instance,
                positions,
                ..
            } => {
                self.include(owner, *instance, positions.map(Vec3::from));
            }
            Primitive::JointFrames {
                instance,
                parent,
                child,
            } => {
                let length = 0.5 * display_scale(joint_scale);
                let points = [parent, child].into_iter().flat_map(|pose| {
                    let transform = Transform::from_translation(Vec3::from(pose.position))
                        .with_rotation(Quat::from_array(pose.rotation));
                    [
                        Vec3::ZERO,
                        Vec3::X * length,
                        Vec3::Y * length,
                        Vec3::Z * length,
                    ]
                    .map(|point| transform.transform_point(point))
                });
                self.include(owner, *instance, points);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
