use crate::{Error, Geometry, Object, SceneState, Value, error};
use pvd_protocol::{Endian, Header};
use std::collections::{BTreeMap, BTreeSet};

mod display;
pub use display::collect_display_rays_in_scenes;
pub use display::{DisplayRay, DisplayRays, collect_display_rays};
mod display_shapes;
pub use display_shapes::{DisplayLine, DisplayLineStyle, DisplayShapes, collect_display_shapes};

const MAX_RECORDS: usize = 100_000;
const MAX_MESH_BYTES: usize = 128 * 1024 * 1024;
#[cfg(test)]
const RAYCASTS: &str = "SceneQueries.Raycasts";
#[cfg(test)]
const HITS: &str = "SceneQueries.Hits";

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectReference {
    pub id: u64,
    pub generation: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub actor: ObjectReference,
    pub shape: ObjectReference,
    pub flags: u32,
    pub position: Option<[f32; 3]>,
    pub normal: Option<[f32; 3]>,
    pub distance: f32,
    pub face_index: Option<u32>,
    pub uv: Option<[f32; 2]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Raycast {
    pub scene_id: u64,
    pub index: usize,
    pub batched: bool,
    pub kind: u32,
    pub filter_data: [u32; 4],
    pub filter_flags: u32,
    pub origin: [f32; 3],
    pub direction: [f32; 3],
    pub distance: f32,
    pub hits: Vec<Hit>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueryGeometry {
    pub reference: ObjectReference,
    pub geometry: Geometry,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sweep {
    pub scene_id: u64,
    pub index: usize,
    pub batched: bool,
    pub kind: u32,
    pub filter_data: [u32; 4],
    pub filter_flags: u32,
    pub geometry: QueryGeometry,
    pub direction: [f32; 3],
    pub distance: f32,
    pub hits: Vec<Hit>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Overlap {
    pub scene_id: u64,
    pub index: usize,
    pub batched: bool,
    pub kind: u32,
    pub filter_data: [u32; 4],
    pub filter_flags: u32,
    pub geometry: QueryGeometry,
    pub hits: Vec<Hit>,
}

#[derive(Default, Debug, PartialEq)]
pub struct QueryScene {
    pub raycasts: Vec<Raycast>,
    pub sweeps: Vec<Sweep>,
    pub overlaps: Vec<Overlap>,
    pub diagnostics: Vec<String>,
}

#[derive(Default)]
struct Budget {
    queries: usize,
    hits: usize,
    mesh_bytes: usize,
}

pub fn collect(state: &SceneState) -> QueryScene {
    collect_objects(state, state.objects.values())
}

pub fn collect_from_ids(state: &SceneState, ids: &BTreeSet<u64>) -> QueryScene {
    collect_objects(state, ids.iter().filter_map(|id| state.objects.get(id)))
}

fn collect_objects<'a>(
    state: &SceneState,
    objects: impl Iterator<Item = &'a Object>,
) -> QueryScene {
    let mut result = QueryScene::default();
    let mut total = Budget::default();
    for scene in objects.filter(|o| o.type_name.ends_with("::PxScene")) {
        match collect_scene(state, scene, &total) {
            Ok((mut local, used)) => {
                total.queries += used.queries;
                total.hits += used.hits;
                total.mesh_bytes += used.mesh_bytes;
                result.raycasts.append(&mut local.raycasts);
                result.sweeps.append(&mut local.sweeps);
                result.overlaps.append(&mut local.overlaps);
            }
            Err(problem) => result
                .diagnostics
                .push(format!("Scene 0x{:X}: {problem}", scene.id)),
        }
    }
    result
}

struct Collector<'a> {
    state: &'a SceneState,
    scene: &'a Object,
    prefix: &'static str,
    used: Budget,
    available: Budget,
    meshes: BTreeMap<crate::mesh::MeshKey, Geometry>,
}

fn collect_scene(
    state: &SceneState,
    scene: &Object,
    total: &Budget,
) -> Result<(QueryScene, Budget), Error> {
    let mut result = QueryScene::default();
    let mut c = Collector {
        state,
        scene,
        prefix: "SceneQueries",
        used: Budget::default(),
        available: Budget {
            queries: MAX_RECORDS - total.queries,
            hits: MAX_RECORDS - total.hits,
            mesh_bytes: MAX_MESH_BYTES - total.mesh_bytes,
        },
        meshes: BTreeMap::new(),
    };
    for (prefix, batched) in [("SceneQueries", false), ("BatchedQueries", true)] {
        c.prefix = prefix;
        for suffix in ["Raycasts", "Sweeps", "Overlaps"] {
            let Some(value) = scene.properties.get(&format!("{prefix}.{suffix}")) else {
                continue;
            };
            let records = array(value)?;
            if records.len() > c.available.queries.saturating_sub(c.used.queries) {
                return Err(error(
                    "Scene-query display exceeds 100,000 rays / sweep / overlap records",
                ));
            }
            c.used.queries += records.len();
            for (index, value) in records.iter().enumerate() {
                match suffix {
                    "Raycasts" => {
                        let ray = raycast_record(value)?;
                        let hits = c.hits(ray.hits, Some(ray.distance), false)?;
                        result.raycasts.push(Raycast {
                            scene_id: scene.id,
                            index,
                            batched,
                            kind: ray.kind,
                            filter_data: ray.filter_data,
                            filter_flags: ray.filter_flags,
                            origin: ray.origin,
                            direction: ray.direction,
                            distance: ray.distance,
                            hits,
                        });
                    }
                    "Sweeps" => {
                        let query = c.sweep_record(value)?;
                        let (direction, distance) = query.motion.expect("sweep motion");
                        let geometry = c.geometry(query.geometry, &query.pose)?;
                        validate_kind(query.kind, &geometry.geometry, true)?;
                        let hits = c.hits(query.hits, Some(distance), true)?;
                        result.sweeps.push(Sweep {
                            scene_id: scene.id,
                            index,
                            batched,
                            kind: query.kind,
                            filter_data: query.filter_data,
                            filter_flags: query.filter_flags,
                            geometry,
                            direction,
                            distance,
                            hits,
                        });
                    }
                    _ => {
                        let query = overlap_record(value)?;
                        let geometry = c.geometry(query.geometry, &query.pose)?;
                        validate_kind(query.kind, &geometry.geometry, false)?;
                        let hits = c.hits(query.hits, None, false)?;
                        result.overlaps.push(Overlap {
                            scene_id: scene.id,
                            index,
                            batched,
                            kind: query.kind,
                            filter_data: query.filter_data,
                            filter_flags: query.filter_flags,
                            geometry,
                            hits,
                        });
                    }
                }
            }
        }
    }
    Ok((result, c.used))
}

impl<'a> Collector<'a> {
    fn sweep_record<'r>(&self, value: &'r Value) -> Result<ShapeQueryRecord<'r>, Error> {
        let fields = record_fields(value, 16)?;
        let kind = uint(&fields[0])?;
        if !(8..=10).contains(&kind) {
            return Err(error(format!("Unsupported sweep kind {kind}")));
        }
        Ok(ShapeQueryRecord {
            kind,
            filter_data: filter_data(singleton(self.range(&fields[10..13], "FilterDataList")?)?)?,
            filter_flags: uint(&fields[1])?,
            motion: Some((unit_direction(&fields[2])?, positive_distance(&fields[3])?)),
            pose: singleton(self.range(&fields[7..10], "PoseList")?)?.clone(),
            geometry: &fields[4..7],
            hits: &fields[13..16],
        })
    }

    fn range(&self, fields: &[Value], suffix: &str) -> Result<&[Value], Error> {
        let property = format!("{}.{}", self.prefix, suffix);
        query_range(self.scene, fields, &property, suffix)
    }

    fn geometry_object(&self, fields: &[Value]) -> Result<&'a Object, Error> {
        let Value::Ref(id) = singleton(self.range(fields, "GeometryList")?)? else {
            return Err(error("Scene-query geometry requires an object reference"));
        };
        let property = format!("{}.GeometryList", self.prefix);
        self.state
            .resolve_reference(self.scene.id, &property, *id)
            .ok_or_else(|| {
                error("Scene-query geometry reference is absent or belongs to a different lifetime")
            })
    }

    fn geometry(&mut self, fields: &[Value], pose: &Value) -> Result<QueryGeometry, Error> {
        let (position, rotation) = checked_transform(pose)?;
        let object = self.geometry_object(fields)?;
        self.geometry_at(object, position, rotation)
    }

    fn geometry_at(
        &mut self,
        object: &Object,
        position: [f32; 3],
        rotation: [f32; 4],
    ) -> Result<QueryGeometry, Error> {
        let geometry = if object.type_name.ends_with("::PxBoxGeometry") {
            let half_extents = vector(required(object, "HalfExtents")?)?;
            if half_extents.iter().any(|v| *v <= 0.0) {
                return Err(error("Query box half extents must be positive"));
            }
            Geometry::Box { half_extents }
        } else if object.type_name.ends_with("::PxSphereGeometry") {
            Geometry::Sphere {
                radius: positive_scalar(required(object, "Radius")?)?,
            }
        } else if object.type_name.ends_with("::PxCapsuleGeometry") {
            let half_height = positive_distance(required(object, "HalfHeight")?)?;
            Geometry::Capsule {
                radius: positive_scalar(required(object, "Radius")?)?,
                half_height,
            }
        } else if object.type_name.ends_with("::PxConvexMeshGeometry") {
            let key = self
                .state
                .mesh_key(object)
                .ok_or_else(|| error("Query convex mesh has an invalid resource reference"))?;
            if let Some(cached) = self.meshes.get(&key) {
                cached.clone()
            } else {
                let decoded = self.state.mesh_geometry(object).ok_or_else(|| {
                    error("Query convex mesh is malformed or exceeds mesh limits")
                })?;
                let Geometry::TriangleMesh { vertices, indices } = &decoded else {
                    unreachable!()
                };
                if vertices.iter().flatten().any(|v| !v.is_finite()) {
                    return Err(error("Query convex mesh contains non-finite vertices"));
                }
                let size = vertices.len() * 12 + indices.len() * 4;
                if size
                    > self
                        .available
                        .mesh_bytes
                        .saturating_sub(self.used.mesh_bytes)
                {
                    return Err(error(
                        "Scene-query geometry exceeds the 128 MiB unique mesh allocation limit",
                    ));
                }
                self.used.mesh_bytes += size;
                self.meshes.insert(key, decoded.clone());
                decoded
            }
        } else {
            return Err(error(format!(
                "Unsupported query geometry {}",
                object.type_name
            )));
        };
        Ok(QueryGeometry {
            reference: ObjectReference {
                id: object.id,
                generation: Some(object.generation),
            },
            geometry,
            position,
            rotation,
        })
    }

    fn hits(
        &mut self,
        fields: &[Value],
        distance: Option<f32>,
        sweep: bool,
    ) -> Result<Vec<Hit>, Error> {
        let count = uint(&fields[2])? as usize;
        if count > self.available.hits.saturating_sub(self.used.hits) {
            return Err(error("Scene-query display exceeds 100,000 referenced hits"));
        }
        let selected = self.range(fields, "Hits")?;
        let property = format!("{}.Hits", self.prefix);
        let mut result = Vec::with_capacity(count);
        for value in selected {
            let (fields, flags, hit_distance) = hit_fields(value, distance, sweep)?;
            let reference = |value: &Value| -> Result<ObjectReference, Error> {
                let Value::Ref(id) = value else {
                    return Err(error("Scene query hit requires typed object references"));
                };
                Ok(ObjectReference {
                    id: *id,
                    generation: self
                        .state
                        .resolve_reference(self.scene.id, &property, *id)
                        .map(|o| o.generation),
                })
            };
            result.push(Hit {
                shape: reference(&fields[0])?,
                actor: reference(&fields[1])?,
                flags,
                position: if flags & 1 != 0 {
                    Some(vector(&fields[4])?)
                } else {
                    None
                },
                normal: if flags & 2 != 0 {
                    Some(vector(&fields[5])?)
                } else {
                    None
                },
                distance: hit_distance,
                face_index: if flags & (1 << 10) != 0
                    || (distance.is_none() && uint(&fields[2])? != u32::MAX)
                {
                    Some(uint(&fields[2])?)
                } else {
                    None
                },
                uv: if flags & (1 << 3) != 0 {
                    Some([real(&fields[7])?, real(&fields[8])?])
                } else {
                    None
                },
            });
        }
        self.used.hits += count;
        Ok(result)
    }
}

