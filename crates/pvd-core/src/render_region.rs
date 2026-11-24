use crate::Geometry;

/// A spherical range in the captured scene's world coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderRegion {
    center: [f32; 3],
    radius: f32,
}

impl RenderRegion {
    pub fn new(center: [f32; 3], radius: f32) -> Option<Self> {
        (center.iter().all(|v| v.is_finite()) && radius.is_finite() && radius > 0.0)
            .then_some(Self { center, radius })
    }

    pub(crate) fn bounds_parameters(self) -> ([f32; 3], f32) {
        (self.center, self.radius)
    }

    pub(crate) fn contains(self, point: [f32; 3]) -> bool {
        let offset = point.map(f64::from);
        let distance_squared = offset
            .into_iter()
            .zip(self.center)
            .map(|(point, center)| (point - f64::from(center)).powi(2))
            .sum::<f64>();
        distance_squared <= f64::from(self.radius).powi(2)
    }

    pub(crate) fn ray_interval(
        self,
        origin: [f64; 3],
        direction: [f64; 3],
        distance: f64,
    ) -> Option<[f64; 2]> {
        let offset = std::array::from_fn(|axis| origin[axis] - f64::from(self.center[axis]));
        let direction_squared = dot(direction, direction);
        let middle = -dot(offset, direction) / direction_squared;
        // The cross product keeps the perpendicular distance independent of the ray's length.
        let rejection = cross(offset, direction);
        let perpendicular_squared = dot(rejection, rejection) / direction_squared;
        let chord_squared = f64::from(self.radius).powi(2) - perpendicular_squared;
        if chord_squared < 0.0 {
            return None;
        }
        let half_span = (chord_squared / direction_squared).sqrt();
        let start = (middle - half_span).max(0.0);
        let end = (middle + half_span).min(distance);
        (start <= end).then_some([start, end])
    }

    pub(crate) fn clip_segment(self, start: [f64; 3], end: [f64; 3]) -> Option<[[f64; 3]; 2]> {
        let direction = std::array::from_fn(|axis| end[axis] - start[axis]);
        if direction == [0.0; 3] {
            return None;
        }
        let interval = self.ray_interval(start, direction, 1.0)?;
        Some(interval.map(|t| std::array::from_fn(|axis| start[axis] + t * direction[axis])))
    }

    pub(crate) fn in_shape_space(
        self,
        position: [f32; 3],
        rotation: [f32; 4],
    ) -> Option<ShapeRegion> {
        if position.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let [x, y, z, w] = unit_rotation(rotation)?;
        let inverse = [-x, -y, -z];
        let offset =
            std::array::from_fn(|axis| f64::from(self.center[axis]) - f64::from(position[axis]));
        let cross_offset = cross(inverse, offset);
        let cross_twice = cross(inverse, cross_offset);
        let center = std::array::from_fn(|axis| {
            offset[axis] + 2.0 * (w * cross_offset[axis] + cross_twice[axis])
        });
        Some(ShapeRegion {
            center,
            radius: f64::from(self.radius),
        })
    }
}

pub(crate) fn normalize_rotation(rotation: [f32; 4]) -> Option<[f32; 4]> {
    unit_rotation(rotation).map(|rotation| rotation.map(|v| v as f32))
}

fn unit_rotation(rotation: [f32; 4]) -> Option<[f64; 4]> {
    let rotation = rotation.map(f64::from);
    let length = rotation.iter().map(|v| v * v).sum::<f64>().sqrt();
    (length.is_finite() && length > 0.0).then(|| rotation.map(|v| v / length))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

#[derive(Clone, Copy)]
pub(crate) struct ShapeRegion {
    center: [f64; 3],
    radius: f64,
}

impl ShapeRegion {
    pub(crate) fn intersects_bounds(self, bounds: LocalBounds) -> bool {
        self.intersects(bounds, self.radius)
    }

    fn intersects(self, bounds: LocalBounds, radius: f64) -> bool {
        let distance_squared = self
            .center
            .iter()
            .enumerate()
            .map(|(axis, &center)| {
                let delta =
                    center - center.clamp(f64::from(bounds.min[axis]), f64::from(bounds.max[axis]));
                delta * delta
            })
            .sum::<f64>();
        distance_squared <= radius * radius * (1.0 + 16.0 * f64::EPSILON)
    }

    pub(crate) fn intersects_geometry(self, geometry: &Geometry) -> bool {
        let nonnegative = |v: f32| v.is_finite() && v >= 0.0;
        match geometry {
            Geometry::Box { half_extents } => {
                half_extents.iter().copied().all(nonnegative)
                    && self.intersects_bounds(LocalBounds::symmetric(*half_extents))
            }
            Geometry::Sphere { radius } => {
                nonnegative(*radius)
                    && self.intersects(
                        LocalBounds::symmetric([0.0; 3]),
                        self.radius + f64::from(*radius),
                    )
            }
            Geometry::Capsule {
                radius,
                half_height,
            } => {
                nonnegative(*radius)
                    && nonnegative(*half_height)
                    && self.intersects(
                        LocalBounds::symmetric([*half_height, 0.0, 0.0]),
                        self.radius + f64::from(*radius),
                    )
            }
            // Unity renders a 10-unit reference square along the local YZ axes.
            Geometry::Plane => self.intersects_bounds(LocalBounds::symmetric([0.0, 5.0, 5.0])),
            Geometry::TriangleMesh { vertices, .. } => {
                LocalBounds::from_points(vertices.iter().copied().map(Some))
                    .is_some_and(|bounds| self.intersects_bounds(bounds))
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct LocalBounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl LocalBounds {
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|axis| self.min[axis].min(other.min[axis])),
            max: std::array::from_fn(|axis| self.max[axis].max(other.max[axis])),
        }
    }

    pub(crate) fn extents(self) -> ([f32; 3], [f32; 3]) {
        (self.min, self.max)
    }

    fn symmetric(half_extents: [f32; 3]) -> Self {
        Self {
            min: half_extents.map(|v| -v),
            max: half_extents,
        }
    }

    pub(crate) fn from_points(points: impl IntoIterator<Item = Option<[f32; 3]>>) -> Option<Self> {
        let mut points = points.into_iter();
        let first = points.next()??;
        if first.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let mut bounds = Self {
            min: first,
            max: first,
        };
        for point in points {
            for (axis, value) in point?.into_iter().enumerate() {
                if !value.is_finite() {
                    return None;
                }
                bounds.min[axis] = bounds.min[axis].min(value);
                bounds.max[axis] = bounds.max[axis].max(value);
            }
        }
        Some(bounds)
    }
}
