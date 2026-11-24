use super::{
    Budget, Collector, MAX_RECORDS, QueryGeometry, array, checked_transform,
    display::display_position, hit_fields, overlap_record, validate_kind, vector,
};
use crate::{Geometry, Object, RenderRegion, SceneState, Value, refs, rotate};
use std::{
    collections::{BTreeMap, BTreeSet},
    f32::consts::TAU,
    ops::ControlFlow,
    sync::LazyLock,
};

type Point = [f64; 3];

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayLineStyle {
    Sweep = 0,
    Overlap = 1,
    Hit = 2,
}

pub struct DisplayLine {
    pub style: DisplayLineStyle,
    pub start: [f32; 3],
    pub end: [f32; 3],
}

#[derive(Default)]
pub struct DisplayShapes {
    pub lines: Vec<DisplayLine>,
    pub truncated: bool,
}

const MAX_DISPLAY_MESH_BYTES: usize = 4 * 1024 * 1024;
const MAX_DISPLAY_VERTICES: usize = 1024;
const MAX_DISPLAY_TRIANGLES: usize = 4096;
const MAX_TESTED_LINES: usize = 200_000;
const STEPS: usize = 12;
static CIRCLE: LazyLock<[[f32; 2]; STEPS]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let (sin, cos) = (i as f32 * TAU / STEPS as f32).sin_cos();
        [cos, sin]
    })
});

/// Streams sweep and overlap wireframes from the scene objects in the restored frame.
/// Limits only the full-view travel. Regional views clip each emitted line to the sphere.
pub fn collect_display_shapes<'a>(
    state: &'a SceneState,
    scenes: impl Iterator<Item = &'a Object>,
    region: Option<RenderRegion>,
    limit: usize,
    global_distance_limit: f32,
) -> DisplayShapes {
    let mut sink = LineSink {
        result: DisplayShapes::default(),
        region,
        limit,
        tested: 0,
    };
    let mut records_left = MAX_RECORDS;
    let mut hits_left = MAX_RECORDS;
    let mut mesh_bytes_left = MAX_DISPLAY_MESH_BYTES;
    for scene in scenes.filter(|scene| scene.type_name == "physx3::PxScene") {
        let mut parser = Collector {
            state,
            scene,
            prefix: "SceneQueries",
            used: Budget::default(),
            available: Budget {
                mesh_bytes: mesh_bytes_left,
                ..Budget::default()
            },
            meshes: BTreeMap::new(),
        };
        let mut invalid_geometry = BTreeSet::new();
        let mut convex_radii = BTreeMap::new();
        for (prefix, sweeps, overlaps) in [
            (
                "SceneQueries",
                "SceneQueries.Sweeps",
                "SceneQueries.Overlaps",
            ),
            (
                "BatchedQueries",
                "BatchedQueries.Sweeps",
                "BatchedQueries.Overlaps",
            ),
        ] {
            parser.prefix = prefix;
            for (property, sweep) in [(sweeps, true), (overlaps, false)] {
                let Some(records) = scene
                    .properties
                    .get(property)
                    .and_then(|value| array(value).ok())
                else {
                    continue;
                };
                for record in records {
                    if records_left == 0 {
                        sink.result.truncated = true;
                        return sink.result;
                    }
                    records_left -= 1;
                    let Ok(query) = (if sweep {
                        parser.sweep_record(record)
                    } else {
                        overlap_record(record)
                    }) else {
                        continue;
                    };
                    let Ok(object) = parser.geometry_object(query.geometry) else {
                        continue;
                    };
                    let Ok((position, rotation)) = checked_transform(&query.pose) else {
                        continue;
                    };
                    if invalid_geometry.contains(&object.id) || !display_position(position) {
                        continue;
                    }
                    let convex = object.type_name.ends_with("::PxConvexMeshGeometry");
                    if convex && !convex_radii.contains_key(&object.id) {
                        let Some(key) = state.mesh_key(object) else {
                            invalid_geometry.insert(object.id);
                            continue;
                        };
                        if !parser.meshes.contains_key(&key) {
                            let Some(bytes) = convex_bytes(state, object) else {
                                invalid_geometry.insert(object.id);
                                continue;
                            };
                            if bytes > mesh_bytes_left - parser.used.mesh_bytes {
                                invalid_geometry.insert(object.id);
                                sink.result.truncated = true;
                                continue;
                            }
                        }
                    }
                    let Ok(geometry) = parser.geometry_at(object, position, rotation) else {
                        invalid_geometry.insert(object.id);
                        continue;
                    };
                    if validate_kind(query.kind, &geometry.geometry, sweep).is_err() {
                        continue;
                    }
                    let radius = if convex {
                        *convex_radii
                            .entry(object.id)
                            .or_insert_with(|| bounding_radius(&geometry.geometry))
                    } else {
                        bounding_radius(&geometry.geometry)
                    };
                    if !display_position([radius as f32; 3]) {
                        continue;
                    }
                    let motion = Motion::new(query.motion, region, global_distance_limit);
                    if region.is_some_and(|region| !intersects(region, position, motion, radius)) {
                        continue;
                    }
                    let hits = parser.range(query.hits, "Hits").unwrap_or(&[]);
                    let hits = if hits.len() <= hits_left {
                        hits_left -= hits.len();
                        hits
                    } else {
                        sink.result.truncated = true;
                        &[]
                    };
                    let maximum = query.motion.map(|(_, distance)| distance);
                    let hits = HitDisplay::from_hits(hits, maximum, motion.travel, region);
                    if emit_shape(&mut sink, geometry, motion, radius, sweep, hits).is_break() {
                        return sink.result;
                    }
                }
            }
        }
        mesh_bytes_left -= parser.used.mesh_bytes;
    }
    sink.result
}

