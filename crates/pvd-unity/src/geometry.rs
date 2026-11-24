mod resources;
pub use resources::{ResourceCache, append_queries, append_rays};

use pvd_core::{
    DisplaySpatialIndex, Geometry, LatestSceneFrame, MeshCache, Object, RenderObject, RenderRegion,
    RenderScene, SceneState, Value,
    live::SceneUpdate,
    room_scope::{RoomScope, scene_for_object},
    rooms::RoomDirectory,
};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

type MeshId = (u64, u64, u32, u64, u32);
type Meshes = BTreeMap<MeshId, Geometry>;

#[derive(Clone, Copy)]
pub struct FocusRequest {
    pub mode: u32,
    pub actor: u64,
    pub generation: u32,
    pub center: [f32; 3],
    pub radius: f32,
}

impl FocusRequest {
    pub fn region(self, state: &SceneState) -> Result<RenderRegion, String> {
        let center = match self.mode {
            1 => self.center,
            2 => {
                let actor = state.objects.get(&self.actor).ok_or_else(|| {
                    format!("Focus actor {} is missing in this frame", self.actor)
                })?;
                if actor.generation != self.generation {
                    return Err(format!(
                        "Focus actor {} generation changed from {} to {}",
                        self.actor, self.generation, actor.generation
                    ));
                }
                if !matches!(
                    actor.type_name.rsplit("::").next(),
                    Some("PxRigidStatic" | "PxRigidDynamic" | "PxArticulationLink")
                ) {
                    return Err(format!(
                        "Focus object {} must be a rigid actor or articulation link",
                        self.actor
                    ));
                }
                let Some(Value::Transform { position, .. }) = actor.properties.get("GlobalPose")
                else {
                    return Err(format!(
                        "Focus actor {} has no recorded GlobalPose",
                        self.actor
                    ));
                };
                *position
            }
            mode => return Err(format!("Unknown focus mode {mode}")),
        };
        RenderRegion::new(center, self.radius)
            .ok_or_else(|| "Focus requires a finite center and a finite positive radius".into())
    }
}

#[derive(Default)]
pub struct FrameCache {
    scope: Option<(u64, u64)>,
    token: u64,
    meshes: Meshes,
    decoded: MeshCache,
    decoded_scope: Option<(u64, u64)>,
    spatial_session: Option<u64>,
    spatial: Option<DisplaySpatialIndex>,
    rooms: RoomAssignments,
}

// Tokens must remain unique across release, shutdown and concurrent readers.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn next_token() -> Result<u64, String> {
    NEXT_TOKEN
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| t.checked_add(1))
        .map_err(|_| "Frame token overflow".into())
}

impl FrameCache {
    pub fn retain_rooms(&mut self, update: &SceneUpdate, previous: &SceneState) {
        self.rooms.retain_for_update(update, previous);
    }

    pub fn sync_spatial(&mut self, session_id: u64, state: &SceneState, changed: Option<&[u64]>) {
        self.sync_spatial_for_update(
            session_id,
            state,
            changed,
            changed.is_some_and(|objects| !objects.is_empty()),
        );
    }

    fn sync_spatial_for_update(
        &mut self,
        session_id: u64,
        state: &SceneState,
        changed: Option<&[u64]>,
        rooms_changed: bool,
    ) {
        if self.spatial_session != Some(session_id) || changed.is_none() {
            self.spatial_session = Some(session_id);
            self.spatial = None;
            self.rooms = RoomAssignments::default();
        } else if let Some(changed) = changed {
            self.rooms.advance(rooms_changed);
            if let Some(spatial) = &mut self.spatial {
                spatial.sync(state, changed);
            }
        }
    }

    pub fn sync_latest(
        &mut self,
        session_id: u64,
        frame: &LatestSceneFrame,
        previous: Option<usize>,
    ) {
        if previous != Some(frame.index) {
            if let Some((objects, rooms_changed)) =
                previous.and_then(|index| frame.changes_since(index))
            {
                self.sync_spatial_for_update(
                    session_id,
                    &frame.scene,
                    Some(&objects),
                    rooms_changed,
                );
            } else {
                self.sync_spatial(session_id, &frame.scene, None);
            }
        }
    }

    pub fn encode(
        &mut self,
        state: &SceneState,
        rooms: &RoomDirectory,
        scope: (u64, u64),
        base: u64,
    ) -> Result<Vec<u8>, String> {
        let (scene, memberships) = self.render(state, rooms, scope, base, None)?;
        self.write(state, scene, &memberships, scope, base)
    }

