use crate::{
    Error,
    debug_draw::{Pose, Primitive},
    error,
};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

pub const MAX_LINES_PER_LIMIT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub instance: u64,
    pub positions: [[f32; 3]; 2],
    pub color: u32,
}

/// Display scale is one captured world unit; cone parameters are tangent-quarter angles.
pub fn tessellate(primitive: &Primitive) -> Result<Vec<Line>, Error> {
    let (instance, active) = match primitive {
        Primitive::LinearLimit {
            instance, active, ..
        }
        | Primitive::AngularLimit {
            instance, active, ..
        }
        | Primitive::LimitCone {
            instance, active, ..
        }
        | Primitive::DoubleCone {
            instance, active, ..
        } => (*instance, *active),
        _ => return Err(error("primitive is not a joint limit")),
    };
    let mut lines = Vec::with_capacity(MAX_LINES_PER_LIMIT);
    let mut emit = |a: [f32; 3], b: [f32; 3]| {
        lines.push(Line {
            instance,
            positions: [a, b],
            color: if active { 0xffff0000 } else { 0xff808080 },
        });
    };
    match primitive {
        Primitive::LinearLimit {
            parent,
            child,
            value,
            ..
        } => {
            validate_pose(parent)?;
            validate_pose(child)?;
            if !value.is_finite() {
                return Err(error("non-finite linear limit"));
            }
            let tip = transform(parent, [*value, 0.0, 0.0]);
            emit(parent.position, tip);
            let axis = sub(tip, parent.position);
            let length = dot(axis, axis).sqrt();
            if !length.is_finite() {
                return Err(error("linear limit arrow exceeds finite coordinate range"));
            }
            if length > 0.0 {
                let normal = mul(axis, 1.0 / length);
                let tangent = if normal[0].abs() < 0.70710677 {
                    [0.0, -normal[2], normal[1]]
                } else {
                    [-normal[1], normal[0], 0.0]
                };
                let tangent = mul(tangent, 1.0 / dot(tangent, tangent).sqrt());
                let side = mul(cross(normal, tangent), length * 0.15 * 0.25 * 3.0f32.sqrt());
                let tangent = mul(tangent, length * 0.15 * 0.25);
                let base = sub(tip, mul(normal, length * 0.15));
                let strip = [
                    tip,
                    add(base, mul(tangent, 2.0)),
                    sub(sub(base, tangent), side),
                    add(sub(base, tangent), side),
                    tip,
                    add(base, mul(tangent, 2.0)),
                ];
                for triangle in strip.windows(3) {
                    for [a, b] in [[0, 1], [1, 2], [2, 0]] {
                        emit(triangle[a], triangle[b]);
                    }
                }
            }
            let half = (FRAC_PI_2 * 0.5).sin();
            let circle_pose = Pose {
                position: tip,
                rotation: quat_mul(parent.rotation, [0.0, half, 0.0, (FRAC_PI_2 * 0.5).cos()]),
            };
            let step = TAU / 20.0;
            let mut angle: f32 = 0.0;
            let mut previous = transform(&circle_pose, [0.0, 0.3, 0.0]);
            for index in 1..=20 {
                angle += step;
                let point = if index == 20 {
                    [0.0, 0.3, 0.0]
                } else {
                    [0.3 * angle.sin(), 0.3 * angle.cos(), 0.0]
                };
                let next = transform(&circle_pose, point);
                emit(previous, next);
                previous = next;
            }
        }
        Primitive::AngularLimit {
            pose, lower, upper, ..
        } => {
            validate_pose(pose)?;
            if !lower.is_finite()
                || !upper.is_finite()
                || lower > upper
                || lower.abs() > TAU
                || upper.abs() > TAU
            {
                return Err(error("angular limit must be ordered within [-2π, 2π]"));
            }
            let at = |angle: f32| transform(pose, [0.0, angle.cos(), angle.sin()]);
            emit(pose.position, at(*lower));
            emit(pose.position, at(*upper));
            let mut angle = *lower;
            let mut previous = at(angle);
            let step = (upper - lower) / 20.0;
            for _ in 0..20 {
                angle += step;
                let next = at(angle);
                emit(previous, next);
                previous = next;
            }
        }
        Primitive::LimitCone {
            pose,
            y_swing,
            z_swing,
            ..
        } => {
            validate_pose(pose)?;
            if ![y_swing, z_swing]
                .into_iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            {
                return Err(error(
                    "cone tangent-quarter angles must be finite within [0, 1]",
                ));
            }
            let mut previous = pose.position;
            for i in 0..=32 {
                let angle = TAU / 32.0 * i as f32;
                let y = -z_swing * angle.sin();
                let z = y_swing * angle.cos();
                let magnitude = y * y + z * z;
                let inverse = 1.0 / (1.0 + magnitude);
                let q = [
                    0.0,
                    2.0 * y * inverse,
                    2.0 * z * inverse,
                    (1.0 - magnitude) * inverse,
                ];
                let next = transform(pose, rotate(q, [1.0, 0.0, 0.0]));
                emit(previous, next);
                emit(pose.position, next);
                previous = next;
            }
        }
        Primitive::DoubleCone { pose, angle, .. } => {
            validate_pose(pose)?;
            if !angle.is_finite() || !(0.0..PI).contains(angle) || angle.cos().abs() < 1e-6 {
                return Err(error(
                    "double-cone angle must be within [0, π), away from the π/2 singularity",
                ));
            }
            let height = angle.tan();
            let point = |side: f32, i: usize| {
                let angle = TAU / 32.0 * i as f32;
                transform(pose, [side * height, angle.cos(), angle.sin()])
            };
            for side in [1.0, -1.0] {
                for i in 0..32 {
                    emit(point(side, i), point(side, i + 1));
                }
            }
            for i in 0..32 {
                emit(pose.position, point(-1.0, i));
                emit(pose.position, point(1.0, i));
            }
        }
        _ => unreachable!(),
    }
    if lines.len() > MAX_LINES_PER_LIMIT
        || lines
            .iter()
            .flat_map(|line| line.positions.iter().flatten())
            .any(|v| !v.is_finite())
    {
        return Err(error(
            "joint limit geometry exceeds its finite output bounds",
        ));
    }
    Ok(lines)
}

fn validate_pose(pose: &Pose) -> Result<(), Error> {
    let norm = pose
        .rotation
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>();
    if !pose
        .position
        .iter()
        .chain(&pose.rotation)
        .all(|v| v.is_finite())
        || (norm - 1.0).abs() > 0.001
    {
        return Err(error(
            "joint limit requires finite position and a unit quaternion",
        ));
    }
    Ok(())
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] + b[i])
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| a[i] - b[i])
}
fn mul(a: [f32; 3], scale: f32) -> [f32; 3] {
    a.map(|v| v * scale)
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn rotate(q: [f32; 4], p: [f32; 3]) -> [f32; 3] {
    let v = [q[0], q[1], q[2]];
    add(
        add(mul(p, 2.0 * q[3] * q[3] - 1.0), mul(v, 2.0 * dot(v, p))),
        mul(cross(v, p), 2.0 * q[3]),
    )
}
fn transform(pose: &Pose, p: [f32; 3]) -> [f32; 3] {
    add(pose.position, rotate(pose.rotation, p))
}
fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let av = [a[0], a[1], a[2]];
    let bv = [b[0], b[1], b[2]];
    let v = add(add(mul(av, b[3]), mul(bv, a[3])), cross(av, bv));
    [v[0], v[1], v[2], a[3] * b[3] - dot(av, bv)]
}