#[derive(Clone, Copy)]
struct Motion {
    direction: Point,
    travel: f64,
}

impl Motion {
    fn new(
        motion: Option<([f32; 3], f32)>,
        region: Option<RenderRegion>,
        global_distance_limit: f32,
    ) -> Self {
        let (direction, distance) = motion.unwrap_or(([0.0; 3], 0.0));
        Self {
            direction: direction.map(f64::from),
            travel: f64::from(if region.is_some() {
                distance
            } else {
                distance.min(global_distance_limit)
            }),
        }
    }

    fn at(self, origin: Point, distance: f64) -> Point {
        std::array::from_fn(|axis| origin[axis] + self.direction[axis] * distance)
    }
}

#[derive(Default)]
struct HitDisplay {
    distance: Option<f32>,
    marker: Option<(f32, [f32; 3])>,
    overlap: bool,
}

impl HitDisplay {
    fn from_hits(
        hits: &[Value],
        maximum: Option<f32>,
        travel: f64,
        region: Option<RenderRegion>,
    ) -> Self {
        let mut display = Self::default();
        for hit in hits {
            let Ok((fields, flags, distance)) = hit_fields(hit, maximum, maximum.is_some()) else {
                continue;
            };
            if !matches!((&fields[0], &fields[1]), (Value::Ref(_), Value::Ref(_))) {
                continue;
            }
            if maximum.is_none() {
                display.overlap = true;
                break;
            }
            if f64::from(distance) > travel {
                continue;
            }
            if distance >= 0.0 && display.distance.is_none_or(|previous| distance < previous) {
                display.distance = Some(distance);
            }
            if flags & 1 != 0
                && display
                    .marker
                    .is_none_or(|(previous, _)| distance < previous)
                && let Ok(position) = vector(&fields[4])
                && display_position(position)
                && region.is_none_or(|region| region.contains(position))
            {
                display.marker = Some((distance, position));
            }
        }
        display
    }
}