struct ShapeQueryRecord<'a> {
    kind: u32,
    filter_data: [u32; 4],
    filter_flags: u32,
    motion: Option<([f32; 3], f32)>,
    pose: Value,
    geometry: &'a [Value],
    hits: &'a [Value],
}

fn overlap_record(value: &Value) -> Result<ShapeQueryRecord<'_>, Error> {
    let fields = record_fields(value, 10)?;
    let kind = uint(&fields[0])?;
    if !(3..=7).contains(&kind) {
        return Err(error(format!("Unsupported overlap kind {kind}")));
    }
    Ok(ShapeQueryRecord {
        kind,
        filter_data: filter_data(&fields[1])?,
        filter_flags: uint(&fields[2])?,
        motion: None,
        pose: fields[3].clone(),
        geometry: &fields[4..7],
        hits: &fields[7..10],
    })
}

struct RaycastRecord<'a> {
    kind: u32,
    filter_data: [u32; 4],
    filter_flags: u32,
    origin: [f32; 3],
    direction: [f32; 3],
    distance: f32,
    hits: &'a [Value],
}

fn raycast_record(value: &Value) -> Result<RaycastRecord<'_>, Error> {
    let fields = record_fields(value, 9)?;
    let kind = uint(&fields[0])?;
    if kind > 2 {
        return Err(error(format!("Unsupported raycast kind {kind}")));
    }
    Ok(RaycastRecord {
        kind,
        filter_data: filter_data(&fields[1])?,
        filter_flags: uint(&fields[2])?,
        origin: vector(&fields[3])?,
        direction: unit_direction(&fields[4])?,
        distance: positive_distance(&fields[5])?,
        hits: &fields[6..9],
    })
}

