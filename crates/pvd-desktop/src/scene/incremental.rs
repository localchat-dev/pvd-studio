use super::*;
use pvd_core::{Object, SceneState, Value, live::SceneUpdate};

type ObjectKey = (u64, u64);
type SharedKey = (usize, usize);

#[derive(Default)]
pub(crate) struct SceneChanges {
    range: Option<(u64, u64)>,
    full: bool,
    objects: HashSet<u64>,
    poses: HashSet<u64>,
}

impl SceneChanges {
    pub(crate) fn changed(&self, from: u64, to: u64) -> Option<&HashSet<u64>> {
        (!self.full && self.range == Some((from, to))).then_some(&self.objects)
    }

    pub(crate) fn record(&mut self, update: &SceneUpdate, revision: u64) {
        self.record_update(update, None, revision);
    }

    pub(crate) fn record_replay(
        &mut self,
        update: &SceneUpdate,
        state: &SceneState,
        revision: u64,
    ) {
        self.record_update(update, Some(state), revision);
    }

    fn record_update(&mut self, update: &SceneUpdate, state: Option<&SceneState>, revision: u64) {
        match self.range {
            Some((_, end)) if end != revision => self.full = true,
            None => self.range = Some((revision, revision)),
            _ => {}
        }
        self.range.as_mut().unwrap().1 = revision.wrapping_add(1);
        self.full |= update.replace_all;
        for (id, next) in update.changed_objects() {
            let first_change = self.objects.insert(id);
            if !self.full
                && (first_change || self.poses.contains(&id))
                && state.is_some_and(|state| rigid_motion_only(state, update, id, next))
            {
                self.poses.insert(id);
            } else {
                self.poses.remove(&id);
            }
        }
    }

    pub(super) fn take(&mut self, from: u64, to: u64) -> Option<(HashSet<u64>, HashSet<u64>)> {
        let current = std::mem::take(self);
        (!current.full && current.range == Some((from, to)))
            .then_some((current.objects, current.poses))
    }
}

fn rigid_motion_only(
    state: &SceneState,
    update: &SceneUpdate,
    id: u64,
    next: Option<&Object>,
) -> bool {
    let (Some(previous), Some(next)) = (state.objects.get(&id), next) else {
        return false;
    };
    if !(next.type_name.ends_with("::PxRigidDynamic")
        || next.type_name.ends_with("::PxRigidStatic"))
        || previous.generation != next.generation
        || previous.type_name != next.type_name
        || !matches!(
            next.properties.get("GlobalPose"),
            Some(Value::Transform { .. })
        )
    {
        return false;
    }
    ["IsSleeping", "RigidBodyFlags", "$pickable"]
        .into_iter()
        .all(|name| previous.properties.get(name) == next.properties.get(name))
        && !update.changes_object_properties(state, id, &["Shapes"])
}

pub(super) struct ShapePose {
    slot: usize,
    position: [f32; 3],
    rotation: [f32; 4],
}

#[derive(Clone, Copy)]
pub(super) struct AssetUse {
    pub mesh: u64,
    pub material: MaterialKey,
}

#[derive(Default)]
pub(super) struct SceneIndex {
    ready: bool,
    dependencies: HashMap<u64, HashSet<u64>>,
    dependents: HashMap<u64, HashSet<u64>>,
    actors: HashMap<u64, HashSet<ObjectKey>>,
    slots: HashMap<ObjectKey, usize>,
    assets: HashMap<ObjectKey, AssetUse>,
    mesh_users: HashMap<u64, usize>,
    material_users: HashMap<MaterialKey, usize>,
    source: SourceResources,
    pub last_actor_count: usize,
    pub last_pose_count: usize,
}

#[derive(Default)]
struct ActorResources {
    shapes: usize,
    shared: Vec<SharedKey>,
}

struct GeometryUsers {
    users: usize,
    bytes: usize,
}

