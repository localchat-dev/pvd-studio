mod terrain;

use crate::render_region::{LocalBounds, ShapeRegion};
use crate::{Geometry, Object, SceneState, Value, refs, rotate, scalar, vec3};
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
pub(crate) use terrain::{FrameTerrainSelections, RegionMesh};
use terrain::{IndexedTerrain, TerrainCache, TerrainSource};

pub(crate) const MAX_VERTICES: usize = 1_000_000;
pub(crate) const MAX_INDICES: usize = 3_000_000;
const MAX_BOUNDS_BYTES: usize = 4 * 1024 * 1024;
const BOUNDS_ENTRY_BYTES: usize =
    std::mem::size_of::<MeshKey>() + std::mem::size_of::<CachedBounds>();

enum ArraySource {
    Values(Weak<Vec<Value>>),
    Packed(Weak<Box<[u8]>>),
}

impl ArraySource {
    fn capture(value: &Value) -> Option<Self> {
        match value {
            Value::Array(values) => Some(Self::Values(Arc::downgrade(values))),
            Value::MeshArray(values) => Some(Self::Packed(Arc::downgrade(values.bytes()))),
            _ => None,
        }
    }

    fn matches(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Values(previous), Value::Array(current)) => {
                previous.as_ptr() == Arc::as_ptr(current)
            }
            (Self::Packed(previous), Value::MeshArray(current)) => {
                previous.as_ptr() == Arc::as_ptr(current.bytes())
            }
            _ => false,
        }
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Values(a), Self::Values(b)) => a.ptr_eq(b),
            (Self::Packed(a), Self::Packed(b)) => a.ptr_eq(b),
            _ => false,
        }
    }
}

struct CachedBounds {
    values: ArraySource,
    dimensions: [usize; 2],
    bounds: Option<LocalBounds>,
}

impl CachedBounds {
    fn matches(&self, values: &Value, dimensions: [usize; 2]) -> bool {
        self.values.matches(values) && self.dimensions == dimensions
    }
}

struct CachedMesh {
    source: Object,
    geometry: Option<Geometry>,
    bytes: usize,
}

