use std::collections::{BTreeMap, BTreeSet};

use crate::{
    MeshCache, Object, RenderRegion, SceneState, Value, quat_mul, refs, render_transform, rotate,
    scalar, vec3,
};

const MAX_ACTORS: usize = 250_000;
const MAX_DEPENDENCIES: usize = 1_000_000;
const MAX_ACTOR_DEPENDENCIES: usize = 4096;
const LEAF_ACTORS: usize = 8;

#[derive(Clone, Copy, PartialEq)]
struct Bounds {
    min: [f64; 3],
    max: [f64; 3],
}

impl Bounds {
    fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|axis| self.min[axis].min(other.min[axis])),
            max: std::array::from_fn(|axis| self.max[axis].max(other.max[axis])),
        }
    }

    fn centered(extents: [f64; 3]) -> Self {
        Self {
            min: extents.map(|extent| -extent),
            max: extents,
        }
    }

    fn from_shape(self, position: [f32; 3], rotation: [f32; 4]) -> Option<Self> {
        if position.iter().any(|coordinate| !coordinate.is_finite()) {
            return None;
        }
        // Match the exact region test's normalization of the combined shape rotation.
        let [x, y, z, w] = rotation.map(f64::from);
        let length = (x * x + y * y + z * z + w * w).sqrt();
        if !length.is_finite() || length == 0.0 {
            return None;
        }
        let [x, y, z, w] = [x, y, z, w].map(|component| component / length);
        let rows = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y - w * z),
                2.0 * (x * z + w * y),
            ],
            [
                2.0 * (x * y + w * z),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z - w * x),
            ],
            [
                2.0 * (x * z - w * y),
                2.0 * (y * z + w * x),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ];
        let scale = (0..3)
            .map(|axis| self.min[axis].abs().max(self.max[axis].abs()))
            .sum::<f64>();
        let mut bounds = Self {
            min: [0.; 3],
            max: [0.; 3],
        };
        for (axis, row) in rows.into_iter().enumerate() {
            let position = f64::from(position[axis]);
            // Expand outward for roundoff in the local/world transforms at large coordinates.
            let padding = 128.0 * f64::EPSILON * (position.abs() + scale);
            let mut low = position - padding;
            let mut high = position + padding;
            for (coordinate, factor) in row.into_iter().enumerate() {
                let a = factor * self.min[coordinate];
                let b = factor * self.max[coordinate];
                low += a.min(b);
                high += a.max(b);
            }
            bounds.min[axis] = low;
            bounds.max[axis] = high;
        }
        Some(bounds)
    }

    fn middle(self, axis: usize) -> f64 {
        (self.min[axis] + self.max[axis]) * 0.5
    }

    fn intersects(self, other: Self) -> bool {
        (0..3).all(|axis| self.min[axis] <= other.max[axis] && self.max[axis] >= other.min[axis])
    }
}

type IndexedActor = (u64, Bounds);

struct Node {
    bounds: Bounds,
    contents: NodeContents,
}

enum NodeContents {
    Actors(Vec<IndexedActor>),
    Children(Box<[Node; 2]>),
}

impl Node {
    fn build(actors: &mut [IndexedActor]) -> Option<Self> {
        let bounds = actors
            .iter()
            .map(|(_, bounds)| *bounds)
            .reduce(Bounds::union)?;
        let contents = if actors.len() <= LEAF_ACTORS {
            NodeContents::Actors(actors.to_vec())
        } else {
            let mut low = [f64::INFINITY; 3];
            let mut high = [f64::NEG_INFINITY; 3];
            for (_, actor) in actors.iter() {
                for axis in 0..3 {
                    let middle = actor.middle(axis);
                    low[axis] = low[axis].min(middle);
                    high[axis] = high[axis].max(middle);
                }
            }
            let axis = (0..3)
                .max_by(|&a, &b| (high[a] - low[a]).total_cmp(&(high[b] - low[b])))
                .unwrap();
            let median = actors.len() / 2;
            actors.select_nth_unstable_by(median, |a, b| {
                a.1.middle(axis).total_cmp(&b.1.middle(axis))
            });
            let (left, right) = actors.split_at_mut(median);
            NodeContents::Children(Box::new([Self::build(left)?, Self::build(right)?]))
        };
        Some(Self { bounds, contents })
    }

    fn collect(&self, query: Bounds, result: &mut Vec<u64>) {
        if !self.bounds.intersects(query) {
            return;
        }
        match &self.contents {
            NodeContents::Actors(actors) => {
                result.extend(
                    actors
                        .iter()
                        .filter(|(_, bounds)| bounds.intersects(query))
                        .map(|&(id, _)| id),
                );
            }
            NodeContents::Children(children) => {
                for child in children.iter() {
                    child.collect(query, result);
                }
            }
        }
    }
}