fn emit_shape(
    sink: &mut LineSink,
    query: QueryGeometry,
    motion: Motion,
    radius: f64,
    sweep: bool,
    hits: HitDisplay,
) -> ControlFlow<()> {
    let origin = query.position.map(f64::from);
    let rotation =
        crate::render_region::normalize_rotation(query.rotation).expect("validated query rotation");
    let draw = |sink: &mut LineSink, center: Point, style| {
        outline(&query.geometry, |a, b| {
            let transform = |p| translate(rotate(rotation, p), center);
            sink.push(style, transform(a), transform(b))
        })
    };
    let style = match (sweep, hits.overlap) {
        (true, _) => DisplayLineStyle::Sweep,
        (_, true) => DisplayLineStyle::Hit,
        _ => DisplayLineStyle::Overlap,
    };
    if motion.travel > 0.0 {
        let end = motion.at(origin, motion.travel);
        sink.push(style, origin, end)?;
        for point in path_anchors(&query.geometry) {
            let point = rotate(rotation, point);
            sink.push(style, translate(point, origin), translate(point, end))?;
        }
        draw(sink, end, style)?;
    }
    draw(sink, origin, style)?;
    if let Some(distance) = hits.distance {
        draw(
            sink,
            motion.at(origin, f64::from(distance)),
            DisplayLineStyle::Hit,
        )?;
    }
    if let Some((_, position)) = hits.marker {
        let center = position.map(f64::from);
        let half_length = (radius * 0.15).clamp(0.08, 0.4);
        for axis in 0..3 {
            let (mut start, mut end) = (center, center);
            start[axis] -= half_length;
            end[axis] += half_length;
            sink.push(DisplayLineStyle::Hit, start, end)?;
        }
    }
    ControlFlow::Continue(())
}

struct LineSink {
    result: DisplayShapes,
    region: Option<RenderRegion>,
    limit: usize,
    tested: usize,
}

impl LineSink {
    fn push(&mut self, style: DisplayLineStyle, start: Point, end: Point) -> ControlFlow<()> {
        if self.tested == MAX_TESTED_LINES {
            self.result.truncated = true;
            return ControlFlow::Break(());
        }
        self.tested += 1;
        let [start, end] = if let Some(region) = self.region {
            let Some(clipped) = region.clip_segment(start, end) else {
                return ControlFlow::Continue(());
            };
            clipped
        } else {
            [start, end]
        }
        .map(|p| p.map(|v| v as f32));
        if start == end || !display_position(start) || !display_position(end) {
            return ControlFlow::Continue(());
        }
        if self.result.lines.len() == self.limit {
            self.result.truncated = true;
            return ControlFlow::Break(());
        }
        self.result.lines.push(DisplayLine { style, start, end });
        ControlFlow::Continue(())
    }
}

fn convex_bytes(state: &SceneState, geometry: &Object) -> Option<usize> {
    let key = "ConvexMesh";
    let id = refs(geometry.properties.get(key)).next()?;
    let mesh = state.resolve_reference(geometry.id, key, id)?;
    let points = array(mesh.properties.get("Points")?).ok()?;
    let polygons = array(mesh.properties.get("HullPolygons")?).ok()?;
    if points.len() > MAX_DISPLAY_VERTICES || polygons.len() > MAX_DISPLAY_TRIANGLES {
        return Some(usize::MAX);
    }
    let mut triangles = 0;
    for polygon in polygons {
        let points = super::uint(array(polygon).ok()?.first()?).ok()? as usize;
        triangles += points.checked_sub(2)?;
        if triangles > MAX_DISPLAY_TRIANGLES {
            return Some(usize::MAX);
        }
    }
    Some(points.len() * 12 + triangles * 12)
}

fn bounding_radius(geometry: &Geometry) -> f64 {
    let length = |point: [f32; 3]| {
        point
            .into_iter()
            .map(|v| f64::from(v).powi(2))
            .sum::<f64>()
            .sqrt()
    };
    match geometry {
        Geometry::Box { half_extents } => length(*half_extents),
        Geometry::Sphere { radius } => f64::from(*radius),
        Geometry::Capsule {
            radius,
            half_height,
        } => f64::from(*radius) + f64::from(*half_height),
        Geometry::TriangleMesh { vertices, .. } => {
            vertices.iter().copied().map(length).fold(0.0, f64::max)
        }
        Geometry::Plane => 0.0,
    }
}