    pub fn render(
        &mut self,
        state: &SceneState,
        rooms: &RoomDirectory,
        scope: (u64, u64),
        base: u64,
        region: Option<RenderRegion>,
    ) -> Result<(RenderScene, BTreeMap<u64, u64>), String> {
        if self.decoded_scope != Some(scope) || base == 0 {
            self.decoded.clear();
        }
        self.decoded_scope = Some(scope);
        if self.spatial_session != Some(scope.0) {
            return render(state, rooms, scope.1, &mut self.decoded, region);
        }
        let candidates = region.and_then(|region| {
            self.spatial
                .get_or_insert_with(|| DisplaySpatialIndex::new(state))
                .candidates(region)
        });
        self.rooms
            .render(state, rooms, scope.1, &mut self.decoded, region, candidates)
    }

    fn write(
        &mut self,
        state: &SceneState,
        scene: RenderScene,
        rooms: &BTreeMap<u64, u64>,
        scope: (u64, u64),
        base: u64,
    ) -> Result<Vec<u8>, String> {
        let base = if self.scope == Some(scope) && base == self.token {
            base
        } else {
            0
        };
        let token = next_token()?;
        let mut data = b"PVS2".to_vec();
        data.extend(token.to_le_bytes());
        data.extend(base.to_le_bytes());
        let mut next = Meshes::new();
        let previous = (base != 0).then_some(&self.meshes);
        let data = write_records(state, scene, rooms, data, previous, Some(&mut next))?;
        self.scope = Some(scope);
        self.meshes = next;
        self.token = token;
        Ok(data)
    }
}

fn same_mesh(previous: &Geometry, current: &Geometry) -> bool {
    let (
        Geometry::TriangleMesh {
            vertices: av,
            indices: ai,
        },
        Geometry::TriangleMesh {
            vertices: bv,
            indices: bi,
        },
    ) = (previous, current)
    else {
        return false;
    };
    (Arc::ptr_eq(ai, bi) || ai == bi)
        && (Arc::ptr_eq(av, bv)
            || av.len() == bv.len()
                && av
                    .iter()
                    .flatten()
                    .zip(bv.iter().flatten())
                    .all(|(a, b)| a.to_bits() == b.to_bits()))
}

const MAX_BYTES: usize = 32 * 1024 * 1024;
const RECORD_BYTES: usize = 2 * 8 + 2 * 4 + 8 + 2 + 14 * 4;

pub fn encode(state: &SceneState, rooms: &RoomDirectory, selected: u64) -> Result<Vec<u8>, String> {
    let (scene, memberships) = render(state, rooms, selected, &mut MeshCache::default(), None)?;
    encode_shapes(state, scene, &memberships)
}

fn render(
    state: &SceneState,
    rooms: &RoomDirectory,
    selected: u64,
    cache: &mut MeshCache,
    region: Option<RenderRegion>,
) -> Result<(RenderScene, BTreeMap<u64, u64>), String> {
    RoomAssignments::default().render(state, rooms, selected, cache, region, None)
}

#[derive(Default)]
struct RoomAssignments {
    revision: u64,
    prepared: bool,
    all: Option<(u64, BTreeMap<u64, u64>)>,
    scope: RoomScope,
}

impl RoomAssignments {
    fn retain_for_update(&mut self, update: &SceneUpdate, previous: &SceneState) {
        self.scope
            .retain_for_update(update, previous, self.revision);
        if self.all.is_some()
            && update.changes_properties(
                previous,
                &[
                    "RigidStatics",
                    "RigidDynamics",
                    "Actors",
                    "Aggregates",
                    "Scene",
                    "Parent",
                ],
            )
        {
            self.all = None;
        }
        self.prepared = true;
    }