fn query_range<'a>(
    scene: &'a Object,
    fields: &[Value],
    property: &str,
    suffix: &str,
) -> Result<&'a [Value], Error> {
    if !matches!(&fields[0], Value::String(name) if name == property) {
        return Err(error(format!(
            "Scene query references an unsupported {suffix} array"
        )));
    }
    let base = uint(&fields[1])? as usize;
    let count = uint(&fields[2])? as usize;
    let end = base
        .checked_add(count)
        .ok_or_else(|| error("Scene-query array range overflow"))?;
    let values = scene
        .properties
        .get(property)
        .map(array)
        .transpose()?
        .unwrap_or(&[]);
    values.get(base..end).ok_or_else(|| {
        error(format!(
            "Scene-query {suffix} range exceeds the captured array"
        ))
    })
}

fn hit_fields(
    value: &Value,
    distance: Option<f32>,
    sweep: bool,
) -> Result<(&[Value], u32, f32), Error> {
    let fields = record_fields(value, 9)?;
    let flags = uint(&fields[3])?;
    let hit_distance = real(&fields[6])?;
    if distance
        .is_some_and(|maximum| hit_distance > maximum + 0.001 || (!sweep && hit_distance < 0.0))
    {
        return Err(error(
            "Scene-query hit distance lies outside the query segment",
        ));
    }
    Ok((fields, flags, hit_distance))
}