fn intersects(region: RenderRegion, origin: [f32; 3], motion: Motion, radius: f64) -> bool {
    let (center, region_radius) = region.bounds_parameters();
    let expanded = RenderRegion::new(center, (f64::from(region_radius) + radius) as f32);
    expanded.is_none_or(|expanded| {
        if motion.travel == 0.0 {
            expanded.contains(origin)
        } else {
            expanded
                .ray_interval(origin.map(f64::from), motion.direction, motion.travel)
                .is_some()
        }
    })
}

fn translate(point: [f32; 3], offset: Point) -> Point {
    std::array::from_fn(|axis| f64::from(point[axis]) + offset[axis])
}

fn path_anchors(geometry: &Geometry) -> [[f32; 3]; 4] {
    match geometry {
        Geometry::Box {
            half_extents: [x, y, z],
        } => [[*x, *y, *z], [-x, -y, *z], [-x, *y, -z], [*x, -y, -z]],
        Geometry::Sphere { radius } | Geometry::Capsule { radius, .. } => [
            [0.0, *radius, 0.0],
            [0.0, -radius, 0.0],
            [0.0, 0.0, *radius],
            [0.0, 0.0, -radius],
        ],
        Geometry::TriangleMesh { vertices, .. } if !vertices.is_empty() => [
            0,
            vertices.len() / 4,
            vertices.len() / 2,
            vertices.len() * 3 / 4,
        ]
        .map(|i| vertices[i]),
        Geometry::TriangleMesh { .. } | Geometry::Plane => [[0.0; 3]; 4],
    }
}

fn outline(
    geometry: &Geometry,
    mut line: impl FnMut([f32; 3], [f32; 3]) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let ring = |i: usize| CIRCLE[i % STEPS];
    match geometry {
        Geometry::Box { half_extents } => {
            let corner = |index| {
                std::array::from_fn(|axis| {
                    if index & (1 << axis) == 0 {
                        -half_extents[axis]
                    } else {
                        half_extents[axis]
                    }
                })
            };
            for index in 0..8 {
                for axis in 0..3 {
                    if index & (1 << axis) == 0 {
                        line(corner(index), corner(index | (1 << axis)))?;
                    }
                }
            }
        }
        Geometry::Capsule {
            radius,
            half_height,
        } if *half_height > 0.0 => {
            let tube = |x, i| {
                let [cos, sin] = ring(i);
                [x, radius * cos, radius * sin]
            };
            for i in 0..STEPS {
                line(tube(-half_height, i), tube(-half_height, i + 1))?;
                line(tube(*half_height, i), tube(*half_height, i + 1))?;
                if i % (STEPS / 4) == 0 {
                    line(tube(-half_height, i), tube(*half_height, i))?;
                }
            }
            // PhysX capsule lengths run along local X.
            for axis in [1, 2] {
                for sign in [-1.0, 1.0] {
                    let cap = |i| {
                        let [cos, sin] = ring(i);
                        let mut p = [sign * (half_height + radius * sin), 0.0, 0.0];
                        p[axis] = radius * cos;
                        p
                    };
                    for i in 0..STEPS / 2 {
                        line(cap(i), cap(i + 1))?;
                    }
                }
            }
        }
        Geometry::Sphere { radius } | Geometry::Capsule { radius, .. } => {
            for axis in 0..3 {
                let point = |i| {
                    let [cos, sin] = ring(i);
                    let mut p = [0.0; 3];
                    p[(axis + 1) % 3] = radius * cos;
                    p[(axis + 2) % 3] = radius * sin;
                    p
                };
                for i in 0..STEPS {
                    line(point(i), point(i + 1))?;
                }
            }
        }
        Geometry::TriangleMesh { vertices, indices } => {
            for triangle in indices.chunks_exact(3) {
                for i in 0..3 {
                    line(
                        vertices[triangle[i] as usize],
                        vertices[triangle[(i + 1) % 3] as usize],
                    )?;
                }
            }
        }
        Geometry::Plane => {}
    }
    ControlFlow::Continue(())
}