struct StaticActor {
    bounds: Option<Bounds>,
    dependencies: Vec<u64>,
}

fn is_static(object: &Object) -> bool {
    object.type_name.ends_with("::PxRigidStatic")
}

fn is_dynamic(object: &Object) -> bool {
    object.type_name.ends_with("::PxRigidDynamic")
        || object.type_name.ends_with("::PxArticulationLink")
}

#[derive(Default)]
struct Index {
    statics: BTreeMap<u64, StaticActor>,
    by_dependency: BTreeMap<u64, BTreeSet<u64>>,
    dependency_count: usize,
    unchecked: BTreeSet<u64>,
    dynamic: BTreeSet<u64>,
    tree: Option<Node>,
    tree_dirty: bool,
    meshes: MeshCache,
}

impl Index {
    fn update(&mut self, state: &SceneState, id: u64) -> bool {
        let object = state.objects.get(&id);
        if object.is_some_and(is_dynamic) && self.dynamic.contains(&id) {
            return true;
        }
        self.dynamic.remove(&id);
        let previous = self.statics.remove(&id);
        let previous_bounds = previous.as_ref().and_then(|actor| actor.bounds);
        if let Some(previous) = previous {
            self.dependency_count -= previous.dependencies.len();
            for dependency in previous.dependencies {
                self.by_dependency.entry(dependency).and_modify(|actors| {
                    actors.remove(&id);
                });
                if self
                    .by_dependency
                    .get(&dependency)
                    .is_some_and(BTreeSet::is_empty)
                {
                    self.by_dependency.remove(&dependency);
                }
            }
            self.unchecked.remove(&id);
        }
        let mut bounds = None;
        if let Some(actor) = object {
            if is_static(actor) {
                let next = static_actor(state, actor, &mut self.meshes);
                bounds = next.bounds;
                self.dependency_count += next.dependencies.len();
                if self.dependency_count > MAX_DEPENDENCIES {
                    return false;
                }
                if bounds.is_none() {
                    self.unchecked.insert(id);
                }
                for &dependency in &next.dependencies {
                    self.by_dependency.entry(dependency).or_default().insert(id);
                }
                self.statics.insert(id, next);
            } else if is_dynamic(actor) {
                self.dynamic.insert(id);
            }
        }
        self.tree_dirty |= bounds != previous_bounds;
        self.statics.len() + self.dynamic.len() <= MAX_ACTORS
    }

    fn candidates(&mut self, region: RenderRegion) -> Vec<u64> {
        if self.tree_dirty {
            let mut actors = self
                .statics
                .iter()
                .filter_map(|(&id, actor)| actor.bounds.map(|bounds| (id, bounds)))
                .collect::<Vec<_>>();
            self.tree = Node::build(&mut actors);
            self.tree_dirty = false;
        }
        let mut result = self
            .dynamic
            .iter()
            .chain(&self.unchecked)
            .copied()
            .collect::<Vec<_>>();
        let (center, radius) = region.bounds_parameters();
        let radius = f64::from(radius) * (1.0 + 16.0 * f64::EPSILON);
        let query = Bounds {
            min: center.map(|coordinate| f64::from(coordinate) - radius),
            max: center.map(|coordinate| f64::from(coordinate) + radius),
        };
        if let Some(tree) = &self.tree {
            tree.collect(query, &mut result);
        }
        // Preserve the scene's actor order for display limits and wire records.
        result.sort_unstable();
        result
    }
}

/// Broad phase for focused display. Actors with uncertain bounds remain candidates
/// for the normal shape test. Object changes must include captured reference bindings.
pub struct DisplaySpatialIndex(Option<Index>);

impl DisplaySpatialIndex {
    pub fn new(state: &SceneState) -> Self {
        let mut index = Index::default();
        for actor in state.objects.values() {
            if (is_static(actor) || is_dynamic(actor)) && !index.update(state, actor.id) {
                return Self(None);
            }
        }
        Self(Some(index))
    }

    /// Updates the supplied changed objects and static actors depending on them.
    /// Replace the index when the scene state is restored without an object delta.
    pub fn sync(&mut self, state: &SceneState, changed: &[u64]) {
        let Some(index) = self.0.as_mut() else { return };
        let mut affected = BTreeSet::new();
        for id in changed {
            if index.statics.contains_key(id)
                || index.dynamic.contains(id)
                || state
                    .objects
                    .get(id)
                    .is_some_and(|object| is_static(object) || is_dynamic(object))
            {
                affected.insert(*id);
            }
            if let Some(owners) = index.by_dependency.get(id) {
                affected.extend(owners);
            }
            if affected.len() > MAX_ACTORS {
                self.0 = None;
                return;
            }
        }
        for id in affected {
            if !index.update(state, id) {
                self.0 = None;
                return;
            }
        }
    }