    fn advance(&mut self, changed: bool) {
        let prepared = std::mem::take(&mut self.prepared);
        if changed || prepared {
            if !prepared {
                self.all = None;
                self.scope = RoomScope::default();
            }
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn render(
        &mut self,
        state: &SceneState,
        directory: &RoomDirectory,
        selected: u64,
        cache: &mut MeshCache,
        region: Option<RenderRegion>,
        candidates: Option<Vec<u64>>,
    ) -> Result<(RenderScene, BTreeMap<u64, u64>), String> {
        if selected == 0 {
            let scene = match (region, candidates) {
                (Some(region), Some(ids)) => state.render_actors_in_region(ids, cache, region),
                (Some(region), None) => state.render_scene_in_region(cache, region),
                (None, _) => state.render_scene_cached(cache),
            };
            if scene.objects.is_empty() {
                return Ok((scene, BTreeMap::new()));
            }
            if self
                .all
                .as_ref()
                .is_none_or(|(revision, _)| *revision != directory.revision())
            {
                self.all = Some((directory.revision(), actor_rooms(state, directory)));
            }
            let all = &self.all.as_ref().expect("room assignments prepared").1;
            let memberships = scene
                .objects
                .iter()
                .filter_map(|object| all.get(&object.id).map(|&room| (object.id, room)))
                .collect();
            return Ok((scene, memberships));
        }
        let info = directory
            .iter()
            .find(|r| r.number == selected)
            .ok_or("unknown physical scene")?;
        self.scope.update(state, Some(info.scene), self.revision);
        let actors = self.scope.actors();
        let scene = match (region, candidates) {
            (Some(region), Some(ids)) => state.render_actors_in_region(
                ids.into_iter().filter(|id| actors.contains(id)),
                cache,
                region,
            ),
            (Some(region), None) => {
                state.render_actors_in_region(actors.iter().copied(), cache, region)
            }
            (None, _) => state.render_actors_cached(actors.iter().copied(), cache),
        };
        let memberships = scene
            .objects
            .iter()
            .map(|object| (object.id, selected))
            .collect();
        Ok((scene, memberships))
    }
}

fn referenced<'a>(
    state: &'a SceneState,
    object: &'a Object,
    property: &'a str,
) -> impl Iterator<Item = &'a Object> {
    let values: &[Value] = match object.properties.get(property) {
        Some(Value::Array(values)) => values,
        Some(value) => std::slice::from_ref(value),
        None => &[],
    };
    values.iter().filter_map(move |value| match value {
        Value::Ref(id) => state.resolve_reference(object.id, property, *id),
        _ => None,
    })
}

fn merge_room(members: &mut BTreeMap<u64, u64>, id: u64, room: u64) {
    members
        .entry(id)
        .and_modify(|old| {
            if *old != room {
                *old = 0;
            }
        })
        .or_insert(room);
}

fn actor_rooms(state: &SceneState, rooms: &RoomDirectory) -> BTreeMap<u64, u64> {
    let mut actors = BTreeMap::new();
    let mut aggregates = BTreeMap::new();
    for room in rooms.iter() {
        let Some(scene) = state
            .objects
            .get(&room.scene.id)
            .filter(|o| o.generation == room.scene.generation)
        else {
            continue;
        };
        for key in ["RigidStatics", "RigidDynamics", "Actors"] {
            for actor in referenced(state, scene, key) {
                merge_room(&mut actors, actor.id, room.number);
            }
        }
        for aggregate in referenced(state, scene, "Aggregates") {
            merge_room(&mut aggregates, aggregate.id, room.number);
        }
    }
    let direct_room = |object: &Object| {
        scene_for_object(state, object)
            .and_then(|scene| rooms.get(scene))
            .map_or(0, |room| room.number)
    };
    for aggregate in state
        .objects
        .values()
        .filter(|o| o.type_name.ends_with("::PxAggregate"))
    {
        let room = if aggregate.properties.contains_key("Scene") {
            direct_room(aggregate)
        } else if let Some(room) = aggregates.get(&aggregate.id) {
            *room
        } else {
            continue;
        };
        for actor in referenced(state, aggregate, "Actors") {
            merge_room(&mut actors, actor.id, room);
        }
    }
    for object in state.objects.values() {
        // Explicit and lifetime-checked Scene references take precedence over inverse lists.
        match object.type_name.rsplit("::").next() {
            Some("PxArticulationLink") => {
                actors.insert(object.id, direct_room(object));
            }
            Some("PxRigidStatic" | "PxRigidDynamic") if object.properties.contains_key("Scene") => {
                actors.insert(object.id, direct_room(object));
            }
            _ => {}
        }
    }
    actors
}

fn pose(object: &Object, property: &str) -> Result<[f32; 7], String> {
    let (p, q) = match object.properties.get(property) {
        Some(Value::Transform { position, rotation }) => (*position, *rotation),
        None => ([0.; 3], [0., 0., 0., 1.]),
        _ => return Err(format!("Invalid {property} on object {}", object.id)),
    };
    let q = q.map(f64::from);
    let length = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !length.is_finite() || length == 0. || !p.iter().all(|v| v.is_finite()) {
        return Err(format!("Invalid {property} on object {}", object.id));
    }
    let [x, y, z, w] = q.map(|v| (v / length) as f32);
    Ok([p[0], p[1], p[2], x, y, z, w])
}

/// PVS1 carries actor world poses, shape local poses and capture-space triangle winding.
fn encode_shapes(
    state: &SceneState,
    scene: RenderScene,
    rooms: &BTreeMap<u64, u64>,
) -> Result<Vec<u8>, String> {
    write_records(state, scene, rooms, b"PVS1".to_vec(), None, None)
}