#[derive(Default)]
pub(super) struct SourceResources {
    actors: HashMap<u64, ActorResources>,
    shared: HashMap<SharedKey, GeometryUsers>,
    shapes: usize,
    bytes: usize,
}

impl SourceResources {
    pub fn from_objects(objects: &[RenderObject]) -> Self {
        let mut result = Self::default();
        for object in objects {
            result.add(object);
        }
        result
    }

    fn add(&mut self, object: &RenderObject) {
        self.shapes += 1;
        let actor = self.actors.entry(object.id).or_default();
        actor.shapes += 1;
        if let Some(key) = shared_key(&object.geometry) {
            actor.shared.push(key);
            let resource = self.shared.entry(key).or_insert_with(|| {
                let bytes = geometry_bytes(&object.geometry);
                self.bytes += bytes;
                GeometryUsers { users: 0, bytes }
            });
            resource.users += 1;
        }
    }

    fn remove(&mut self, actor: u64) {
        if let Some(resources) = self.actors.remove(&actor) {
            self.shapes -= resources.shapes;
            for key in resources.shared {
                let resource = self.shared.get_mut(&key).unwrap();
                resource.users -= 1;
                if resource.users == 0 {
                    self.bytes -= resource.bytes;
                    self.shared.remove(&key);
                }
            }
        }
    }

    fn replacement_fits(&self, actors: &HashSet<u64>, objects: &[RenderObject]) -> bool {
        let mut shapes = self.shapes;
        let mut removed = HashMap::<SharedKey, usize>::new();
        for actor in actors.iter().filter_map(|id| self.actors.get(id)) {
            shapes -= actor.shapes;
            for key in &actor.shared {
                *removed.entry(*key).or_default() += 1;
            }
        }
        if shapes.saturating_add(objects.len()) > 100_000 {
            return false;
        }
        let mut bytes = self.bytes;
        for (key, users) in &removed {
            let resource = &self.shared[key];
            if *users == resource.users {
                bytes -= resource.bytes;
            }
        }
        let mut added = HashSet::new();
        for object in objects {
            if let Some(key) = shared_key(&object.geometry) {
                let retained = self.shared.get(&key).is_some_and(|resource| {
                    removed.get(&key).copied().unwrap_or(0) < resource.users
                });
                if !retained && added.insert(key) {
                    bytes = bytes.saturating_add(geometry_bytes(&object.geometry));
                }
            }
        }
        bytes <= 128 * 1024 * 1024
    }
}

pub(super) fn object_visible(studio: &Studio, object: &RenderObject) -> bool {
    studio.room_scope.contains(object.id)
        && (studio.prefs.show_kinematic
            || !object.dynamic
            || !is_kinematic(&studio.state, object.id))
        && (!object.sleeping
            || (studio.prefs.show_sleeping
                && studio
                    .prefs
                    .sleeping_scope
                    .includes(studio.is_selected(object.id))))
        && if object.dynamic {
            studio.prefs.show_dynamic
        } else {
            studio.prefs.show_static
        }
        && trigger_visible(studio, object)
}

fn trigger_visible(studio: &Studio, object: &RenderObject) -> bool {
    let selected = studio.is_selected(object.id) || studio.is_selected(object.shape_id);
    if studio.prefs.show_triggers && studio.prefs.trigger_scope.includes(selected) {
        return true;
    }
    studio
        .state
        .resolve_reference(object.id, "Shapes", object.shape_id)
        .and_then(|shape| shape.properties.get("Flags"))
        .and_then(|value| match value {
            Value::U64(value) => u8::try_from(*value).ok(),
            Value::I64(value) => u8::try_from(*value).ok(),
            _ => None,
        })
        .is_none_or(|flags| flags & 4 == 0)
}

fn actor(object: &Object) -> bool {
    [
        "::PxRigidDynamic",
        "::PxRigidStatic",
        "::PxArticulationLink",
    ]
    .iter()
    .any(|kind| object.type_name.ends_with(kind))
}