    /// `None` means the caller should apply the original region test to the scene.
    pub fn candidates(&mut self, region: RenderRegion) -> Option<Vec<u64>> {
        self.0.as_mut().map(|index| index.candidates(region))
    }
}

fn static_actor(state: &SceneState, actor: &Object, cache: &mut MeshCache) -> StaticActor {
    let mut dependencies = Vec::new();
    let mut collect =
        |id| (dependencies.len() < MAX_ACTOR_DEPENDENCIES).then(|| dependencies.push(id));
    let bounds = static_bounds(state, actor, cache, &mut collect);
    dependencies.sort_unstable();
    dependencies.dedup();
    StaticActor {
        bounds,
        dependencies,
    }
}

fn static_bounds(
    state: &SceneState,
    actor: &Object,
    cache: &mut MeshCache,
    collect: &mut impl FnMut(u64) -> Option<()>,
) -> Option<Bounds> {
    let (position, rotation) = render_transform(actor.properties.get("GlobalPose"), true)?;
    let mut actor_bounds: Option<Bounds> = None;
    let mut uncertain = false;
    for shape_id in refs(actor.properties.get("Shapes")) {
        collect(shape_id)?;
        let Some(shape) = state.resolve_reference(actor.id, "Shapes", shape_id) else {
            uncertain = true;
            continue;
        };
        let Some(geometry_id) = refs(shape.properties.get("Geometry")).next() else {
            uncertain = true;
            continue;
        };
        collect(geometry_id)?;
        let Some(geometry) = state.resolve_reference(shape.id, "Geometry", geometry_id) else {
            uncertain = true;
            continue;
        };
        let bounds = geometry_bounds(state, geometry, cache, collect).and_then(|bounds| {
            let (local_position, local_rotation) =
                render_transform(shape.properties.get("LocalPose"), true)?;
            let offset = rotate(rotation, local_position);
            bounds.from_shape(
                std::array::from_fn(|axis| position[axis] + offset[axis]),
                quat_mul(rotation, local_rotation),
            )
        });
        if let Some(bounds) = bounds {
            actor_bounds = Some(actor_bounds.map_or(bounds, |previous| previous.union(bounds)));
        } else {
            uncertain = true;
        }
    }
    if uncertain { None } else { actor_bounds }
}

fn geometry_bounds(
    state: &SceneState,
    geometry: &Object,
    cache: &mut MeshCache,
    collect: &mut impl FnMut(u64) -> Option<()>,
) -> Option<Bounds> {
    let type_name = geometry.type_name.rsplit_once("::")?.1;
    let extents = match type_name {
        "PxBoxGeometry" => vec3(geometry.properties.get("HalfExtents"))?.map(f64::from),
        "PxSphereGeometry" => [f64::from(scalar(geometry.properties.get("Radius"))?); 3],
        "PxCapsuleGeometry" => {
            let radius = f64::from(scalar(geometry.properties.get("Radius"))?);
            let half_height = f64::from(scalar(geometry.properties.get("HalfHeight"))?);
            if half_height < 0.0 || radius < 0.0 {
                return None;
            }
            [radius + half_height, radius, radius]
        }
        "PxPlaneGeometry" => [0., 5., 5.],
        _ => {
            let property = match type_name {
                "PxConvexMeshGeometry" => "ConvexMesh",
                "PxTriangleMeshGeometry" => "TriangleMesh",
                "PxHeightFieldGeometry" => "HeightField",
                _ => return None,
            };
            let resource = refs(geometry.properties.get(property)).next()?;
            collect(resource)?;
            if property == "HeightField"
                && let Some(mesh) = state.resolve_reference(geometry.id, property, resource)
                && let Some(Value::Ref(physics)) = mesh.properties.get("Physics")
            {
                collect(*physics)?;
            }
            let (min, max) = cache
                .local_bounds(state, &state.mesh_key(geometry)?)?
                .extents();
            return Some(Bounds {
                min: min.map(f64::from),
                max: max.map(f64::from),
            });
        }
    };
    extents
        .iter()
        .all(|extent| extent.is_finite() && *extent >= 0.0)
        .then(|| Bounds::centered(extents))
}

#[cfg(test)]
mod tests;