fn write_records(
    state: &SceneState,
    scene: RenderScene,
    rooms: &BTreeMap<u64, u64>,
    mut data: Vec<u8>,
    previous: Option<&Meshes>,
    mut next: Option<&mut Meshes>,
) -> Result<Vec<u8>, String> {
    let mut expanded = data.len() + 4;
    data.extend(
        u32::try_from(scene.objects.len())
            .map_err(|_| "Too many scene shapes")?
            .to_le_bytes(),
    );
    for object in scene.objects {
        let (kind, payload_bytes) = payload_layout(&object.geometry);
        expanded = expanded
            .saturating_add(RECORD_BYTES)
            .saturating_add(payload_bytes);
        if expanded > MAX_BYTES {
            return Err("Scene surfaces exceed the 32 MiB frame limit".into());
        }
        let (room, actor, shape) = shape_identity(state, &object, rooms)?;
        let mut kind = kind;
        if kind == 4
            && let Some(next) = next.as_deref_mut()
        {
            let key = (room, actor.id, actor.generation, shape.id, shape.generation);
            if previous
                .and_then(|cache| cache.get(&key))
                .is_some_and(|old| same_mesh(old, &object.geometry))
            {
                kind = 5;
            }
            next.insert(key, object.geometry.clone());
        }
        write_prefix(&mut data, room, actor, shape, &object, kind)?;
        if kind == 5 {
            continue;
        }
        write_payload(&mut data, &object.geometry)?;
    }
    Ok(data)
}

fn payload_layout(geometry: &Geometry) -> (u8, usize) {
    match geometry {
        Geometry::Box { .. } => (0, 12),
        Geometry::Sphere { .. } => (1, 4),
        Geometry::Capsule { .. } => (2, 8),
        Geometry::Plane => (3, 0),
        Geometry::TriangleMesh { vertices, indices } => (
            4,
            8usize
                .saturating_add(vertices.len().saturating_mul(12))
                .saturating_add(indices.len().saturating_mul(4)),
        ),
    }
}

fn shape_identity<'a>(
    state: &'a SceneState,
    object: &RenderObject,
    rooms: &BTreeMap<u64, u64>,
) -> Result<(u64, &'a Object, &'a Object), String> {
    let actor = state.objects.get(&object.id).ok_or("Missing frame actor")?;
    let shape = state
        .objects
        .get(&object.shape_id)
        .ok_or("Missing frame shape")?;
    let room = rooms.get(&actor.id).copied().unwrap_or(0);
    Ok((room, actor, shape))
}

fn write_prefix(
    data: &mut Vec<u8>,
    room: u64,
    actor: &Object,
    shape: &Object,
    object: &RenderObject,
    kind: u8,
) -> Result<(), String> {
    data.extend(room.to_le_bytes());
    data.extend(actor.id.to_le_bytes());
    data.extend(actor.generation.to_le_bytes());
    data.extend(shape.id.to_le_bytes());
    data.extend(shape.generation.to_le_bytes());
    let activity = if object.dynamic {
        1 + u8::from(object.sleeping)
    } else {
        0
    };
    data.extend([activity, kind]);
    for v in pose(actor, "GlobalPose")?
        .into_iter()
        .chain(pose(shape, "LocalPose")?)
    {
        data.extend(v.to_le_bytes());
    }
    Ok(())
}

fn write_payload(data: &mut Vec<u8>, geometry: &Geometry) -> Result<(), String> {
    match geometry {
        Geometry::Box { half_extents } => dimensions(data, half_extents)?,
        Geometry::Sphere { radius } => dimensions(data, &[*radius])?,
        Geometry::Capsule {
            radius,
            half_height,
        } => dimensions(data, &[*radius, *half_height])?,
        Geometry::Plane => (),
        Geometry::TriangleMesh { vertices, indices } => {
            if !indices.len().is_multiple_of(3)
                || indices.iter().any(|&i| i as usize >= vertices.len())
            {
                return Err("Invalid triangle indices".into());
            }
            data.extend((vertices.len() as u32).to_le_bytes());
            data.extend((indices.len() as u32).to_le_bytes());
            for &v in vertices.iter().flatten() {
                if !v.is_finite() {
                    return Err("Invalid mesh vertex".into());
                }
                data.extend(v.to_le_bytes());
            }
            for i in indices.iter() {
                data.extend(i.to_le_bytes());
            }
        }
    }
    Ok(())
}

fn dimensions(data: &mut Vec<u8>, values: &[f32]) -> Result<(), String> {
    for &value in values {
        if !value.is_finite() || value < 0. {
            return Err("Invalid primitive dimensions".into());
        }
        data.extend(value.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