fn references(object: &Object, property: &str) -> Vec<u64> {
    match object.properties.get(property) {
        Some(Value::Ref(id)) => vec![*id],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| {
                if let Value::Ref(id) = value {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn dependencies(state: &SceneState, id: u64) -> HashSet<u64> {
    let mut result = HashSet::new();
    let Some(source) = state.objects.get(&id).filter(|object| actor(object)) else {
        return result;
    };
    for shape_id in references(source, "Shapes") {
        result.insert(shape_id);
        let Some(shape) = state.resolve_reference(id, "Shapes", shape_id) else {
            continue;
        };
        for geometry_id in references(shape, "Geometry") {
            result.insert(geometry_id);
            if let Some(geometry) = state.resolve_reference(shape_id, "Geometry", geometry_id) {
                for property in ["TriangleMesh", "ConvexMesh", "HeightField"] {
                    result.extend(references(geometry, property));
                }
            }
        }
    }
    let mut parent = source;
    for _ in 0..64 {
        if !parent.type_name.ends_with("::PxArticulationLink") {
            break;
        }
        let Some(id) = references(parent, "Parent").first().copied() else {
            break;
        };
        if !result.insert(id) {
            break;
        }
        let Some(next) = state.resolve_reference(parent.id, "Parent", id) else {
            break;
        };
        parent = next;
    }
    result
}

impl SceneIndex {
    pub fn rebuild(
        state: &SceneState,
        scope: &crate::room_scope::RoomScope,
        objects: &[RenderObject],
        assets: HashMap<ObjectKey, AssetUse>,
        source: SourceResources,
        ready: bool,
    ) -> Self {
        let mut result = Self {
            ready,
            source,
            ..default()
        };
        for object in state
            .objects
            .values()
            .filter(|object| actor(object) && scope.contains(object.id))
        {
            result.reindex(state, object.id);
        }
        result.last_actor_count = result.dependencies.len();
        for (slot, object) in objects.iter().enumerate() {
            let key = (object.id, object.shape_id);
            result.slots.insert(key, slot);
            result.actors.entry(object.id).or_default().insert(key);
            if let Some(usage) = assets.get(&key) {
                result.add_assets(key, *usage);
            }
        }
        result
    }

    pub fn affected(
        &self,
        state: &SceneState,
        changed: HashSet<u64>,
        mut poses: HashSet<u64>,
    ) -> Option<(HashSet<u64>, HashSet<u64>)> {
        if !self.ready {
            return None;
        }
        let mut result = HashSet::new();
        for id in changed {
            let pose_only = poses.contains(&id);
            if let Some(object) = state.objects.get(&id) {
                if object.type_name.ends_with("::PxCloth") {
                    return None;
                }
                if actor(object) && !pose_only {
                    result.insert(id);
                }
            }
            if self.dependencies.contains_key(&id) && !pose_only {
                result.insert(id);
            }
            if let Some(actors) = self.dependents.get(&id) {
                result.extend(actors);
            }
        }
        poses.retain(|id| !result.contains(id));
        Some((result, poses))
    }

    pub fn shape_poses(&self, state: &SceneState, actors: &HashSet<u64>) -> Option<Vec<ShapePose>> {
        let mut result = Vec::with_capacity(actors.len());
        for &id in actors {
            let Some(keys) = self.actors.get(&id) else {
                continue;
            };
            let Value::Transform { position, rotation } =
                state.objects.get(&id)?.properties.get("GlobalPose")?
            else {
                return None;
            };
            for &key in keys {
                let shape = state.resolve_reference(id, "Shapes", key.1)?;
                let (position, rotation) =
                    pvd_core::rigid_shape_pose((*position, *rotation), shape);
                if !valid_transform(position, rotation) {
                    return None;
                }
                result.push(ShapePose {
                    slot: *self.slots.get(&key)?,
                    position,
                    rotation,
                });
            }
        }
        Some(result)
    }

    pub fn accepts(
        &self,
        actors: &HashSet<u64>,
        scene: &pvd_core::RenderScene,
        shared: &HashMap<SharedKey, SharedGeometry>,
    ) -> bool {
        scene.diagnostics.is_empty()
            && self.source.replacement_fits(actors, &scene.objects)
            && scene.objects.iter().all(|object| {
                let geometry_valid = shared_key(&object.geometry)
                    .and_then(|key| shared.get(&key))
                    .map_or_else(|| valid_geometry(&object.geometry), |entry| entry.valid);
                valid_pose(object) && geometry_valid
            })
    }

    fn reindex(&mut self, state: &SceneState, actor_id: u64) {
        if let Some(previous) = self.dependencies.remove(&actor_id) {
            for id in previous {
                if let Some(actors) = self.dependents.get_mut(&id) {
                    actors.remove(&actor_id);
                    if actors.is_empty() {
                        self.dependents.remove(&id);
                    }
                }
            }
        }
        if state.objects.get(&actor_id).is_some_and(actor) {
            let dependencies = dependencies(state, actor_id);
            for id in &dependencies {
                self.dependents.entry(*id).or_default().insert(actor_id);
            }
            self.dependencies.insert(actor_id, dependencies);
        }
    }

    fn add_assets(&mut self, key: ObjectKey, usage: AssetUse) {
        *self.mesh_users.entry(usage.mesh).or_default() += 1;
        *self.material_users.entry(usage.material).or_default() += 1;
        self.assets.insert(key, usage);
    }

    fn remove_assets(&mut self, key: ObjectKey) {
        if let Some(usage) = self.assets.remove(&key) {
            release(&mut self.mesh_users, usage.mesh);
            release(&mut self.material_users, usage.material);
        }
    }
}

fn release<K: std::hash::Hash + Eq>(users: &mut HashMap<K, usize>, key: K) {
    if let Some(count) = users.get_mut(&key) {
        *count -= 1;
        if *count == 0 {
            users.remove(&key);
        }
    }
}

pub(super) fn shared_key(geometry: &Geometry) -> Option<SharedKey> {
    if let Geometry::TriangleMesh { vertices, indices } = geometry {
        Some((
            std::sync::Arc::as_ptr(vertices) as usize,
            std::sync::Arc::as_ptr(indices) as usize,
        ))
    } else {
        None
    }
}

fn geometry_bytes(geometry: &Geometry) -> usize {
    if let Geometry::TriangleMesh { vertices, indices } = geometry {
        vertices.len() * std::mem::size_of::<[f32; 3]>()
            + indices.len() * std::mem::size_of::<u32>()
    } else {
        0
    }
}

pub(super) fn sync_actors(
    commands: &mut Commands,
    studio: &Studio,
    viewport: &mut Viewport,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    actors: HashSet<u64>,
    objects: Vec<RenderObject>,
) {
    let mut index = std::mem::take(&mut viewport.incremental);
    index.last_actor_count = actors.len();
    let mut removed = HashSet::new();
    for actor in &actors {
        index.source.remove(*actor);
        if let Some(keys) = index.actors.remove(actor) {
            removed.extend(keys);
        }
        index.reindex(&studio.state, *actor);
    }
    for object in objects {
        index.source.add(&object);
        if let Some(shared) = shared_key(&object.geometry)
            && let Geometry::TriangleMesh { vertices, indices } = &object.geometry
        {
            viewport
                .shared_geometry
                .entry(shared)
                .or_insert_with(|| SharedGeometry {
                    vertices: vertices.clone(),
                    indices: indices.clone(),
                    hash: geometry_hash(&object.geometry),
                    valid: true,
                    focus_radius: Default::default(),
                });
        }
        if !object_visible(studio, &object) {
            continue;
        }
        let key = (object.id, object.shape_id);
        if let Some(usage) = sync_object(commands, studio, viewport, meshes, materials, &object) {
            index.remove_assets(key);
            index.add_assets(key, usage);
        }
        removed.remove(&key);
        index.actors.entry(object.id).or_default().insert(key);
        if let Some(slot) = index.slots.get(&key).copied() {
            viewport.objects[slot] = object;
        } else {
            index.slots.insert(key, viewport.objects.len());
            viewport.objects.push(object);
        }
    }
    for key in removed {
        index.remove_assets(key);
        if let Some(object) = viewport.rendered.remove(&key) {
            commands.entity(object.entity).despawn();
        }
        if let Some(slot) = index.slots.remove(&key) {
            viewport.objects.swap_remove(slot);
            if let Some(moved) = viewport.objects.get(slot) {
                index.slots.insert((moved.id, moved.shape_id), slot);
            }
        }
    }
    viewport.meshes.retain(|key, mesh| {
        if index.mesh_users.contains_key(key) {
            true
        } else {
            meshes.remove(mesh.id());
            false
        }
    });
    viewport.materials.retain(|key, material| {
        if index.material_users.contains_key(key) {
            true
        } else {
            materials.remove(material.id());
            false
        }
    });
    viewport.wires.retain(|id, _| meshes.contains(*id));
    viewport
        .shared_geometry
        // Source users include hidden actors; their Arcs keep pointer keys valid.
        .retain(|key, _| index.source.shared.contains_key(key));
    viewport.incremental = index;
}

pub(super) fn sync_poses(commands: &mut Commands, viewport: &mut Viewport, poses: Vec<ShapePose>) {
    viewport.incremental.last_pose_count = poses.len();
    for pose in poses {
        let object = &mut viewport.objects[pose.slot];
        object.position = pose.position;
        object.rotation = pose.rotation;
        if let Some(rendered) = viewport.rendered.get_mut(&(object.id, object.shape_id)) {
            let transform = object_transform(viewport.coordinates, object);
            if rendered.transform != transform {
                rendered.transform = transform;
                commands.entity(rendered.entity).insert(transform);
            }
        }
    }
}

#[cfg(test)]
mod filter_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
    use std::sync::Arc;

    pub(super) fn app() -> App {
        let mut app = App::new();
        app.insert_resource(Studio::default())
            .insert_resource(Viewport::default())
            .insert_resource(Assets::<Mesh>::default())
            .insert_resource(Assets::<StandardMaterial>::default())
            .add_systems(Update, sync_scene);
        app
    }

    pub(super) fn name(handle: u32) -> Name {
        Name {
            namespace: 41,
            name: handle,
        }
    }
    pub(super) fn create(id: u64, class: u32) -> Event {
        Event::CreateInstance {
            class: name(class),
            id,
        }
    }
    pub(super) fn set(id: u64, property: u32, datatype: u32, data: Vec<u8>) -> Event {
        Event::SetPropertyValue {
            id,
            property,
            datatype: name(datatype),
            items: 1,
            data: Arc::from(data),
        }
    }
    pub(super) fn reference(id: u64, property: u32, target: u64) -> Event {
        set(id, property, 30, target.to_le_bytes().to_vec())
    }
    pub(super) fn pose(id: u64, property: u32, x: f32) -> Event {
        set(
            id,
            property,
            31,
            [0.0_f32, 0.0, 0.0, 1.0, x, 0.0, 0.0]
                .iter()
                .flat_map(|n| n.to_le_bytes())
                .collect(),
        )
    }
    pub(super) fn frame(
        builder: &mut pvd_core::RecordingBuilder,
        index: usize,
        mut events: Vec<Event>,
    ) -> SceneState {
        events.push(Event::EndSection {
            id: 100,
            name: 40,
            timestamp: index as u64,
        });
        builder
            .push(EventGroup {
                stream_id: 1,
                timestamp: 0,
                offset: 0,
                events,
            })
            .unwrap();
        builder.snapshot().unwrap().state_at(index).unwrap()
    }
    pub(super) fn fixture() -> (pvd_core::RecordingBuilder, SceneState) {
        let mut builder = pvd_core::RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        let mut events: Vec<_> = [
            (1, "PxRigidDynamic"),
            (2, "PxRigidStatic"),
            (3, "PxShape"),
            (4, "PxSphereGeometry"),
            (5, "PxArticulationLink"),
            (6, "PxArticulation"),
            (20, "Shapes"),
            (21, "Geometry"),
            (22, "Radius"),
            (23, "GlobalPose"),
            (24, "LocalPose"),
            (25, "Parent"),
            (26, "IsSleeping"),
            (27, "RigidBodyFlags"),
            (30, "ObjectRef"),
            (31, "PxTransform"),
            (32, "PvdF32"),
            (33, "PvdBool"),
            (34, "PvdU32"),
            (40, "frame"),
            (41, "physx"),
        ]
        .into_iter()
        .map(|(handle, value)| Event::StringHandle {
            handle,
            value: value.into(),
        })
        .collect();
        events.extend((1..=6).map(|handle| Event::CreateClass { name: name(handle) }));
        events.extend([
            create(1, 1),
            create(2, 1),
            create(3, 2),
            create(4, 5),
            create(50, 6),
            create(10, 3),
            create(11, 3),
            create(20, 4),
            create(21, 4),
        ]);
        events.extend([
            reference(1, 20, 10),
            reference(2, 20, 10),
            reference(3, 20, 11),
            reference(4, 20, 10),
            reference(4, 25, 50),
            reference(10, 21, 20),
            reference(11, 21, 21),
        ]);
        events.extend([
            set(20, 22, 32, 1f32.to_le_bytes().to_vec()),
            set(21, 22, 32, 2f32.to_le_bytes().to_vec()),
        ]);
        let state = frame(&mut builder, 0, events);
        assert_eq!(state.render_scene().objects.len(), 4);
        (builder, state)
    }

    pub(super) fn objects(app: &App) -> Vec<RenderObject> {
        let mut objects = app.world().resource::<Viewport>().objects.clone();
        objects.sort_by_key(|object| (object.id, object.shape_id));
        objects
    }

    pub(super) fn update(app: &mut App, state: SceneState, changed: &[u64]) {
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            let revision = studio.revision;
            studio.scene_changes = SceneChanges {
                range: Some((revision, revision + 1)),
                objects: changed.iter().copied().collect(),
                ..Default::default()
            };
            studio.state = state;
            studio.revision += 1;
        }
        app.update();
    }

    #[test]
    fn replay_motion_reuses_render_assets_and_checks_shape_changes() {
        let (_, mut initial) = fixture();
        let local_pose = |x| Value::Transform {
            position: [x, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
        };
        initial
            .objects
            .get_mut(&10)
            .unwrap()
            .properties
            .insert("LocalPose".into(), local_pose(1.0));
        let mut app = app();
        app.world_mut().resource_mut::<Studio>().state = initial.clone();
        app.update();
        let rendered = &app.world().resource::<Viewport>().rendered[&(1, 10)];
        let (entity, mesh) = (rendered.entity, rendered.mesh.id());
        let mut expected = initial;
        let commit = |app: &mut App, next: &SceneState, changed: &[u64]| {
            let mut patch = SceneUpdate::full(next);
            patch.replace_all = false;
            {
                let studio = &mut *app.world_mut().resource_mut::<Studio>();
                studio
                    .scene_changes
                    .record_replay(&patch, &studio.state, studio.revision);
                studio
                    .scene_changes
                    .objects
                    .retain(|id| changed.contains(id));
                studio.scene_changes.poses.retain(|id| changed.contains(id));
                studio.state = next.clone();
                studio.revision += 1;
            }
            app.update();
            let mut full = next.render_scene().objects;
            full.sort_by_key(|object| (object.id, object.shape_id));
            assert_eq!(objects(app), full);
        };
        expected.objects.get_mut(&1).unwrap().properties.insert(
            "GlobalPose".into(),
            Value::Transform {
                position: [2.0, 3.0, 4.0],
                rotation: [0.0, 0.0, 1.0, 0.0],
            },
        );
        commit(&mut app, &expected, &[1]);
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(viewport.incremental.last_pose_count, 1);
        assert_eq!(viewport.rendered[&(1, 10)].entity, entity);
        assert_eq!(viewport.rendered[&(1, 10)].mesh.id(), mesh);
        expected
            .objects
            .get_mut(&1)
            .unwrap()
            .properties
            .insert("GlobalPose".into(), local_pose(2.0));
        expected
            .objects
            .get_mut(&10)
            .unwrap()
            .properties
            .insert("LocalPose".into(), local_pose(3.0));
        commit(&mut app, &expected, &[1, 10]);
        assert_eq!(
            app.world()
                .resource::<Viewport>()
                .incremental
                .last_pose_count,
            0
        );
    }

    #[test]
    fn overlay_edges_follow_display_demand_without_replacing_surface_assets() {
        let (_, initial) = fixture();
        let mut app = app();
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.state = initial;
        studio.prefs.show_bounds = false;
        studio.prefs.wireframe = false;
        app.update();
        let viewport = app.world().resource::<Viewport>();
        let selected_mesh = viewport.rendered[&(3, 11)].mesh.id();
        let shared_mesh = viewport.rendered[&(1, 10)].mesh.id();
        assert_ne!(selected_mesh, shared_mesh);
        assert!(viewport.wires.is_empty());

        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.select_object(Some(3), false);
        studio.prefs.bounds_scope = crate::studio::ObjectScope::Selected;
        studio.prefs.show_bounds = true;
        app.update();
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(viewport.wires.len(), 1);
        assert!(!viewport.wires[&selected_mesh].is_empty());

        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.wireframe = true;
        app.update();
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(viewport.wires.len(), 2);
        let shared_edges = &viewport.wires[&shared_mesh];
        let mut mesh = app
            .world()
            .resource::<Assets<Mesh>>()
            .get(shared_mesh)
            .unwrap()
            .clone();
        orient_winding(&mut mesh, true);
        assert_eq!(*shared_edges, mesh_edges(&mesh, true));

        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio.prefs.wireframe = false;
        studio.prefs.show_bounds = false;
        app.update();
        let viewport = app.world().resource::<Viewport>();
        assert!(viewport.wires.is_empty());
        assert_eq!(viewport.rendered[&(1, 10)].mesh.id(), shared_mesh);
        assert_eq!(viewport.rendered[&(3, 11)].mesh.id(), selected_mesh);
    }

    #[test]
    fn dependencies_lifetimes_and_filters_match_full_rendering() {
        let (mut builder, initial) = fixture();
        let mut app = app();
        update(&mut app, initial.clone(), &[]);
        let static_entity = app.world().resource::<Viewport>().rendered[&(3, 11)].entity;
        let scenarios = [
            (vec![pose(1, 23, 4.0)], vec![1], 1),
            (vec![pose(10, 24, 3.0)], vec![10], 3),
            (
                vec![set(20, 22, 32, 3f32.to_le_bytes().to_vec())],
                vec![20],
                3,
            ),
            (vec![set(50, 26, 33, vec![1])], vec![50], 1),
            (vec![Event::DestroyInstance { id: 10 }], vec![10], 3),
            (vec![create(10, 3), reference(10, 21, 20)], vec![10], 3),
            (vec![reference(2, 20, 10)], vec![2], 1),
            (vec![Event::DestroyInstance { id: 20 }], vec![20], 1),
            (
                vec![create(20, 4), set(20, 22, 32, 7f32.to_le_bytes().to_vec())],
                vec![20],
                1,
            ),
            (vec![reference(10, 21, 20)], vec![10], 3),
        ];
        for (index, (events, changed, affected)) in scenarios.into_iter().enumerate() {
            let state = frame(&mut builder, index + 1, events);
            let mut expected = state.render_scene().objects;
            expected.sort_by_key(|object| (object.id, object.shape_id));
            update(&mut app, state, &changed);
            assert_eq!(objects(&app), expected, "frame {}", index + 1);
            let viewport = app.world().resource::<Viewport>();
            assert_eq!(
                viewport.incremental.last_actor_count,
                affected,
                "frame {}",
                index + 1
            );
            assert_eq!(viewport.rendered[&(3, 11)].entity, static_entity);
            assert_eq!(viewport.rendered.len(), viewport.objects.len());
        }
        app.world_mut().resource_mut::<Studio>().prefs.show_static = false;
        app.update();
        assert!(objects(&app).iter().all(|object| object.id != 3));
        app.world_mut().resource_mut::<Studio>().prefs.show_static = true;
        app.update();
        assert!(objects(&app).iter().any(|object| object.id == 3));
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.state = initial;
            studio.revision += 1;
        }
        app.update();
        assert_eq!(objects(&app).len(), 4);
        assert_eq!(
            app.world()
                .resource::<Viewport>()
                .incremental
                .last_actor_count,
            4
        );
    }

    #[test]
    fn change_ranges_reject_missing_full_and_replaced_revisions() {
        let mut changes = SceneChanges::default();
        changes.record(&SceneUpdate::default(), 4);
        changes.record(&SceneUpdate::default(), 5);
        assert!(changes.take(4, 6).is_some());
        changes.record(&SceneUpdate::default(), 6);
        changes.record(&SceneUpdate::default(), 8);
        assert!(changes.take(6, 9).is_none());
        changes.record(&SceneUpdate::full(&SceneState::default()), 9);
        assert!(changes.take(9, 10).is_none());
        changes.record(&SceneUpdate::default(), 10);
        assert!(changes.take(9, 11).is_none());
    }

    #[test]
    #[ignore = "Requires an official SDK capture; CPU scene sync and state comparison, no GPU/window"]
    fn real_capture_incremental_sync_matches_full_scene() {
        use std::time::Instant;
        let path = std::env::var_os("PVD_SCENE_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4-cct-large.pvdstream")
            });
        let bytes = std::fs::read(&path).unwrap();
        let mut live =
            pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig::default())
                .unwrap();
        let mut app = app();
        let mut timings = Vec::new();
        let mut full_times = Vec::new();
        let mut affected = 0usize;
        let mut updates = 0usize;
        for chunk in bytes.chunks(64 * 1024) {
            live.feed(chunk).unwrap();
            if let Some((frame, patch)) = live.take_update() {
                let from = app.world().resource::<Viewport>().revision;
                {
                    let mut studio = app.world_mut().resource_mut::<Studio>();
                    let revision = studio.revision;
                    studio.scene_changes.record(&patch, revision);
                    patch.apply(&mut studio.state);
                    studio.revision += 1;
                }
                let start = Instant::now();
                app.update();
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                let start = Instant::now();
                let mut expected = app
                    .world()
                    .resource::<Studio>()
                    .state
                    .render_scene()
                    .objects;
                full_times.push(start.elapsed().as_secs_f64() * 1000.0);
                expected.sort_by_key(|object| (object.id, object.shape_id));
                assert_eq!(objects(&app), expected, "frame {frame}");
                let viewport = app.world().resource::<Viewport>();
                if from != u64::MAX {
                    timings.push(elapsed);
                    affected += viewport.incremental.last_actor_count;
                }
                updates += 1;
            }
        }
        assert!(updates > 5);
        let viewport = app.world().resource::<Viewport>();
        let average = |times: &[f64]| times.iter().sum::<f64>() / times.len() as f64;
        eprintln!(
            "CPU-only incremental scene sync: source={} updates={} shapes={} avg_ms={:.3} max_ms={:.3} avg_affected_actors={:.1} full_core_scene_avg_ms={:.3}",
            path.display(),
            updates,
            viewport.objects.len(),
            average(&timings),
            timings.iter().copied().fold(0.0, f64::max),
            affected as f64 / timings.len() as f64,
            average(&full_times)
        );
    }
}
