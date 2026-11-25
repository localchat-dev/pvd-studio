use crate::{
    Value,
    contacts::Contact,
    debug_draw::{Pose, Primitive},
};

// Equality describes captured data, so NaN payloads and signed zero retain their identity.
fn floats_equal<const N: usize>(a: &[f32; N], b: &[f32; N]) -> bool {
    a.map(f32::to_bits) == b.map(f32::to_bits)
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::I64(a), Self::I64(b)) => a == b,
            (Self::U64(a), Self::U64(b)) => a == b,
            (Self::F32(a), Self::F32(b)) => a.to_bits() == b.to_bits(),
            (Self::F64(a), Self::F64(b)) => a.to_bits() == b.to_bits(),
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Vec3(a), Self::Vec3(b)) => floats_equal(a, b),
            (Self::Quat(a), Self::Quat(b)) => floats_equal(a, b),
            (
                Self::Transform {
                    position: a_position,
                    rotation: a_rotation,
                },
                Self::Transform {
                    position: b_position,
                    rotation: b_rotation,
                },
            ) => floats_equal(a_position, b_position) && floats_equal(a_rotation, b_rotation),
            (Self::Ref(a), Self::Ref(b)) => a == b,
            (Self::Array(a), Self::Array(b)) => std::sync::Arc::ptr_eq(a, b) || a == b,
            (Self::Bytes(a), Self::Bytes(b)) => std::sync::Arc::ptr_eq(a, b) || a == b,
            (Self::MeshArray(a), Self::MeshArray(b)) => a == b,
            (Self::Array(a), Self::MeshArray(b)) | (Self::MeshArray(b), Self::Array(a)) => {
                a.len() == b.len() && b.iter().zip(a.iter()).all(|(left, right)| left == *right)
            }
            _ => false,
        }
    }
}

impl PartialEq for Pose {
    fn eq(&self, other: &Self) -> bool {
        floats_equal(&self.position, &other.position)
            && floats_equal(&self.rotation, &other.rotation)
    }
}

impl PartialEq for Contact {
    fn eq(&self, other: &Self) -> bool {
        floats_equal(&self.position, &other.position)
            && floats_equal(&self.normal, &other.normal)
            && self.shapes == other.shapes
            && self.separation.to_bits() == other.separation.to_bits()
            && self.normal_force.to_bits() == other.normal_force.to_bits()
            && self.faces == other.faces
            && self.normal_force_available == other.normal_force_available
    }
}

impl PartialEq for Primitive {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Point {
                    instance: a_instance,
                    position: a_position,
                    color: a_color,
                },
                Self::Point {
                    instance: b_instance,
                    position: b_position,
                    color: b_color,
                },
            ) => {
                a_instance == b_instance
                    && floats_equal(a_position, b_position)
                    && a_color == b_color
            }
            (
                Self::Line {
                    instance: a_instance,
                    positions: a_positions,
                    colors: a_colors,
                },
                Self::Line {
                    instance: b_instance,
                    positions: b_positions,
                    colors: b_colors,
                },
            ) => {
                a_instance == b_instance
                    && a_positions
                        .iter()
                        .zip(b_positions)
                        .all(|(a, b)| floats_equal(a, b))
                    && a_colors == b_colors
            }
            (
                Self::Triangle {
                    instance: a_instance,
                    positions: a_positions,
                    colors: a_colors,
                },
                Self::Triangle {
                    instance: b_instance,
                    positions: b_positions,
                    colors: b_colors,
                },
            ) => {
                a_instance == b_instance
                    && a_positions
                        .iter()
                        .zip(b_positions)
                        .all(|(a, b)| floats_equal(a, b))
                    && a_colors == b_colors
            }
            (
                Self::Text {
                    instance: a_instance,
                    position: a_position,
                    size: a_size,
                    color: a_color,
                    text: a_text,
                },
                Self::Text {
                    instance: b_instance,
                    position: b_position,
                    size: b_size,
                    color: b_color,
                    text: b_text,
                },
            ) => {
                a_instance == b_instance
                    && floats_equal(a_position, b_position)
                    && a_size.to_bits() == b_size.to_bits()
                    && a_color == b_color
                    && a_text == b_text
            }
            (
                Self::JointFrames {
                    instance: a_instance,
                    parent: a_parent,
                    child: a_child,
                },
                Self::JointFrames {
                    instance: b_instance,
                    parent: b_parent,
                    child: b_child,
                },
            ) => a_instance == b_instance && a_parent == b_parent && a_child == b_child,
            (
                Self::LinearLimit {
                    instance: a_instance,
                    parent: a_parent,
                    child: a_child,
                    value: a_value,
                    active: a_active,
                },
                Self::LinearLimit {
                    instance: b_instance,
                    parent: b_parent,
                    child: b_child,
                    value: b_value,
                    active: b_active,
                },
            ) => {
                a_instance == b_instance
                    && a_parent == b_parent
                    && a_child == b_child
                    && a_value.to_bits() == b_value.to_bits()
                    && a_active == b_active
            }
            (
                Self::AngularLimit {
                    instance: a_instance,
                    pose: a_pose,
                    lower: a_lower,
                    upper: a_upper,
                    active: a_active,
                },
                Self::AngularLimit {
                    instance: b_instance,
                    pose: b_pose,
                    lower: b_lower,
                    upper: b_upper,
                    active: b_active,
                },
            ) => {
                a_instance == b_instance
                    && a_pose == b_pose
                    && a_lower.to_bits() == b_lower.to_bits()
                    && a_upper.to_bits() == b_upper.to_bits()
                    && a_active == b_active
            }
            (
                Self::LimitCone {
                    instance: a_instance,
                    pose: a_pose,
                    y_swing: a_y_swing,
                    z_swing: a_z_swing,
                    active: a_active,
                },
                Self::LimitCone {
                    instance: b_instance,
                    pose: b_pose,
                    y_swing: b_y_swing,
                    z_swing: b_z_swing,
                    active: b_active,
                },
            ) => {
                a_instance == b_instance
                    && a_pose == b_pose
                    && a_y_swing.to_bits() == b_y_swing.to_bits()
                    && a_z_swing.to_bits() == b_z_swing.to_bits()
                    && a_active == b_active
            }
            (
                Self::DoubleCone {
                    instance: a_instance,
                    pose: a_pose,
                    angle: a_angle,
                    active: a_active,
                },
                Self::DoubleCone {
                    instance: b_instance,
                    pose: b_pose,
                    angle: b_angle,
                    active: b_active,
                },
            ) => {
                a_instance == b_instance
                    && a_pose == b_pose
                    && a_angle.to_bits() == b_angle.to_bits()
                    && a_active == b_active
            }
            _ => false,
        }
    }
}