fn validate_kind(kind: u32, geometry: &Geometry, sweep: bool) -> Result<(), Error> {
    let valid = if sweep {
        matches!(
            (kind, geometry),
            (8, Geometry::Box { .. })
                | (9, Geometry::Sphere { .. } | Geometry::Capsule { .. })
                | (10, Geometry::TriangleMesh { .. })
        )
    } else {
        matches!(
            (kind, geometry),
            (3, Geometry::Sphere { .. })
                | (4 | 5, Geometry::Box { .. })
                | (6, Geometry::Capsule { .. })
                | (7, Geometry::TriangleMesh { .. })
        )
    };
    if valid {
        Ok(())
    } else {
        Err(error("Scene-query kind disagrees with its geometry"))
    }
}
fn required<'a>(object: &'a Object, property: &str) -> Result<&'a Value, Error> {
    object
        .properties
        .get(property)
        .ok_or_else(|| error(format!("Query geometry has no {property}")))
}
fn singleton(values: &[Value]) -> Result<&Value, Error> {
    if let [value] = values {
        Ok(value)
    } else {
        Err(error(
            "Scene query requires exactly one geometry, pose and filter record",
        ))
    }
}
fn array(value: &Value) -> Result<&[Value], Error> {
    match value {
        Value::Array(values) => Ok(values),
        _ => Err(error(
            "Scene query property has no supported structured decoder",
        )),
    }
}
fn record_fields(value: &Value, count: usize) -> Result<&[Value], Error> {
    let values = array(value)?;
    if values.len() == count {
        Ok(values)
    } else {
        Err(error("Scene query record has an invalid field count"))
    }
}
fn uint(value: &Value) -> Result<u32, Error> {
    match value {
        Value::U64(value) => {
            u32::try_from(*value).map_err(|_| error("Scene query integer exceeds u32"))
        }
        _ => Err(error("Scene query requires an unsigned integer")),
    }
}
fn real(value: &Value) -> Result<f32, Error> {
    match value {
        Value::F32(value) if value.is_finite() => Ok(*value),
        _ => Err(error("Scene query requires a finite scalar")),
    }
}
fn vector(value: &Value) -> Result<[f32; 3], Error> {
    match value {
        Value::Vec3(value) if value.iter().all(|v| v.is_finite()) => Ok(*value),
        _ => Err(error("Scene query requires a finite vector")),
    }
}
fn filter_data(value: &Value) -> Result<[u32; 4], Error> {
    let values = record_fields(value, 4)?;
    Ok([
        uint(&values[0])?,
        uint(&values[1])?,
        uint(&values[2])?,
        uint(&values[3])?,
    ])
}
fn unit_direction(value: &Value) -> Result<[f32; 3], Error> {
    let value = vector(value)?;
    if (value.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() > 0.001 {
        return Err(error("Scene query requires a unit direction"));
    }
    Ok(value)
}
fn positive_distance(value: &Value) -> Result<f32, Error> {
    let value = real(value)?;
    if value < 0.0 {
        Err(error("Scene query requires a non-negative distance"))
    } else {
        Ok(value)
    }
}
fn positive_scalar(value: &Value) -> Result<f32, Error> {
    let value = real(value)?;
    if value <= 0.0 {
        Err(error("Scene query requires a positive dimension"))
    } else {
        Ok(value)
    }
}
fn checked_transform(value: &Value) -> Result<([f32; 3], [f32; 4]), Error> {
    let Value::Transform { position, rotation } = value else {
        return Err(error("Scene query requires a transform"));
    };
    if position.iter().chain(rotation).any(|v| !v.is_finite())
        || (rotation.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() > 0.001
    {
        return Err(error(
            "Scene-query transform must be finite with a unit quaternion",
        ));
    }
    Ok((*position, *rotation))
}

pub(crate) fn decode_property(
    typename: &str,
    data: &[u8],
    items: u32,
    header: &Header,
) -> Result<Option<Value>, Error> {
    let pointer = if header.flags & 1 != 0 { 8 } else { 4 };
    let reference_size = pointer + 8;
    let (stride, references) = match typename {
        "physx3::PvdRaycast" => (
            align(52, pointer) + reference_size,
            vec![align(52, pointer)],
        ),
        "physx3::PvdSweep" => (
            24 + reference_size * 4,
            (0..4).map(|i| 24 + i * reference_size).collect(),
        ),
        "physx3::PvdOverlap" => (
            align(52, pointer) + reference_size * 2,
            (0..2)
                .map(|i| align(52, pointer) + i * reference_size)
                .collect(),
        ),
        "physx3::PvdSqHit" => (align(2 * pointer + 44, pointer), vec![]),
        _ => return Ok(None),
    };
    let count = items as usize;
    if count > MAX_RECORDS {
        return Err(error("Scene-query property exceeds 100,000 records"));
    }
    let minimum = count
        .checked_mul(stride + references.len())
        .ok_or_else(|| error("Scene-query byte count overflow"))?;
    if data.len() < minimum || (references.is_empty() && data.len() != minimum) {
        return Err(error(
            "Scene-query payload length disagrees with the producer layout",
        ));
    }
    let mut offset = 0;
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let bytes = data
            .get(offset..offset + stride)
            .ok_or_else(|| error("Truncated scene-query record"))?;
        let read =
            |start: usize, size: usize| read_uint(&bytes[start..start + size], header.endian);
        let uint = |start| Value::U64(read(start, 4));
        let real = |start| Value::F32(f32::from_bits(read(start, 4) as u32));
        let vector = |start| {
            Value::Vec3(std::array::from_fn(|i| {
                f32::from_bits(read(start + i * 4, 4) as u32)
            }))
        };
        let filter = |start: usize| {
            Value::Array(
                (0..4)
                    .map(|i| uint(start + i * 4))
                    .collect::<Vec<_>>()
                    .into(),
            )
        };
        offset += stride;
        let mut fields = match typename {
            "physx3::PvdRaycast" => vec![
                uint(0),
                filter(4),
                uint(20),
                vector(24),
                vector(36),
                real(48),
            ],
            "physx3::PvdSweep" => vec![uint(0), uint(4), vector(8), real(20)],
            "physx3::PvdOverlap" => vec![
                uint(0),
                filter(4),
                uint(20),
                Value::Transform {
                    rotation: std::array::from_fn(|i| f32::from_bits(read(24 + i * 4, 4) as u32)),
                    position: std::array::from_fn(|i| f32::from_bits(read(40 + i * 4, 4) as u32)),
                },
            ],
            _ => {
                let tail = 2 * pointer;
                vec![
                    Value::Ref(read(0, pointer)),
                    Value::Ref(read(pointer, pointer)),
                    uint(tail),
                    uint(tail + 4),
                    vector(tail + 8),
                    vector(tail + 20),
                    real(tail + 32),
                    real(tail + 36),
                    real(tail + 40),
                ]
            }
        };
        // The SDK appends strings after each native record in property order; pointer bytes are opaque.
        for reference in &references {
            let tail = data
                .get(offset..)
                .ok_or_else(|| error("Missing scene-query array name"))?;
            let length = tail
                .iter()
                .take(257)
                .position(|byte| *byte == 0)
                .ok_or_else(|| error("Unterminated or oversized scene-query array name"))?;
            let name = std::str::from_utf8(&tail[..length])
                .map_err(|_| error("Scene-query array name is not UTF-8"))?;
            offset += length + 1;
            fields.extend([
                Value::String(name.into()),
                uint(reference + pointer),
                uint(reference + pointer + 4),
            ]);
        }
        result.push(Value::Array(fields.into()));
    }
    if offset != data.len() {
        return Err(error("Scene-query payload has trailing bytes"));
    }
    Ok(Some(Value::Array(result.into())))
}

fn align(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}
fn read_uint(bytes: &[u8], endian: Endian) -> u64 {
    let mut storage = [0u8; 8];
    match endian {
        Endian::Little => {
            storage[..bytes.len()].copy_from_slice(bytes);
            u64::from_le_bytes(storage)
        }
        Endian::Big => {
            storage[8 - bytes.len()..].copy_from_slice(bytes);
            u64::from_be_bytes(storage)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn header(pointer: usize, endian: Endian) -> Header {
        Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: u32::from(pointer == 8),
            endian,
        }
    }

    fn write(bytes: &mut [u8], start: usize, value: u64, width: usize, endian: Endian) {
        let encoded = match endian {
            Endian::Little => value.to_le_bytes(),
            Endian::Big => value.to_be_bytes(),
        };
        let encoded = match endian {
            Endian::Little => &encoded[..width],
            Endian::Big => &encoded[8 - width..],
        };
        bytes[start..start + width].copy_from_slice(encoded);
    }

    fn ray(pointer: usize, endian: Endian) -> Vec<u8> {
        let size = if pointer == 8 { 72 } else { 64 };
        let base = if pointer == 8 { 64 } else { 56 };
        let mut bytes = vec![0; size];
        write(&mut bytes, 0, 1, 4, endian);
        write(&mut bytes, 4, 123, 4, endian);
        write(&mut bytes, 20, 3, 4, endian);
        write(&mut bytes, 28, u64::from(10.0f32.to_bits()), 4, endian);
        write(&mut bytes, 40, u64::from((-1.0f32).to_bits()), 4, endian);
        write(&mut bytes, 48, u64::from(20.0f32.to_bits()), 4, endian);
        write(&mut bytes, base, 0, 4, endian);
        write(&mut bytes, base + 4, 1, 4, endian);
        bytes.extend_from_slice(b"SceneQueries.Hits\0");
        bytes
    }

    fn hit(pointer: usize, endian: Endian) -> Vec<u8> {
        let mut bytes = vec![0; if pointer == 8 { 64 } else { 52 }];
        write(&mut bytes, 0, 0x12345678, pointer, endian);
        write(&mut bytes, pointer, 0x87654321, pointer, endian);
        write(&mut bytes, pointer * 2, u64::from(u32::MAX), 4, endian);
        write(&mut bytes, pointer * 2 + 4, 3, 4, endian);
        write(
            &mut bytes,
            pointer * 2 + 24,
            u64::from(1.0f32.to_bits()),
            4,
            endian,
        );
        write(
            &mut bytes,
            pointer * 2 + 32,
            u64::from(10.0f32.to_bits()),
            4,
            endian,
        );
        bytes
    }

    #[test]
    fn producer_width_and_endian_control_records_and_interleaved_strings() {
        for pointer in [4, 8] {
            for endian in [Endian::Little, Endian::Big] {
                let header = header(pointer, endian);
                let mut bytes = ray(pointer, endian);
                bytes.extend(ray(pointer, endian));
                let decoded = decode_property("physx3::PvdRaycast", &bytes, 2, &header)
                    .unwrap()
                    .unwrap();
                let rays = array(&decoded).unwrap();
                assert_eq!(rays.len(), 2);
                for ray in rays {
                    let fields = record_fields(ray, 9).unwrap();
                    assert_eq!(fields[0], Value::U64(1));
                    assert_eq!(fields[3], Value::Vec3([0.0, 10.0, 0.0]));
                    assert_eq!(fields[4], Value::Vec3([0.0, -1.0, 0.0]));
                    assert_eq!(fields[5], Value::F32(20.0));
                    assert_eq!(fields[6], Value::String(HITS.into()));
                }
                let decoded =
                    decode_property("physx3::PvdSqHit", &hit(pointer, endian), 1, &header)
                        .unwrap()
                        .unwrap();
                let fields = record_fields(&array(&decoded).unwrap()[0], 9).unwrap();
                assert_eq!(fields[0], Value::Ref(0x12345678));
                assert_eq!(fields[1], Value::Ref(0x87654321));
                assert_eq!(fields[5], Value::Vec3([0.0, 1.0, 0.0]));
                assert_eq!(fields[6], Value::F32(10.0));
            }
        }
    }

    #[test]
    fn truncated_names_lengths_and_record_budgets_are_rejected() {
        let header = header(8, Endian::Little);
        let bytes = ray(8, Endian::Little);
        for length in 0..bytes.len() {
            assert!(decode_property("physx3::PvdRaycast", &bytes[..length], 1, &header).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_property("physx3::PvdRaycast", &trailing, 1, &header).is_err());
        assert!(decode_property("physx3::PvdRaycast", &bytes, 2, &header).is_err());
        assert!(decode_property("physx3::PvdRaycast", &bytes, 100_001, &header).is_err());
        let mut invalid_utf8 = bytes.clone();
        invalid_utf8[72] = 0xff;
        assert!(decode_property("physx3::PvdRaycast", &invalid_utf8, 1, &header).is_err());
        let mut oversized = vec![0; 72];
        oversized.extend(vec![b'a'; 257]);
        oversized.push(0);
        assert!(decode_property("physx3::PvdRaycast", &oversized, 1, &header).is_err());
        let hit = hit(8, Endian::Little);
        assert!(decode_property("physx3::PvdSqHit", &hit[..63], 1, &header).is_err());
        assert!(
            decode_property("other::PvdRaycast", &bytes, 1, &header)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn malformed_query_ranges_and_nonfinite_values_have_visible_diagnostics() {
        let header = header(8, Endian::Little);
        let mut scene = Object {
            id: 1,
            generation: 0,
            type_name: "physx3::PxScene".into(),
            properties: BTreeMap::from([
                (
                    RAYCASTS.into(),
                    decode_property("physx3::PvdRaycast", &ray(8, Endian::Little), 1, &header)
                        .unwrap()
                        .unwrap(),
                ),
                (
                    HITS.into(),
                    decode_property("physx3::PvdSqHit", &hit(8, Endian::Little), 1, &header)
                        .unwrap()
                        .unwrap(),
                ),
            ]),
        };
        let mut state = SceneState::default();
        state.objects.insert(1, scene.clone());
        assert!(collect(&state).diagnostics.is_empty());
        let original = scene.properties[RAYCASTS].clone();
        for (index, value) in [
            (7, Value::U64(u32::MAX.into())),
            (3, Value::Vec3([f32::NAN, 0.0, 0.0])),
            (4, Value::Vec3([0.0; 3])),
            (5, Value::F32(f32::INFINITY)),
            (6, Value::String("Unrelated.Hits".into())),
        ] {
            let mut records = array(&original).unwrap().to_vec();
            let mut fields = array(&records[0]).unwrap().to_vec();
            fields[index] = value;
            records[0] = Value::Array(fields.into());
            scene
                .properties
                .insert(RAYCASTS.into(), Value::Array(records.into()));
            state.objects.insert(1, scene.clone());
            let result = collect(&state);
            assert!(result.raycasts.is_empty());
            assert_eq!(result.diagnostics.len(), 1);
        }
    }

    fn query_record(hit_count: usize) -> Value {
        let header = header(8, Endian::Little);
        let decoded = decode_property("physx3::PvdRaycast", &ray(8, Endian::Little), 1, &header)
            .unwrap()
            .unwrap();
        let mut fields = array(&array(&decoded).unwrap()[0]).unwrap().to_vec();
        fields[8] = Value::U64(hit_count as u64);
        Value::Array(fields.into())
    }

    fn hit_record() -> Value {
        let decoded = decode_property(
            "physx3::PvdSqHit",
            &hit(8, Endian::Little),
            1,
            &header(8, Endian::Little),
        )
        .unwrap()
        .unwrap();
        array(&decoded).unwrap()[0].clone()
    }

    fn scene(id: u64, rays: Vec<Value>, hits: Vec<Value>) -> Object {
        Object {
            id,
            generation: 0,
            type_name: "physx3::PxScene".into(),
            properties: BTreeMap::from([
                (RAYCASTS.into(), Value::Array(rays.into())),
                (HITS.into(), Value::Array(hits.into())),
            ]),
        }
    }

    #[test]
    fn display_rays_clip_the_recorded_segment_and_use_the_nearest_usable_hit() {
        let mut fields = array(&query_record(4)).unwrap().to_vec();
        fields[3] = Value::Vec3([-20_000.0, 0.0, 0.0]);
        fields[4] = Value::Vec3([1.0, 0.0, 0.0]);
        fields[5] = Value::F32(40_000.0);
        let hits = [
            (20_002.0, [2.0, 0.0, 0.0], 1),
            (19_999.0, [-1.0, 0.0, 0.0], 0),
            (20_000.0, [f32::NAN, 0.0, 0.0], 1),
            (20_001.0, [1.0, 0.0, 0.0], 1),
        ]
        .map(|(distance, position, flags)| {
            let mut hit = array(&hit_record()).unwrap().to_vec();
            hit[3] = Value::U64(flags);
            hit[4] = Value::Vec3(position);
            hit[6] = Value::F32(distance);
            Value::Array(hit.into())
        });
        let mut state = SceneState::default();
        state.objects.insert(
            1,
            scene(1, vec![Value::Array(fields.into())], hits.to_vec()),
        );

        let region = crate::RenderRegion::new([0.0; 3], 3.0);
        let local = collect_display_rays(&state, None, region, 1, 10_000.0);
        assert_eq!(local.rays.len(), 1);
        let ray = &local.rays[0];
        assert_eq!(ray.start, [-3.0, 0.0, 0.0]);
        assert_eq!(ray.end, [3.0, 0.0, 0.0]);
        assert_eq!(ray.marker, Some([1.0, 0.0, 0.0]));
        assert!(ray.has_hit && !local.truncated);

        let region = crate::RenderRegion::new([0.0; 3], 0.5);
        let local = collect_display_rays(&state, None, region, 1, 10_000.0);
        assert!(local.rays[0].has_hit);
        assert_eq!(local.rays[0].marker, None);

        let region = crate::RenderRegion::new([2.0, 0.0, 0.0], 0.25);
        let local = collect_display_rays(&state, None, region, 1, 10_000.0);
        assert_eq!(local.rays[0].marker, Some([2.0, 0.0, 0.0]));

        let global = collect_display_rays(&state, None, None, 1, 10_000.0);
        assert_eq!(global.rays[0].end, [-10_000.0, 0.0, 0.0]);
        assert!(global.rays[0].has_hit);
        assert_eq!(global.rays[0].marker, None);
    }

    #[test]
    fn display_rays_use_the_scene_generation_and_limit_only_visible_rays() {
        let mut ordinary = array(&query_record(0)).unwrap().to_vec();
        ordinary[3] = Value::Vec3([0.0, 20.0, 0.0]);
        ordinary[4] = Value::Vec3([1.0, 0.0, 0.0]);
        let mut batched = array(&query_record(0)).unwrap().to_vec();
        batched[3] = Value::Vec3([0.0; 3]);
        batched[6] = Value::String("BatchedQueries.Hits".into());
        let batched = Value::Array(batched.into());
        let mut state = SceneState::default();
        let mut scene = scene(1, vec![Value::Array(ordinary.into())], vec![]);
        scene.generation = 5;
        scene.properties.insert(
            "BatchedQueries.Raycasts".into(),
            Value::Array(vec![Value::F32(f32::NAN), batched.clone(), batched].into()),
        );
        scene
            .properties
            .insert("SceneQueries.Sweeps".into(), Value::U64(1));
        state.objects.insert(1, scene);

        let region = crate::RenderRegion::new([0.0; 3], 3.0);
        let current = crate::frame_sources::FrameObject {
            id: 1,
            generation: 5,
        };
        let visible = collect_display_rays(&state, Some(current), region, 2, 10_000.0);
        assert_eq!(visible.rays.len(), 2);
        assert!(!visible.truncated);
        let limited = collect_display_rays(&state, Some(current), region, 1, 10_000.0);
        assert_eq!(limited.rays.len(), 1);
        assert!(limited.truncated);

        let obsolete = crate::frame_sources::FrameObject {
            id: 1,
            generation: 4,
        };
        let other = collect_display_rays(&state, Some(obsolete), region, 1, 10_000.0);
        assert!(other.rays.is_empty() && !other.truncated);
    }

    #[test]
    fn invalid_scene_does_not_consume_other_scenes_hit_budget() {
        let valid_hit = hit_record();
        let mut invalid_fields = array(&valid_hit).unwrap().to_vec();
        invalid_fields[4] = Value::Vec3([f32::NAN, 0.0, 0.0]);
        let mut hits = vec![valid_hit.clone(); MAX_RECORDS];
        hits[0] = Value::Array(invalid_fields.into());
        let mut state = SceneState::default();
        state
            .objects
            .insert(1, scene(1, vec![query_record(MAX_RECORDS)], hits));
        state
            .objects
            .insert(2, scene(2, vec![query_record(1)], vec![valid_hit]));

        let result = collect(&state);
        assert_eq!(
            result,
            collect_from_ids(&state, &state.objects.keys().copied().collect())
        );
        assert_eq!(result.diagnostics.len(), 1);
        assert!(result.diagnostics[0].contains("Scene 0x1"));
        assert!(result.diagnostics[0].contains("finite vector"));
        assert_eq!(result.raycasts.len(), 1);
        assert_eq!(result.raycasts[0].scene_id, 2);
        assert_eq!(result.raycasts[0].hits.len(), 1);
    }

    #[test]
    fn discarded_overflow_scene_preserves_remaining_ray_and_hit_capacity() {
        let valid_hit = hit_record();
        let mut state = SceneState::default();
        state
            .objects
            .insert(1, scene(1, vec![query_record(0); MAX_RECORDS - 1], vec![]));
        state.objects.insert(
            2,
            scene(2, vec![query_record(1); 2], vec![valid_hit.clone()]),
        );
        state.objects.insert(
            3,
            scene(
                3,
                vec![query_record(MAX_RECORDS)],
                vec![valid_hit; MAX_RECORDS],
            ),
        );

        let result = collect(&state);
        assert_eq!(
            result,
            collect_from_ids(&state, &state.objects.keys().copied().collect())
        );
        assert_eq!(result.diagnostics.len(), 1);
        assert!(result.diagnostics[0].contains("Scene 0x2"));
        assert!(result.diagnostics[0].contains("100,000 rays"));
        assert_eq!(result.raycasts.len(), MAX_RECORDS);
        assert!(result.raycasts.iter().all(|ray| ray.scene_id != 2));
        let last = result.raycasts.last().unwrap();
        assert_eq!(last.scene_id, 3);
        assert_eq!(last.hits.len(), MAX_RECORDS);
    }

    fn shape_query_bytes(pointer: usize, endian: Endian, sweep: bool) -> Vec<u8> {
        let reference_start = if sweep { 24 } else { align(52, pointer) };
        let names: &[&str] = if sweep {
            &[
                "SceneQueries.GeometryList",
                "SceneQueries.PoseList",
                "SceneQueries.FilterDataList",
                "SceneQueries.Hits",
            ]
        } else {
            &["SceneQueries.GeometryList", "SceneQueries.Hits"]
        };
        let mut bytes = vec![0; reference_start + (pointer + 8) * names.len()];
        write(&mut bytes, 0, if sweep { 8 } else { 5 }, 4, endian);
        write(&mut bytes, if sweep { 4 } else { 20 }, 3, 4, endian);
        if sweep {
            write(&mut bytes, 12, u64::from((-1.0f32).to_bits()), 4, endian);
            write(&mut bytes, 20, u64::from(10.0f32.to_bits()), 4, endian);
        } else {
            write(&mut bytes, 4, 17, 4, endian);
            write(&mut bytes, 36, u64::from(1.0f32.to_bits()), 4, endian);
            write(&mut bytes, 44, u64::from(5.0f32.to_bits()), 4, endian);
        }
        for (index, name) in names.iter().enumerate() {
            let base = reference_start + (pointer + 8) * index;
            write(&mut bytes, base + pointer, index as u64, 4, endian);
            write(&mut bytes, base + pointer + 4, 1, 4, endian);
            bytes.extend_from_slice(name.as_bytes());
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn sweep_overlap_layouts_validate_all_pointer_widths_endians_and_strings() {
        for pointer in [4, 8] {
            for endian in [Endian::Little, Endian::Big] {
                for sweep in [false, true] {
                    let typename = if sweep {
                        "physx3::PvdSweep"
                    } else {
                        "physx3::PvdOverlap"
                    };
                    let bytes = shape_query_bytes(pointer, endian, sweep);
                    let header = header(pointer, endian);
                    let mut two_records = bytes.clone();
                    two_records.extend_from_slice(&bytes);
                    let decoded = decode_property(typename, &two_records, 2, &header)
                        .unwrap()
                        .unwrap();
                    for record in array(&decoded).unwrap() {
                        let fields = record_fields(record, if sweep { 16 } else { 10 }).unwrap();
                        assert_eq!(fields[4], Value::String("SceneQueries.GeometryList".into()));
                        assert_eq!(fields[5], Value::U64(0));
                        assert_eq!(fields[6], Value::U64(1));
                        if sweep {
                            assert_eq!(fields[2], Value::Vec3([0.0, -1.0, 0.0]));
                            assert_eq!(fields[3], Value::F32(10.0));
                            assert_eq!(
                                fields[10],
                                Value::String("SceneQueries.FilterDataList".into())
                            );
                            assert_eq!(fields[13], Value::String("SceneQueries.Hits".into()));
                        } else {
                            assert_eq!(
                                fields[3],
                                Value::Transform {
                                    position: [0.0, 5.0, 0.0],
                                    rotation: [0.0, 0.0, 0.0, 1.0]
                                }
                            );
                            assert_eq!(fields[7], Value::String("SceneQueries.Hits".into()));
                        }
                    }
                    for length in 0..bytes.len() {
                        assert!(decode_property(typename, &bytes[..length], 1, &header).is_err());
                    }
                    let mut trailing = bytes.clone();
                    trailing.push(0);
                    assert!(decode_property(typename, &trailing, 1, &header).is_err());
                    assert!(decode_property(typename, &bytes, 100_001, &header).is_err());
                }
            }
        }
    }

    fn shape_scene() -> SceneState {
        let header = header(8, Endian::Little);
        let decoded = decode_property(
            "physx3::PvdOverlap",
            &shape_query_bytes(8, Endian::Little, false),
            1,
            &header,
        )
        .unwrap()
        .unwrap();
        let mut fields = array(&array(&decoded).unwrap()[0]).unwrap().to_vec();
        fields[8] = Value::U64(0);
        fields[9] = Value::U64(0);
        let mut scene = scene(1, vec![], vec![]);
        scene.properties.insert(
            "SceneQueries.Overlaps".into(),
            Value::Array(vec![Value::Array(fields.into())].into()),
        );
        scene.properties.insert(
            "SceneQueries.GeometryList".into(),
            Value::Array(vec![Value::Ref(2)].into()),
        );
        let geometry = Object {
            id: 2,
            generation: 4,
            type_name: "physx3::PxBoxGeometry".into(),
            properties: BTreeMap::from([("HalfExtents".into(), Value::Vec3([1.0; 3]))]),
        };
        let mut state = SceneState::default();
        state.objects.insert(1, scene);
        state.objects.insert(2, geometry);
        state.reference_lifetimes.insert(
            (1, "SceneQueries.GeometryList".into()),
            std::sync::Arc::new(BTreeMap::from([(2, 4)])),
        );
        state
    }

    #[test]
    fn shape_display_retains_valid_records_after_malformed_records_and_limits_lines() {
        let mut state = shape_scene();
        let scene = state.objects.get_mut(&1).unwrap();
        let records = scene.properties.get_mut("SceneQueries.Overlaps").unwrap();
        let valid = array(records).unwrap()[0].clone();
        *records = Value::Array(vec![Value::U64(0), valid].into());
        let collect =
            |limit| collect_display_shapes(&state, state.objects.values(), None, limit, 10_000.0);
        let all = collect(12);
        assert_eq!(all.lines.len(), 12);
        assert!(!all.truncated);
        assert!(
            all.lines
                .iter()
                .all(|line| line.style == DisplayLineStyle::Overlap)
        );
        assert!(all.lines.iter().any(|line| line.end == [1.0, 6.0, 1.0]));
        let limited = collect(1);
        assert_eq!(limited.lines.len(), 1);
        assert!(limited.truncated);
    }

    #[test]
    fn batched_capsule_sweep_clips_crossing_path_and_rotates_physx_x_axis() {
        let mut state = shape_scene();
        let source = state.objects.get_mut(&1).unwrap();
        let properties = &mut source.properties;
        properties.remove("SceneQueries.Overlaps");
        let transform = Value::Transform {
            position: [-20_000.0, 0.0, 0.0],
            rotation: [
                0.0,
                0.0,
                std::f32::consts::FRAC_1_SQRT_2,
                std::f32::consts::FRAC_1_SQRT_2,
            ],
        };
        let decoded = decode_property(
            "physx3::PvdSweep",
            &shape_query_bytes(8, Endian::Little, true),
            1,
            &header(8, Endian::Little),
        )
        .unwrap()
        .unwrap();
        let mut fields = array(&array(&decoded).unwrap()[0]).unwrap().to_vec();
        fields[0] = Value::U64(9);
        fields[2] = Value::Vec3([1.0, 0.0, 0.0]);
        fields[3] = Value::F32(40_000.0);
        for (base, suffix) in [
            (4, "GeometryList"),
            (7, "PoseList"),
            (10, "FilterDataList"),
            (13, "Hits"),
        ] {
            fields[base] = Value::String(format!("BatchedQueries.{suffix}"));
            fields[base + 1] = Value::U64(0);
        }
        properties.insert(
            "BatchedQueries.GeometryList".into(),
            Value::Array(vec![Value::Ref(2)].into()),
        );
        properties.insert(
            "BatchedQueries.PoseList".into(),
            Value::Array(vec![transform].into()),
        );
        properties.insert(
            "BatchedQueries.FilterDataList".into(),
            Value::Array(vec![Value::Array(vec![Value::U64(0); 4].into())].into()),
        );
        properties.insert(
            "BatchedQueries.Sweeps".into(),
            Value::Array(vec![Value::Array(fields.into())].into()),
        );
        let mut hit = array(&hit_record()).unwrap().to_vec();
        hit[3] = Value::U64(0);
        hit[4] = Value::Vec3([0.0, 20.0, 0.0]);
        hit[6] = Value::F32(20_000.0);
        properties.insert(
            "BatchedQueries.Hits".into(),
            Value::Array(vec![Value::Array(hit.into())].into()),
        );
        let capsule = state.objects.get_mut(&2).unwrap();
        capsule.type_name = "physx3::PxCapsuleGeometry".into();
        capsule.properties = BTreeMap::from([
            ("Radius".into(), Value::F32(0.5)),
            ("HalfHeight".into(), Value::F32(2.0)),
        ]);
        state.reference_lifetimes.insert(
            (1, "BatchedQueries.GeometryList".into()),
            std::sync::Arc::new(BTreeMap::from([(2, 4)])),
        );
        let display =
            |region| collect_display_shapes(&state, state.objects.values(), region, 1000, 10_000.0);
        let local = display(crate::RenderRegion::new([0.0; 3], 3.0));
        assert!(!local.truncated);
        assert!(
            local
                .lines
                .iter()
                .any(|line| line.style == DisplayLineStyle::Sweep
                    && line.start == [-3.0, 0.0, 0.0]
                    && line.end == [3.0, 0.0, 0.0])
        );
        assert!(
            local
                .lines
                .iter()
                .filter(|line| line.style == DisplayLineStyle::Hit)
                .any(|line| line.end[1] > 2.49 && line.end[0].abs() < 0.51)
        );
        assert!(
            local
                .lines
                .iter()
                .flat_map(|line| [line.start, line.end])
                .all(|point| point.into_iter().map(|v| v * v).sum::<f32>() < 9.001)
        );
        let global = display(None);
        assert!(
            global
                .lines
                .iter()
                .any(|line| line.style == DisplayLineStyle::Sweep
                    && line.start == [-20_000.0, 0.0, 0.0]
                    && line.end == [-10_000.0, 0.0, 0.0])
        );
        assert!(
            global
                .lines
                .iter()
                .all(|line| line.style != DisplayLineStyle::Hit)
        );
    }

    #[test]
    fn shape_queries_reject_stale_geometry_lifetimes_and_invalid_transforms() {
        let mut state = shape_scene();
        assert_eq!(collect(&state).overlaps.len(), 1);
        state.objects.get_mut(&2).unwrap().generation = 5;
        let invalid = collect(&state);
        assert!(invalid.overlaps.is_empty());
        assert!(invalid.diagnostics[0].contains("different lifetime"));
        state.objects.get_mut(&2).unwrap().generation = 4;
        let properties = &mut state.objects.get_mut(&1).unwrap().properties;
        let value = properties.get("SceneQueries.Overlaps").unwrap();
        let mut fields = array(&array(value).unwrap()[0]).unwrap().to_vec();
        fields[3] = Value::Transform {
            position: [0.0; 3],
            rotation: [0.0; 4],
        };
        properties.insert(
            "SceneQueries.Overlaps".into(),
            Value::Array(vec![Value::Array(fields.into())].into()),
        );
        assert!(collect(&state).diagnostics[0].contains("unit quaternion"));
    }

    #[test]
    fn ordinary_and_batched_shapes_share_query_budgets_without_losing_other_scenes() {
        let mut state = shape_scene();
        let source = state.objects.get_mut(&1).unwrap();
        source.properties.insert(
            RAYCASTS.into(),
            Value::Array(vec![query_record(0); MAX_RECORDS - 1].into()),
        );
        let mut batched_fields =
            array(&array(&source.properties["SceneQueries.Overlaps"]).unwrap()[0])
                .unwrap()
                .to_vec();
        batched_fields[4] = Value::String("BatchedQueries.GeometryList".into());
        batched_fields[7] = Value::String("BatchedQueries.Hits".into());
        source.properties.insert(
            "BatchedQueries.Overlaps".into(),
            Value::Array(vec![Value::Array(batched_fields.into())].into()),
        );
        state
            .objects
            .insert(3, scene(3, vec![query_record(0)], vec![]));
        let result = collect(&state);
        assert_eq!(
            result,
            collect_from_ids(&state, &state.objects.keys().copied().collect())
        );
        assert!(result.overlaps.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.raycasts.len(), 1);
        assert_eq!(result.raycasts[0].scene_id, 3);
    }
}
