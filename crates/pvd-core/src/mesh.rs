use crate::{Geometry, Object, SceneState, Value, refs, rotate, scalar, vec3};
use std::collections::BTreeMap;

const MAX_VERTICES: usize = 1_000_000;
const MAX_INDICES: usize = 3_000_000;

struct CachedMesh {
    source: Object,
    geometry: Option<Geometry>,
    bytes: usize,
}

/// Derived geometry is shared across displayed frames while captured mesh data stays equal.
pub struct MeshCache {
    entries: BTreeMap<MeshKey, CachedMesh>,
    bytes: usize,
    limit: usize,
}

impl Default for MeshCache {
    fn default() -> Self {
        Self::with_byte_limit(128 * 1024 * 1024)
    }
}

impl MeshCache {
    pub fn with_byte_limit(limit: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            bytes: 0,
            limit,
        }
    }

    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub(crate) fn retain_resources(&mut self, state: &SceneState) {
        self.entries.retain(|key, entry| {
            let keep = state
                .objects
                .get(&key.resource)
                .is_some_and(|source| *source == entry.source);
            if !keep {
                self.bytes -= entry.bytes;
            }
            keep
        });
    }

    pub(crate) fn decode(&mut self, state: &SceneState, geometry: &Object) -> Option<Geometry> {
        let key = state.mesh_key(geometry)?;
        let source = state.objects.get(&key.resource)?;
        if let Some(entry) = self.entries.get(&key)
            && *source == entry.source
        {
            return entry.geometry.clone();
        }
        if let Some(entry) = self.entries.remove(&key) {
            self.bytes -= entry.bytes;
        }
        let decoded = state.mesh_geometry(geometry);
        let geometry_bytes = match &decoded {
            Some(Geometry::TriangleMesh { vertices, indices }) => {
                vertices.capacity() * std::mem::size_of::<[f32; 3]>()
                    + indices.capacity() * std::mem::size_of::<u32>()
            }
            _ => 0,
        };
        let bytes = std::mem::size_of::<CachedMesh>()
            + std::mem::size_of::<MeshKey>()
            + source.type_name.capacity()
            + source
                .properties
                .iter()
                .map(|(name, value)| {
                    name.capacity() + std::mem::size_of::<Value>() + value.retained_bytes()
                })
                .sum::<usize>()
            + geometry_bytes;
        if bytes <= self.limit {
            if bytes > self.limit.saturating_sub(self.bytes) {
                self.clear();
            }
            self.bytes += bytes;
            self.entries.insert(
                key,
                CachedMesh {
                    source: source.clone(),
                    geometry: decoded.clone(),
                    bytes,
                },
            );
        }
        decoded
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MeshKey {
    resource: u64,
    generation: u32,
    kind: MeshKind,
    scale: [u32; 3],
    rotation: [u32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MeshKind {
    Convex,
    Triangle,
    HeightField,
}

impl MeshKind {
    fn for_geometry(geometry: &Object) -> Option<Self> {
        if geometry.type_name.ends_with("::PxConvexMeshGeometry") {
            Some(Self::Convex)
        } else if geometry.type_name.ends_with("::PxTriangleMeshGeometry") {
            Some(Self::Triangle)
        } else if geometry.type_name.ends_with("::PxHeightFieldGeometry") {
            Some(Self::HeightField)
        } else {
            None
        }
    }

    fn property(self) -> &'static str {
        match self {
            Self::Convex => "ConvexMesh",
            Self::Triangle => "TriangleMesh",
            Self::HeightField => "HeightField",
        }
    }
}

fn height_field_scale(geometry: &Object) -> Option<[f32; 3]> {
    let scale = [
        scalar(geometry.properties.get("RowScale"))?,
        scalar(geometry.properties.get("HeightScale"))?,
        scalar(geometry.properties.get("ColumnScale"))?,
    ];
    scale
        .iter()
        .all(|v| v.is_finite() && *v > 0.0)
        .then_some(scale)
}

fn height_field_geometry(mesh: &Object, geometry: &Object) -> Option<Geometry> {
    if integer(mesh.properties.get("Format")?)? != 1 {
        return None;
    }
    let rows = integer(mesh.properties.get("NbRows")?)? as usize;
    let columns = integer(mesh.properties.get("NbColumns")?)? as usize;
    let count = rows.checked_mul(columns)?;
    let index_count = rows
        .checked_sub(1)?
        .checked_mul(columns.checked_sub(1)?)?
        .checked_mul(6)?;
    if rows < 2 || columns < 2 || count > MAX_VERTICES || index_count > MAX_INDICES {
        return None;
    }
    let scale = height_field_scale(geometry)?;
    let thickness = scalar(mesh.properties.get("Thickness"))?;
    if !thickness.is_finite() {
        return None;
    }
    let Value::Array(samples) = mesh.properties.get("Samples")? else {
        return None;
    };
    if samples.len() != count {
        return None;
    }
    let mut vertices = Vec::with_capacity(count);
    let mut indices = Vec::with_capacity(index_count);
    for (index, sample) in samples.iter().enumerate() {
        let Value::Array(fields) = sample else {
            return None;
        };
        if fields.len() != 3 {
            return None;
        }
        let Value::I64(height) = fields[0] else {
            return None;
        };
        let height = i16::try_from(height).ok()?;
        let material0 = u8::try_from(integer(&fields[1])?).ok()?;
        let material1 = u8::try_from(integer(&fields[2])?).ok()?;
        let row = index / columns;
        let column = index % columns;
        let vertex = [
            row as f32 * scale[0],
            f32::from(height) * scale[1],
            column as f32 * scale[2],
        ];
        if !vertex.iter().all(|v| v.is_finite()) {
            return None;
        }
        vertices.push(vertex);
        if row == rows - 1 || column == columns - 1 {
            continue;
        }
        let cell = index as u32;
        let stride = columns as u32;
        let triangles = if material0 & 0x80 != 0 {
            [
                [cell + stride, cell, cell + stride + 1],
                [cell + 1, cell + stride + 1, cell],
            ]
        } else {
            [
                [cell, cell + 1, cell + stride],
                [cell + stride + 1, cell + stride, cell + 1],
            ]
        };
        for (mut triangle, material) in triangles.into_iter().zip([material0, material1]) {
            if material & 0x7f == 127 {
                continue;
            }
            if thickness > 0.0 {
                triangle.swap(1, 2);
            }
            indices.extend(triangle);
        }
    }
    indices.shrink_to_fit();
    Some(Geometry::TriangleMesh {
        vertices: vertices.into(),
        indices: indices.into(),
    })
}

impl SceneState {
    pub(crate) fn mesh_key(&self, geometry: &Object) -> Option<MeshKey> {
        let kind = MeshKind::for_geometry(geometry)?;
        let property = kind.property();
        let id = *refs(geometry.properties.get(property)).first()?;
        let resource = self.resolve_reference(geometry.id, property, id)?;
        let (scale, rotation) = if kind == MeshKind::HeightField {
            (height_field_scale(geometry)?, [0.0, 0.0, 0.0, 1.0])
        } else {
            let scale = vec3(geometry.properties.get("Scale.Scale"))?;
            let Some(Value::Quat(rotation)) = geometry.properties.get("Scale.Rotation") else {
                return None;
            };
            (scale, *rotation)
        };
        Some(MeshKey {
            resource: resource.id,
            generation: resource.generation,
            kind,
            scale: scale.map(f32::to_bits),
            rotation: rotation.map(f32::to_bits),
        })
    }
    pub(crate) fn mesh_geometry(&self, geometry: &Object) -> Option<Geometry> {
        let kind = MeshKind::for_geometry(geometry)?;
        let property = kind.property();
        let id = *refs(geometry.properties.get(property)).first()?;
        let mesh = self.resolve_reference(geometry.id, property, id)?;
        if kind == MeshKind::HeightField {
            return height_field_geometry(mesh, geometry);
        }
        let Value::Array(points) = mesh.properties.get("Points")? else {
            return None;
        };
        if points.len() > MAX_VERTICES {
            return None;
        }
        let mut vertices = points
            .iter()
            .map(|v| vec3(Some(v)))
            .collect::<Option<Vec<_>>>()?;
        let mut indices = Vec::new();
        if kind == MeshKind::Convex {
            let Value::Array(polygons) = mesh.properties.get("HullPolygons")? else {
                return None;
            };
            let Value::Array(polygon_indices) = mesh.properties.get("PolygonIndexes")? else {
                return None;
            };
            let mut index_count = 0usize;
            for polygon in polygons.iter() {
                let Value::Array(fields) = polygon else {
                    return None;
                };
                let count = integer(fields.first()?)? as usize;
                let base = integer(fields.get(1)?)? as usize;
                polygon_indices.get(base..base.checked_add(count)?)?;
                index_count = index_count.checked_add(count.checked_sub(2)?.checked_mul(3)?)?;
                if index_count > MAX_INDICES {
                    return None;
                }
            }
            indices.reserve(index_count);
            for polygon in polygons.iter() {
                let Value::Array(fields) = polygon else {
                    return None;
                };
                let count = integer(fields.first()?)? as usize;
                let base = integer(fields.get(1)?)? as usize;
                if count < 3 {
                    return None;
                }
                let polygon = polygon_indices.get(base..base.checked_add(count)?)?;
                let first = integer(&polygon[0])?;
                for i in 1..count - 1 {
                    indices.extend([first, integer(&polygon[i])?, integer(&polygon[i + 1])?]);
                }
            }
        } else {
            let Value::Array(triangles) = mesh.properties.get("Triangles")? else {
                return None;
            };
            if triangles.len() > MAX_INDICES {
                return None;
            }
            indices.reserve(triangles.len());
            for v in triangles.iter() {
                indices.push(integer(v)?);
            }
            if indices.len() % 3 != 0 {
                return None;
            }
        }
        if indices.iter().any(|i| *i as usize >= vertices.len()) {
            return None;
        }
        let scale = vec3(geometry.properties.get("Scale.Scale"))?;
        let Some(Value::Quat(rotation)) = geometry.properties.get("Scale.Rotation") else {
            return None;
        };
        let inverse = [-rotation[0], -rotation[1], -rotation[2], rotation[3]];
        for vertex in &mut vertices {
            let local = rotate(*rotation, *vertex);
            *vertex = rotate(
                inverse,
                [
                    local[0] * scale[0],
                    local[1] * scale[1],
                    local[2] * scale[2],
                ],
            );
        }
        if scale[0] * scale[1] * scale[2] < 0.0 {
            for triangle in indices.chunks_exact_mut(3) {
                triangle.swap(1, 2);
            }
        }
        Some(Geometry::TriangleMesh {
            vertices: vertices.into(),
            indices: indices.into(),
        })
    }
}
fn integer(value: &Value) -> Option<u32> {
    if let Value::U64(v) = value {
        u32::try_from(*v).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Action;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn height_field_state() -> SceneState {
        let mut state = SceneState::default();
        for (id, name) in [
            (1, "PxRigidStatic"),
            (2, "PxShape"),
            (3, "PxHeightFieldGeometry"),
            (4, "PxHeightField"),
        ] {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: format!("physx::{name}"),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
        }
        Action::Push(1, "Shapes".into(), 2).apply(&mut state);
        Action::Set(2, "Geometry".into(), Value::Ref(3)).apply(&mut state);
        Action::Set(3, "HeightField".into(), Value::Ref(4)).apply(&mut state);
        for (name, value) in [
            ("RowScale", 2.0),
            ("HeightScale", 0.5),
            ("ColumnScale", 3.0),
        ] {
            Action::Set(3, name.into(), Value::F32(value)).apply(&mut state);
        }
        for (name, value) in [("NbRows", 2), ("NbColumns", 2), ("Format", 1)] {
            Action::Set(4, name.into(), Value::U64(value)).apply(&mut state);
        }
        Action::Set(4, "Thickness".into(), Value::F32(-1.0)).apply(&mut state);
        Action::Set(
            4,
            "Samples".into(),
            Value::Array(
                vec![
                    Value::Array(vec![Value::I64(-6), Value::U64(128), Value::U64(127)].into()),
                    Value::Array(vec![Value::I64(2), Value::U64(0), Value::U64(0)].into()),
                    Value::Array(vec![Value::I64(4), Value::U64(0), Value::U64(0)].into()),
                    Value::Array(vec![Value::I64(6), Value::U64(0), Value::U64(0)].into()),
                ]
                .into(),
            ),
        )
        .apply(&mut state);
        state
    }

    fn cached_vertices(cache: &mut MeshCache, state: &SceneState) -> Arc<Vec<[f32; 3]>> {
        let Geometry::TriangleMesh { vertices, .. } =
            cache.decode(state, &state.objects[&3]).unwrap()
        else {
            panic!("expected height-field geometry")
        };
        vertices
    }

    #[test]
    fn persistent_mesh_cache_shares_frames_and_invalidates_resource_updates() {
        let mut state = height_field_state();
        let mut cache = MeshCache::default();
        let original = cached_vertices(&mut cache, &state);
        assert!(Arc::ptr_eq(&original, &cached_vertices(&mut cache, &state)));
        Action::Set(
            1,
            "GlobalPose".into(),
            Value::Transform {
                position: [9.0, 2.0, 1.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
        )
        .apply(&mut state);
        cache.retain_resources(&state);
        assert!(Arc::ptr_eq(&original, &cached_vertices(&mut cache, &state)));

        let Value::Array(samples) = &mut state
            .objects
            .get_mut(&4)
            .unwrap()
            .properties
            .get_mut("Samples")
            .unwrap()
        else {
            panic!()
        };
        let Value::Array(sample) = &mut Arc::make_mut(samples)[0] else {
            panic!()
        };
        Arc::make_mut(sample)[0] = Value::I64(8);
        let modified = cached_vertices(&mut cache, &state);
        assert_eq!(original[0][1], -3.0);
        assert_eq!(modified[0][1], 4.0);
        assert!(!Arc::ptr_eq(&original, &modified));

        Action::Set(3, "HeightScale".into(), Value::F32(2.0)).apply(&mut state);
        assert_eq!(cached_vertices(&mut cache, &state)[0][1], 16.0);
        Action::Destroy(4).apply(&mut state);
        cache.retain_resources(&state);
        assert_eq!(cache.retained_bytes(), 0);
        assert!(cache.decode(&state, &state.objects[&3]).is_none());
    }

    #[test]
    fn mesh_cache_budget_eviction_preserves_geometry_and_id_reuse() {
        let mut state = height_field_state();
        let mut sizing = MeshCache::default();
        let original = cached_vertices(&mut sizing, &state);
        let mut cache = MeshCache::with_byte_limit(sizing.retained_bytes());
        let cached = cached_vertices(&mut cache, &state);
        for scale in 1..32 {
            Action::Set(3, "RowScale".into(), Value::F32(scale as f32)).apply(&mut state);
            assert_eq!(cached_vertices(&mut cache, &state)[2][0], scale as f32);
            assert!(cache.retained_bytes() <= cache.limit);
            assert_eq!(cache.entries.len(), 1);
        }
        assert_eq!(cached[2][0], original[2][0]);
        let mut replacement = state.objects[&4].clone();
        replacement.generation = 1;
        Action::Destroy(4).apply(&mut state);
        Action::Create(replacement).apply(&mut state);
        assert!(cache.decode(&state, &state.objects[&3]).is_none());
        Action::Set(3, "HeightField".into(), Value::Ref(4)).apply(&mut state);
        assert_eq!(cached_vertices(&mut cache, &state)[2][0], 31.0);

        let mut disabled = MeshCache::with_byte_limit(0);
        assert_eq!(
            *cached_vertices(&mut disabled, &state),
            *cached_vertices(&mut cache, &state)
        );
        assert_eq!(disabled.retained_bytes(), 0);
    }

    #[test]
    fn height_fields_decode_signed_heights_tessellation_holes_and_thickness() {
        let mut state = height_field_state();
        let Geometry::TriangleMesh { vertices, indices } =
            state.mesh_geometry(&state.objects[&3]).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            &*vertices,
            &[
                [0.0, -3.0, 0.0],
                [0.0, 1.0, 3.0],
                [2.0, 2.0, 0.0],
                [2.0, 3.0, 3.0]
            ]
        );
        assert_eq!(&*indices, &[2, 0, 3]);
        Action::Set(4, "Thickness".into(), Value::F32(1.0)).apply(&mut state);
        let Geometry::TriangleMesh { indices, .. } =
            state.mesh_geometry(&state.objects[&3]).unwrap()
        else {
            panic!()
        };
        assert_eq!(&*indices, &[2, 3, 0]);
        assert!(state.render_scene_with_limits(10, 59).objects.is_empty());
        assert_eq!(state.render_scene_with_limits(10, 60).objects.len(), 1);
    }

    #[test]
    fn height_fields_reject_truncation_invalid_dimensions_scalars_and_excessive_resources() {
        let original = height_field_state();
        for (id, property, value) in [
            (
                4,
                "Samples",
                Value::Array(vec![Value::Array(vec![Value::U64(0); 3].into()); 3].into()),
            ),
            (
                4,
                "Samples",
                Value::Array(vec![Value::Array(vec![Value::U64(0); 2].into()); 4].into()),
            ),
            (
                4,
                "Samples",
                Value::Array(
                    vec![
                        Value::Array(vec![Value::I64(32768), Value::U64(0), Value::U64(0)].into());
                        4
                    ]
                    .into(),
                ),
            ),
            (4, "NbRows", Value::U64(u32::MAX as u64)),
            (4, "NbRows", Value::U64(0)),
            (4, "NbColumns", Value::U64(1)),
            (4, "Format", Value::U64(2)),
            (4, "Thickness", Value::F32(f32::NAN)),
            (3, "HeightScale", Value::F32(f32::INFINITY)),
            (3, "RowScale", Value::F32(-1.0)),
            (3, "ColumnScale", Value::F32(0.0)),
            (3, "HeightScale", Value::F32(f32::MAX)),
        ] {
            let mut state = original.clone();
            Action::Set(id, property.into(), value).apply(&mut state);
            assert!(
                state.mesh_geometry(&state.objects[&3]).is_none(),
                "{id}: {property}"
            );
        }
        let mut state = original;
        Action::Set(4, "NbRows".into(), Value::U64(750)).apply(&mut state);
        Action::Set(4, "NbColumns".into(), Value::U64(750)).apply(&mut state);
        let sample = Value::Array(vec![Value::I64(0), Value::U64(0), Value::U64(0)].into());
        Action::Set(
            4,
            "Samples".into(),
            Value::Array(vec![sample; 750 * 750].into()),
        )
        .apply(&mut state);
        assert!(state.mesh_geometry(&state.objects[&3]).is_none());
    }

    #[test]
    fn height_field_keys_preserve_scale_kind_and_resource_generation() {
        let mut state = height_field_state();
        let key = state.mesh_key(&state.objects[&3]).unwrap();
        Action::Set(3, "RowScale".into(), Value::F32(4.0)).apply(&mut state);
        assert_ne!(key, state.mesh_key(&state.objects[&3]).unwrap());
        let mut replacement = state.objects[&4].clone();
        replacement.generation = 1;
        Action::Destroy(4).apply(&mut state);
        Action::Create(replacement).apply(&mut state);
        assert!(state.mesh_key(&state.objects[&3]).is_none());
        assert!(state.mesh_geometry(&state.objects[&3]).is_none());
        Action::Set(3, "HeightField".into(), Value::Ref(4)).apply(&mut state);
        Action::Set(3, "RowScale".into(), Value::F32(2.0)).apply(&mut state);
        let rebound = state.mesh_key(&state.objects[&3]).unwrap();
        assert_ne!(key, rebound);
        assert_eq!(rebound.generation, 1);
        let mut triangle_key = rebound.clone();
        triangle_key.kind = MeshKind::Triangle;
        assert_ne!(triangle_key, rebound);
    }

    #[test]
    fn shared_meshes_use_one_budget_allocation_and_distinct_scale_keys() {
        let mut state = SceneState::default();
        for (id, name) in [
            (1, "PxRigidStatic"),
            (2, "PxRigidStatic"),
            (11, "PxShape"),
            (12, "PxShape"),
            (21, "PxTriangleMeshGeometry"),
            (22, "PxTriangleMeshGeometry"),
            (30, "PxTriangleMesh"),
        ] {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: format!("physx::{name}"),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
        }
        Action::Set(
            30,
            "Points".into(),
            Value::Array(
                vec![
                    Value::Vec3([0.0; 3]),
                    Value::Vec3([1.0, 0.0, 0.0]),
                    Value::Vec3([0.0, 1.0, 0.0]),
                ]
                .into(),
            ),
        )
        .apply(&mut state);
        Action::Set(
            30,
            "Triangles".into(),
            Value::Array(vec![Value::U64(0), Value::U64(1), Value::U64(2)].into()),
        )
        .apply(&mut state);
        for (actor, shape, geometry) in [(1, 11, 21), (2, 12, 22)] {
            Action::Push(actor, "Shapes".into(), shape).apply(&mut state);
            Action::Set(shape, "Geometry".into(), Value::Ref(geometry)).apply(&mut state);
            Action::Set(geometry, "TriangleMesh".into(), Value::Ref(30)).apply(&mut state);
            Action::Set(geometry, "Scale.Scale".into(), Value::Vec3([1.0; 3])).apply(&mut state);
            Action::Set(
                geometry,
                "Scale.Rotation".into(),
                Value::Quat([0.0, 0.0, 0.0, 1.0]),
            )
            .apply(&mut state);
        }
        let shared = state.render_scene_with_limits(100, 48);
        assert_eq!(shared.objects.len(), 2);
        assert!(shared.diagnostics.is_empty());
        let Geometry::TriangleMesh {
            vertices: first,
            indices: first_indices,
        } = &shared.objects[0].geometry
        else {
            panic!()
        };
        let Geometry::TriangleMesh {
            vertices: second,
            indices: second_indices,
        } = &shared.objects[1].geometry
        else {
            panic!()
        };
        assert!(Arc::ptr_eq(first, second));
        assert!(Arc::ptr_eq(first_indices, second_indices));
        Action::Set(22, "Scale.Scale".into(), Value::Vec3([2.0; 3])).apply(&mut state);
        let limited = state.render_scene_with_limits(100, 48);
        assert_eq!(limited.objects.len(), 1);
        assert!(limited.diagnostics[0].contains("unique mesh allocation limit"));
        let scaled = state.render_scene_with_limits(100, 96);
        let Geometry::TriangleMesh { vertices, .. } = &scaled.objects[1].geometry else {
            panic!()
        };
        assert_eq!(vertices[1], [2.0, 0.0, 0.0]);
        let limited = state.render_scene_with_limits(1, 96);
        assert_eq!(limited.objects.len(), 1);
        assert!(limited.diagnostics[0].contains("shape limit"));
    }

    #[test]
    fn overlapping_hull_polygons_cannot_expand_without_a_bound() {
        let mut state = SceneState::default();
        let geometry = Object {
            id: 1,
            generation: 0,
            type_name: "physx::PxConvexMeshGeometry".into(),
            properties: BTreeMap::new(),
        };
        Action::Create(geometry).apply(&mut state);
        Action::Create(Object {
            id: 2,
            generation: 0,
            type_name: "physx::PxConvexMesh".into(),
            properties: BTreeMap::from([
                (
                    "Points".into(),
                    Value::Array(vec![Value::Vec3([0.0, 0.0, 0.0]); 3].into()),
                ),
                (
                    "HullPolygons".into(),
                    Value::Array(
                        vec![Value::Array(vec![Value::U64(65535), Value::U64(0)].into()); 100]
                            .into(),
                    ),
                ),
                (
                    "PolygonIndexes".into(),
                    Value::Array(vec![Value::U64(0); 65535].into()),
                ),
            ]),
        })
        .apply(&mut state);
        Action::Set(1, "ConvexMesh".into(), Value::Ref(2)).apply(&mut state);
        Action::Set(1, "Scale.Scale".into(), Value::Vec3([1.0; 3])).apply(&mut state);
        Action::Set(
            1,
            "Scale.Rotation".into(),
            Value::Quat([0.0, 0.0, 0.0, 1.0]),
        )
        .apply(&mut state);
        assert!(state.mesh_geometry(&state.objects[&1]).is_none());
    }
}