/// Derived geometry is shared across displayed frames while captured mesh data stays equal.
pub struct MeshCache {
    entries: BTreeMap<MeshKey, CachedMesh>,
    bounds: BTreeMap<MeshKey, CachedBounds>,
    terrain: TerrainCache,
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
            bounds: BTreeMap::new(),
            terrain: TerrainCache::default(),
            bytes: 0,
            limit,
        }
    }

    pub fn retained_bytes(&self) -> usize {
        self.bytes + self.bounds.len() * BOUNDS_ENTRY_BYTES + self.terrain.retained_bytes()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bounds.clear();
        self.terrain = TerrainCache::default();
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
        self.bounds.retain(|key, entry| {
            key.bounds_source(state)
                .is_some_and(|(values, dimensions)| entry.matches(values, dimensions))
        });
        self.terrain.retain_resources(state);
    }

    pub(crate) fn decode_terrain(
        &mut self,
        state: &SceneState,
        geometry: &Object,
        key: &MeshKey,
        region: ShapeRegion,
        max_mesh_bytes: usize,
        frame: &mut FrameTerrainSelections,
    ) -> RegionMesh {
        let Some(source) = TerrainSource::capture(state, key) else {
            return RegionMesh::Full;
        };
        let Some(mut terrain) = self.terrain.take(key, &source).or_else(|| {
            let decoded = state.mesh_geometry(geometry)?;
            IndexedTerrain::new(source, decoded)
        }) else {
            return RegionMesh::Invalid;
        };
        let result = terrain.select(
            region,
            max_mesh_bytes,
            frame.entry(key.clone()).or_default(),
        );
        let ordinary_bytes = self.bytes + self.bounds.len() * BOUNDS_ENTRY_BYTES;
        if terrain.index_bytes() <= self.limit
            && terrain.index_bytes() > self.limit - ordinary_bytes
        {
            self.entries.clear();
            self.bounds.clear();
            self.bytes = 0;
        }
        let available = self.limit - self.bytes - self.bounds.len() * BOUNDS_ENTRY_BYTES;
        self.terrain.insert(key.clone(), terrain, available);
        result
    }

    pub(crate) fn local_bounds(
        &mut self,
        state: &SceneState,
        key: &MeshKey,
    ) -> Option<LocalBounds> {
        let (values, dimensions) = key.bounds_source(state)?;
        if let Some(entry) = self.bounds.get(key)
            && entry.matches(values, dimensions)
        {
            return entry.bounds;
        }
        self.bounds.remove(key);
        let scale = key.scale.map(f32::from_bits);
        let bounds = if key.kind == MeshKind::HeightField {
            let [rows, columns] = dimensions;
            let (min, max) =
                values
                    .array_items()?
                    .try_fold((i16::MAX, i16::MIN), |(min, max), value| {
                        let Value::Array(fields) = value.as_ref() else {
                            return None;
                        };
                        let Value::I64(height) = fields.first()? else {
                            return None;
                        };
                        let height = i16::try_from(*height).ok()?;
                        Some((min.min(height), max.max(height)))
                    })?;
            LocalBounds::from_points([
                Some([0.0, f32::from(min) * scale[1], 0.0]),
                Some([
                    (rows - 1) as f32 * scale[0],
                    f32::from(max) * scale[1],
                    (columns - 1) as f32 * scale[2],
                ]),
            ])
        } else {
            let rotation = key.rotation.map(f32::from_bits);
            LocalBounds::from_points(values.array_items()?.map(|value| {
                vec3(Some(&value)).map(|point| scale_mesh_point(point, scale, rotation))
            }))
        };
        if self.bounds.len() < MAX_BOUNDS_BYTES / BOUNDS_ENTRY_BYTES
            && BOUNDS_ENTRY_BYTES <= self.limit.saturating_sub(self.retained_bytes())
        {
            // A weak reference tracks array replacements and copy-on-write edits without retaining samples.
            self.bounds.insert(
                key.clone(),
                CachedBounds {
                    values: ArraySource::capture(values)?,
                    dimensions,
                    bounds,
                },
            );
        }
        bounds
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
            if bytes > self.limit.saturating_sub(self.retained_bytes()) {
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

impl MeshKey {
    fn bounds_source<'a>(&self, state: &'a SceneState) -> Option<(&'a Value, [usize; 2])> {
        let source = state.objects.get(&self.resource)?;
        if source.generation != self.generation {
            return None;
        }
        let (property, dimensions) = if self.kind == MeshKind::HeightField {
            ("Samples", height_field_dimensions(source)?)
        } else {
            ("Points", [0; 2])
        };
        let values = source.properties.get(property)?;
        let count = values.array_len()?;
        let [rows, columns] = dimensions;
        (count <= MAX_VERTICES && (self.kind != MeshKind::HeightField || count == rows * columns))
            .then_some((values, dimensions))
    }
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

fn height_field_dimensions(mesh: &Object) -> Option<[usize; 2]> {
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
    Some([rows, columns])
}

fn height_field_geometry(mesh: &Object, geometry: &Object, thickness: f32) -> Option<Geometry> {
    let [rows, columns] = height_field_dimensions(mesh)?;
    let count = rows * columns;
    let index_count = (rows - 1) * (columns - 1) * 6;
    let scale = height_field_scale(geometry)?;
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
    fn height_field_thickness(&self, mesh: &Object) -> Option<f32> {
        if let Some(value) = mesh.properties.get("Thickness") {
            let thickness = scalar(Some(value))?;
            return thickness.is_finite().then_some(thickness);
        }
        let Value::Ref(id) = mesh.properties.get("Physics")? else {
            return None;
        };
        let physics = self.resolve_reference(mesh.id, "Physics", *id)?;
        // 4.1.2 不发送 Thickness；其他来源缺少字段时不能推断三角面绕序。
        (physics.type_name.ends_with("::PxPhysics")
            && physics.properties.get("Version.Major") == Some(&Value::U64(4))
            && physics.properties.get("Version.Minor") == Some(&Value::U64(1))
            && physics.properties.get("Version.Bugfix") == Some(&Value::U64(2)))
        .then_some(0.0)
    }

    pub(crate) fn mesh_key(&self, geometry: &Object) -> Option<MeshKey> {
        let kind = MeshKind::for_geometry(geometry)?;
        let property = kind.property();
        let id = refs(geometry.properties.get(property)).next()?;
        let resource = self.resolve_reference(geometry.id, property, id)?;
        let (scale, rotation) = if kind == MeshKind::HeightField {
            self.height_field_thickness(resource)?;
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
        let id = refs(geometry.properties.get(property)).next()?;
        let mesh = self.resolve_reference(geometry.id, property, id)?;
        if kind == MeshKind::HeightField {
            return height_field_geometry(mesh, geometry, self.height_field_thickness(mesh)?);
        }
        let points = mesh.properties.get("Points")?.array_items()?;
        if points.len() > MAX_VERTICES {
            return None;
        }
        let mut vertices = points.map(|v| vec3(Some(&v))).collect::<Option<Vec<_>>>()?;
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
            let triangles = mesh.properties.get("Triangles")?.array_items()?;
            if triangles.len() > MAX_INDICES {
                return None;
            }
            indices.reserve(triangles.len());
            for v in triangles {
                indices.push(integer(&v)?);
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
        for vertex in &mut vertices {
            *vertex = scale_mesh_point(*vertex, scale, *rotation);
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

fn scale_mesh_point(point: [f32; 3], scale: [f32; 3], rotation: [f32; 4]) -> [f32; 3] {
    let inverse = [-rotation[0], -rotation[1], -rotation[2], rotation[3]];
    let local = rotate(rotation, point);
    rotate(
        inverse,
        std::array::from_fn(|axis| local[axis] * scale[axis]),
    )
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
    use crate::{Action, RenderRegion};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    fn create(state: &mut SceneState, id: u64, name: &str) {
        Action::Create(Object {
            id,
            generation: 0,
            type_name: format!("physx::{name}"),
            properties: BTreeMap::new(),
        })
        .apply(state);
    }

    fn set_pose(
        state: &mut SceneState,
        id: u64,
        key: &str,
        position: [f32; 3],
        rotation: [f32; 4],
    ) {
        Action::Set(id, key.into(), Value::Transform { position, rotation }).apply(state);
    }

    fn add_shape(state: &mut SceneState, id: u64, name: &str) {
        let shape = id + 1;
        let geometry = id + 2;
        create(state, id, "PxRigidDynamic");
        create(state, shape, "PxShape");
        create(state, geometry, name);
        Action::Push(id, "Shapes".into(), shape).apply(state);
        Action::Set(shape, "Geometry".into(), Value::Ref(geometry)).apply(state);
    }

    const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
    const QUARTER_TURN: [f32; 4] = [
        0.0,
        0.0,
        std::f32::consts::FRAC_1_SQRT_2,
        std::f32::consts::FRAC_1_SQRT_2,
    ];

    #[test]
    fn render_region_composes_nonunit_poses_like_the_unity_frame_writer() {
        let mut state = SceneState::default();
        add_shape(&mut state, 1, "PxBoxGeometry");
        Action::Set(3, "HalfExtents".into(), Value::Vec3([0.5; 3])).apply(&mut state);
        set_pose(&mut state, 1, "GlobalPose", [0.0; 3], [0.0, 0.0, 0.0, 2.0]);
        set_pose(
            &mut state,
            2,
            "LocalPose",
            [10.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 0.5],
        );
        let region = RenderRegion::new([10.0, 0.0, 0.0], 1.0).unwrap();
        let scoped = state.render_scene_in_region(&mut MeshCache::default(), region);
        assert_eq!(scoped.objects.len(), 1);
        assert_eq!(scoped.objects[0].position, [10.0, 0.0, 0.0]);
        assert_eq!(state.render_scene().objects[0].position, [40.0, 0.0, 0.0]);
    }

    #[test]
    fn render_region_checks_rotated_local_shape_extent_and_dynamic_pose() {
        let mut state = SceneState::default();
        add_shape(&mut state, 1, "PxBoxGeometry");
        add_shape(&mut state, 4, "PxCapsuleGeometry");
        Action::Set(3, "HalfExtents".into(), Value::Vec3([20.0, 0.5, 0.5])).apply(&mut state);
        Action::Set(6, "Radius".into(), Value::F32(0.5)).apply(&mut state);
        Action::Set(6, "HalfHeight".into(), Value::F32(20.0)).apply(&mut state);
        for actor in [1, 4] {
            set_pose(
                &mut state,
                actor,
                "GlobalPose",
                [5.0, 21.0, 0.0],
                QUARTER_TURN,
            );
            set_pose(
                &mut state,
                actor + 1,
                "LocalPose",
                [0.0, 5.0, 0.0],
                IDENTITY,
            );
        }
        let mut cache = MeshCache::default();
        let region = RenderRegion::new([0.0; 3], 1.05).unwrap();
        let visible = state.render_scene_in_region(&mut cache, region);
        assert_eq!(
            visible
                .objects
                .iter()
                .map(|shape| shape.id)
                .collect::<Vec<_>>(),
            [1, 4]
        );
        assert_eq!(
            state
                .render_actors_in_region([4, 10], &mut cache, region)
                .objects[0]
                .id,
            4
        );

        set_pose(&mut state, 4, "GlobalPose", [5.0, 25.0, 0.0], QUARTER_TURN);
        for (center, expected) in [([0.0; 3], 1), ([1.55, 0.0, 0.0], 0)] {
            let region = RenderRegion::new(center, 1.05).unwrap();
            assert_eq!(
                state
                    .render_scene_in_region(&mut cache, region)
                    .objects
                    .len(),
                expected
            );
        }
        assert_eq!(state.render_scene().objects.len(), 2);
    }

    #[test]
    fn render_region_validates_inputs_and_includes_tangent_reference_geometry() {
        let mut state = SceneState::default();
        let mut cache = MeshCache::default();
        for radius in [f32::NAN, f32::INFINITY, 0.0, -1.0] {
            assert!(RenderRegion::new([0.0; 3], radius).is_none());
        }
        assert!(RenderRegion::new([0.0, f32::INFINITY, 0.0], 1.0).is_none());
        let region = RenderRegion::new([0.0; 3], 1.0).unwrap();
        assert!(
            state
                .render_scene_in_region(&mut cache, region)
                .objects
                .is_empty()
        );
        add_shape(&mut state, 1, "PxSphereGeometry");
        add_shape(&mut state, 4, "PxPlaneGeometry");
        add_shape(&mut state, 7, "PxSphereGeometry");
        Action::Set(3, "Radius".into(), Value::F32(1.0)).apply(&mut state);
        Action::Set(9, "Radius".into(), Value::F32(f32::INFINITY)).apply(&mut state);
        set_pose(&mut state, 1, "GlobalPose", [2.0, 0.0, 0.0], IDENTITY);
        set_pose(&mut state, 4, "GlobalPose", [1.0, 4.0, 0.0], IDENTITY);
        assert_eq!(
            state
                .render_scene_in_region(&mut cache, region)
                .objects
                .len(),
            2
        );
        set_pose(&mut state, 1, "GlobalPose", [f32::NAN, 0.0, 0.0], IDENTITY);
        set_pose(&mut state, 4, "GlobalPose", [1.0, 7.0, 0.0], IDENTITY);
        assert!(
            state
                .render_scene_in_region(&mut cache, region)
                .objects
                .is_empty()
        );
    }

    #[test]
    fn render_region_mesh_bounds_apply_scale_and_survive_resource_changes_without_allocating_far_meshes()
     {
        let mut state = SceneState::default();
        for id in [1, 4, 7] {
            add_shape(&mut state, id, "PxTriangleMeshGeometry");
            create(&mut state, id + 20, "PxTriangleMesh");
            Action::Set(id + 2, "TriangleMesh".into(), Value::Ref(id + 20)).apply(&mut state);
            Action::Set(id + 2, "Scale.Scale".into(), Value::Vec3([1.0, -2.0, 1.0]))
                .apply(&mut state);
            Action::Set(id + 2, "Scale.Rotation".into(), Value::Quat(QUARTER_TURN))
                .apply(&mut state);
            Action::Set(
                id + 20,
                "Triangles".into(),
                Value::Array(vec![Value::U64(0), Value::U64(1), Value::U64(2)].into()),
            )
            .apply(&mut state);
            let x = if id == 4 { 5.0 } else { 100.0 };
            Action::Set(
                id + 20,
                "Points".into(),
                Value::Array(
                    vec![
                        Value::Vec3([x, 0.0, 0.0]),
                        Value::Vec3([x, 1.0, 0.0]),
                        Value::Vec3([x, 0.0, 1.0]),
                    ]
                    .into(),
                ),
            )
            .apply(&mut state);
        }
        let mut cache = MeshCache::default();
        let region = RenderRegion::new([-10.0, 0.0, 0.0], 0.5).unwrap();
        let visible = state.render_scene_with_cache(1, 48, &mut cache, Some(region));
        assert_eq!(
            visible
                .objects
                .iter()
                .map(|shape| shape.id)
                .collect::<Vec<_>>(),
            [4]
        );
        assert!(visible.diagnostics.is_empty());
        assert_eq!(cache.entries.len(), 1);
        assert!(
            !cache
                .entries
                .contains_key(&state.mesh_key(&state.objects[&3]).unwrap())
        );

        let Value::Array(points) = state
            .objects
            .get_mut(&21)
            .unwrap()
            .properties
            .get_mut("Points")
            .unwrap()
        else {
            panic!()
        };
        for point in Arc::make_mut(points) {
            let Value::Vec3(point) = point else { panic!() };
            point[0] = 5.0;
        }
        assert_eq!(
            state
                .render_scene_in_region(&mut cache, region)
                .objects
                .len(),
            2
        );
    }

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

    fn terrain_triangles() -> SceneState {
        let mut state = SceneState::default();
        add_shape(&mut state, 1, "PxTriangleMeshGeometry");
        create(&mut state, 30, "PxTriangleMesh");
        Action::Set(3, "TriangleMesh".into(), Value::Ref(30)).apply(&mut state);
        Action::Set(3, "Scale.Scale".into(), Value::Vec3([1.0; 3])).apply(&mut state);
        Action::Set(
            3,
            "Scale.Rotation".into(),
            Value::Quat([0.0, 0.0, 0.0, 1.0]),
        )
        .apply(&mut state);
        let mut points = Vec::new();
        for i in -512..512 {
            let x = i as f32 * 128.0;
            points.extend([[x, 0.0, 0.0], [x, 1.0, 0.0], [x + 1.0, 0.0, 0.0]].map(Value::Vec3));
        }
        points.extend(
            [
                [-36000.0, 0.0, 0.0],
                [36000.0, 0.0, 0.0],
                [50000.0, 10.0, 0.0],
            ]
            .map(Value::Vec3),
        );
        let triangles = (0..points.len() as u64).map(Value::U64).collect::<Vec<_>>();
        Action::Set(30, "Points".into(), Value::Array(points.into())).apply(&mut state);
        Action::Set(30, "Triangles".into(), Value::Array(triangles.into())).apply(&mut state);
        state
    }

    fn surface(scene: &crate::RenderScene, object: usize) -> (&Arc<Vec<[f32; 3]>>, &Arc<Vec<u32>>) {
        let Geometry::TriangleMesh { vertices, indices } = &scene.objects[object].geometry else {
            panic!("expected a triangle surface")
        };
        (vertices, indices)
    }

    fn triangles(scene: &crate::RenderScene, object: usize) -> Vec<[[f32; 3]; 3]> {
        let (vertices, indices) = surface(scene, object);
        indices
            .chunks_exact(3)
            .map(|i| std::array::from_fn(|j| vertices[i[j] as usize]))
            .collect()
    }

    #[test]
    fn scoped_terrain_selects_local_blocks_and_preserves_a_crossing_triangle() {
        let state = terrain_triangles();
        let mut cache = MeshCache::default();
        let near = RenderRegion::new([0.0, 0.0, 0.0], 2.0).unwrap();
        let far = RenderRegion::new([-49152.0, 0.0, 0.0], 2.0).unwrap();
        let local = state.render_scene_in_region(&mut cache, near);
        let distant = state.render_scene_in_region(&mut cache, far);
        let local_triangles = triangles(&local, 0);
        let distant_triangles = triangles(&distant, 0);
        assert!(local_triangles.len() < 200);
        assert!(distant_triangles.len() < 200);
        assert!(local_triangles.contains(&[[0.0; 3], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]]));
        assert!(local_triangles.contains(&[
            [-36000.0, 0.0, 0.0],
            [36000.0, 0.0, 0.0],
            [50000.0, 10.0, 0.0]
        ]));
        assert!(distant_triangles.contains(&[
            [-49152.0, 0.0, 0.0],
            [-49152.0, 1.0, 0.0],
            [-49151.0, 0.0, 0.0]
        ]));
        assert!(!distant_triangles.contains(&local_triangles[0]));
        let repeat = state.render_scene_in_region(&mut cache, near);
        assert!(Arc::ptr_eq(surface(&local, 0).0, surface(&repeat, 0).0));
        assert!(Arc::ptr_eq(surface(&local, 0).1, surface(&repeat, 0).1));
        assert_eq!(
            triangles(&state.render_scene_cached(&mut cache), 0).len(),
            1025
        );
    }

    #[test]
    fn scoped_terrain_separates_shared_meshes_by_shape_pose_and_display_budget() {
        let mut state = terrain_triangles();
        for actor in [4, 7] {
            add_shape(&mut state, actor, "PxTriangleMeshGeometry");
            Action::Set(actor + 1, "Geometry".into(), Value::Ref(3)).apply(&mut state);
        }
        let half_turn_y = [0.0, 1.0, 0.0, 0.0];
        set_pose(
            &mut state,
            5,
            "LocalPose",
            [-49152.0, 0.0, 0.0],
            half_turn_y,
        );
        let mut cache = MeshCache::default();
        let region = RenderRegion::new([0.0; 3], 2.0).unwrap();
        let rendered = state.render_actors_in_region([1, 7, 4], &mut cache, region);
        assert_eq!(
            rendered
                .objects
                .iter()
                .map(|o| (o.id, o.shape_id))
                .collect::<Vec<_>>(),
            [(1, 2), (7, 8), (4, 5)]
        );
        assert!(Arc::ptr_eq(
            surface(&rendered, 0).0,
            surface(&rendered, 1).0
        ));
        assert!(!Arc::ptr_eq(
            surface(&rendered, 0).0,
            surface(&rendered, 2).0
        ));
        assert!(triangles(&rendered, 2).contains(&[
            [-49152.0, 0.0, 0.0],
            [-49152.0, 1.0, 0.0],
            [-49151.0, 0.0, 0.0]
        ]));
        let (vertices, indices) = surface(&rendered, 0);
        let first_bytes = vertices.len() * 12 + indices.len() * 4;
        let limited = state.render_actor_objects(
            [&state.objects[&1], &state.objects[&7], &state.objects[&4]],
            10,
            first_bytes,
            &mut cache,
            Vec::new(),
            Some(region),
        );
        assert_eq!(limited.objects.len(), 2);
        assert!(
            limited
                .diagnostics
                .iter()
                .any(|text| text.contains("unique mesh allocation limit"))
        );
        let tiny = state.render_scene_with_cache(10, 47, &mut cache, Some(region));
        assert!(tiny.objects.is_empty());
        assert!(
            tiny.diagnostics
                .iter()
                .any(|text| text.contains("unique mesh allocation limit"))
        );
        let mut transient = MeshCache::with_byte_limit(0);
        let limited = state.render_actor_objects(
            [&state.objects[&1], &state.objects[&7], &state.objects[&4]],
            10,
            first_bytes,
            &mut transient,
            Vec::new(),
            Some(region),
        );
        assert_eq!(limited.objects.len(), 2);
        assert!(Arc::ptr_eq(surface(&limited, 0).0, surface(&limited, 1).0));
        assert_eq!(transient.retained_bytes(), 0);
    }

    #[test]
    fn scoped_terrain_height_field_preserves_winding_holes_and_resource_edits() {
        let mut state = height_field_state();
        for name in ["NbRows", "NbColumns"] {
            Action::Set(4, name.into(), Value::U64(64)).apply(&mut state);
        }
        let sample = |height, m0, m1| {
            Value::Array(vec![Value::I64(height), Value::U64(m0), Value::U64(m1)].into())
        };
        let mut samples = vec![sample(0, 0, 0); 64 * 64];
        samples[0] = sample(-6, 128, 127);
        Action::Set(4, "Samples".into(), Value::Array(samples.into())).apply(&mut state);
        let region = RenderRegion::new([0.0; 3], 5.0).unwrap();
        let mut cache = MeshCache::default();
        let initial = state.render_scene_in_region(&mut cache, region);
        let local = triangles(&initial, 0);
        assert!(local.len() < 500);
        assert!(local.contains(&[[2.0, 0.0, 0.0], [0.0, -3.0, 0.0], [2.0, 0.0, 3.0]]));
        assert!(!local.contains(&[[0.0, 0.0, 3.0], [2.0, 0.0, 3.0], [0.0, -3.0, 0.0]]));
        let Value::Array(samples) = state
            .objects
            .get_mut(&4)
            .unwrap()
            .properties
            .get_mut("Samples")
            .unwrap()
        else {
            panic!()
        };
        Arc::make_mut(samples)[0] = sample(-8, 128, 127);
        let edited = state.render_scene_in_region(&mut cache, region);
        assert!(triangles(&edited, 0).contains(&[
            [2.0, 0.0, 0.0],
            [0.0, -4.0, 0.0],
            [2.0, 0.0, 3.0]
        ]));
        assert!(!Arc::ptr_eq(surface(&initial, 0).0, surface(&edited, 0).0));
        let mut replacement = state.objects[&4].clone();
        replacement.generation = 1;
        replacement
            .properties
            .insert("Thickness".into(), Value::F32(1.0));
        Action::Destroy(4).apply(&mut state);
        Action::Create(replacement).apply(&mut state);
        assert!(
            state
                .render_scene_in_region(&mut cache, region)
                .objects
                .is_empty()
        );
        Action::Set(3, "HeightField".into(), Value::Ref(4)).apply(&mut state);
        let rebound = state.render_scene_in_region(&mut cache, region);
        assert!(triangles(&rebound, 0).contains(&[
            [2.0, 0.0, 0.0],
            [2.0, 0.0, 3.0],
            [0.0, -4.0, 0.0]
        ]));
        assert!(cache.retained_bytes() <= cache.limit);
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
    fn height_fields_without_thickness_preserve_standard_winding_and_cache_identity() {
        let mut state = height_field_state();
        let mut cache = MeshCache::default();
        Action::Set(4, "Thickness".into(), Value::F32(1.0)).apply(&mut state);
        let Geometry::TriangleMesh { indices, .. } =
            cache.decode(&state, &state.objects[&3]).unwrap()
        else {
            panic!()
        };
        assert_eq!(&*indices, &[2, 3, 0]);
        state
            .objects
            .get_mut(&4)
            .unwrap()
            .properties
            .remove("Thickness");
        assert!(cache.decode(&state, &state.objects[&3]).is_none());
        let physics = Object {
            id: 5,
            generation: 0,
            type_name: "physx::PxPhysics".into(),
            properties: BTreeMap::from([
                ("Version.Major".into(), Value::U64(4)),
                ("Version.Minor".into(), Value::U64(1)),
                ("Version.Bugfix".into(), Value::U64(2)),
            ]),
        };
        Action::Create(physics.clone()).apply(&mut state);
        Action::Set(4, "Physics".into(), Value::Ref(5)).apply(&mut state);
        let Geometry::TriangleMesh { indices, .. } =
            cache.decode(&state, &state.objects[&3]).unwrap()
        else {
            panic!()
        };
        assert_eq!(&*indices, &[2, 0, 3]);
        assert_eq!(state.render_scene().objects.len(), 1);
        for (name, invalid) in [
            ("Version.Major", Value::U64(3)),
            ("Version.Minor", Value::U64(0)),
            ("Version.Bugfix", Value::U64(1)),
            ("Version.Major", Value::String("4".into())),
        ] {
            Action::Set(5, name.into(), invalid).apply(&mut state);
            assert!(cache.decode(&state, &state.objects[&3]).is_none());
            Action::Set(5, name.into(), physics.properties[name].clone()).apply(&mut state);
            assert!(cache.decode(&state, &state.objects[&3]).is_some());
        }
        for thickness in [Value::F32(f32::NAN), Value::String("0".into())] {
            Action::Set(4, "Thickness".into(), thickness).apply(&mut state);
            assert!(cache.decode(&state, &state.objects[&3]).is_none());
        }
        state
            .objects
            .get_mut(&4)
            .unwrap()
            .properties
            .remove("Thickness");
        Action::Destroy(5).apply(&mut state);
        let replacement = Object {
            generation: 1,
            ..physics
        };
        Action::Create(replacement).apply(&mut state);
        assert!(cache.decode(&state, &state.objects[&3]).is_none());
        Action::Set(4, "Physics".into(), Value::Ref(5)).apply(&mut state);
        assert!(cache.decode(&state, &state.objects[&3]).is_some());
        state.objects.get_mut(&5).unwrap().type_name = "custom::Version".into();
        assert!(cache.decode(&state, &state.objects[&3]).is_none());
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
            (4, "Thickness", Value::String("0".into())),
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
