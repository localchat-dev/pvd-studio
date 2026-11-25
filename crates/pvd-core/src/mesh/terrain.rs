use super::{ArraySource, MeshKey, MeshKind, SceneState};
use crate::Geometry;
use crate::render_region::{LocalBounds, ShapeRegion};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

const MIN_TRIANGLES: usize = 512;
const LEAF_TRIANGLES: usize = 32;
const RECENT_SELECTIONS: usize = 8;
const VERTEX_BYTES: usize = size_of::<[f32; 3]>();
const INDEX_BYTES: usize = size_of::<u32>();

pub(crate) type FrameTerrainSelections = BTreeMap<MeshKey, ShapeSelections>;
type ShapeSelections = BTreeMap<Arc<[usize]>, Geometry>;

pub(super) struct TerrainSource {
    points: ArraySource,
    indices: Option<ArraySource>,
    dimensions: [usize; 2],
    reverse_winding: bool,
}

impl TerrainSource {
    pub(super) fn capture(state: &SceneState, key: &MeshKey) -> Option<Self> {
        let (points, dimensions) = key.bounds_source(state)?;
        let resource = state.objects.get(&key.resource)?;
        let (indices, triangle_count, reverse_winding) = match key.kind {
            MeshKind::Triangle => {
                let indices = resource.properties.get("Triangles")?;
                (
                    Some(ArraySource::capture(indices)?),
                    indices.array_len()? / 3,
                    false,
                )
            }
            MeshKind::HeightField => {
                let [rows, columns] = dimensions;
                let reverse_winding = state.height_field_thickness(resource)? > 0.0;
                (None, (rows - 1) * (columns - 1) * 2, reverse_winding)
            }
            MeshKind::Convex => return None,
        };
        if triangle_count < MIN_TRIANGLES && points.array_len()? < MIN_TRIANGLES * 3 {
            return None;
        }
        Some(Self {
            points: ArraySource::capture(points)?,
            indices,
            dimensions,
            reverse_winding,
        })
    }

    fn matches(&self, other: &Self) -> bool {
        self.points.ptr_eq(&other.points)
            && self.dimensions == other.dimensions
            && self.reverse_winding == other.reverse_winding
            && match (&self.indices, &other.indices) {
                (Some(a), Some(b)) => a.ptr_eq(b),
                (None, None) => true,
                _ => false,
            }
    }
}

pub(crate) enum RegionMesh {
    Full,
    Outside,
    Visible(Geometry),
    Invalid,
    BudgetExceeded,
}

#[derive(Default)]
pub(super) struct TerrainCache {
    entries: BTreeMap<MeshKey, IndexedTerrain>,
    bytes: usize,
    clock: u64,
}

impl TerrainCache {
    pub(super) fn retained_bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn retain_resources(&mut self, state: &SceneState) {
        self.entries.retain(|key, entry| {
            let keep = TerrainSource::capture(state, key)
                .is_some_and(|source| entry.source.matches(&source));
            if !keep {
                self.bytes -= entry.retained_bytes();
            }
            keep
        });
    }

    pub(super) fn take(&mut self, key: &MeshKey, source: &TerrainSource) -> Option<IndexedTerrain> {
        let entry = self.remove(key)?;
        entry.source.matches(source).then_some(entry)
    }

    pub(super) fn insert(&mut self, key: MeshKey, mut entry: IndexedTerrain, limit: usize) {
        if entry.index_bytes() > limit {
            return;
        }
        while entry.retained_bytes() > limit {
            entry.selections.pop_front();
        }
        let bytes = entry.retained_bytes();
        while bytes > limit.saturating_sub(self.bytes) {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
                .expect("the terrain cache holds the remaining bytes");
            self.remove(&oldest);
        }
        self.clock = self.clock.wrapping_add(1);
        entry.last_used = self.clock;
        self.bytes += bytes;
        self.entries.insert(key, entry);
    }

    fn remove(&mut self, key: &MeshKey) -> Option<IndexedTerrain> {
        let entry = self.entries.remove(key)?;
        self.bytes -= entry.retained_bytes();
        Some(entry)
    }
}

struct Selection {
    leaves: Arc<[usize]>,
    geometry: Geometry,
    bytes: usize,
}

pub(super) struct IndexedTerrain {
    source: TerrainSource,
    vertices: Arc<Vec<[f32; 3]>>,
    indices: Arc<Vec<u32>>,
    faces: Vec<u32>,
    nodes: Vec<Node>,
    selections: VecDeque<Selection>,
    last_used: u64,
}

struct Node {
    bounds: LocalBounds,
    triangles: Range<usize>,
    right: usize,
}

struct Face {
    bounds: LocalBounds,
    midpoint: [f32; 3],
    index: u32,
}

impl IndexedTerrain {
    pub(super) fn new(source: TerrainSource, geometry: Geometry) -> Option<Self> {
        let Geometry::TriangleMesh { vertices, indices } = geometry else {
            return None;
        };
        let mut faces = indices
            .chunks_exact(3)
            .enumerate()
            .map(|(index, triangle)| {
                let bounds = LocalBounds::from_points(
                    triangle.iter().map(|&i| vertices.get(i as usize).copied()),
                )?;
                let (min, max) = bounds.extents();
                Some(Face {
                    bounds,
                    midpoint: std::array::from_fn(|axis| min[axis] * 0.5 + max[axis] * 0.5),
                    index: index as u32,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let mut nodes = Vec::new();
        if !faces.is_empty() {
            build_nodes(&mut faces, 0, &mut nodes);
        }
        Some(Self {
            source,
            vertices,
            indices,
            faces: faces.into_iter().map(|face| face.index).collect(),
            nodes,
            selections: VecDeque::new(),
            last_used: 0,
        })
    }

    pub(super) fn index_bytes(&self) -> usize {
        size_of::<MeshKey>()
            + size_of::<Self>()
            + self.vertices.capacity() * VERTEX_BYTES
            + self.indices.capacity() * INDEX_BYTES
            + self.faces.capacity() * size_of::<u32>()
            + self.nodes.capacity() * size_of::<Node>()
            + self.selections.capacity() * size_of::<Selection>()
    }

    fn retained_bytes(&self) -> usize {
        self.index_bytes()
            + self
                .selections
                .iter()
                .map(|selection| selection.bytes)
                .sum::<usize>()
    }

    pub(super) fn select(
        &mut self,
        region: ShapeRegion,
        max_bytes: usize,
        frame: &mut ShapeSelections,
    ) -> RegionMesh {
        let mut leaves = Vec::new();
        if !self.nodes.is_empty() {
            self.intersecting_leaves(0, region, &mut leaves);
        }
        if leaves.is_empty() {
            return RegionMesh::Outside;
        }
        // A frame may see more variants than the persistent cache holds.
        if let Some(geometry) = frame.get(leaves.as_slice()) {
            return RegionMesh::Visible(geometry.clone());
        }
        if let Some(index) = self
            .selections
            .iter()
            .position(|entry| entry.leaves.as_ref() == leaves.as_slice())
        {
            let entry = self
                .selections
                .remove(index)
                .expect("the selected terrain is cached");
            let geometry = entry.geometry.clone();
            frame.insert(entry.leaves.clone(), geometry.clone());
            self.selections.push_back(entry);
            return RegionMesh::Visible(geometry);
        }
        let index_count = leaves
            .iter()
            .map(|&index| self.nodes[index].triangles.len() * 3)
            .sum();
        let Some(geometry) = self.compact_geometry(&leaves, index_count, max_bytes) else {
            return RegionMesh::BudgetExceeded;
        };
        let Geometry::TriangleMesh { vertices, indices } = &geometry else {
            unreachable!("terrain is a triangle mesh");
        };
        let bytes = vertices.capacity() * VERTEX_BYTES
            + indices.capacity() * INDEX_BYTES
            + leaves.len() * size_of::<usize>();
        let leaves: Arc<[usize]> = leaves.into();
        if self.selections.len() == RECENT_SELECTIONS {
            self.selections.pop_front();
        }
        self.selections.push_back(Selection {
            leaves: leaves.clone(),
            geometry: geometry.clone(),
            bytes,
        });
        frame.insert(leaves, geometry.clone());
        RegionMesh::Visible(geometry)
    }

    fn intersecting_leaves(&self, index: usize, region: ShapeRegion, leaves: &mut Vec<usize>) {
        let node = &self.nodes[index];
        if !region.intersects_bounds(node.bounds) {
            return;
        }
        if node.right == 0 {
            leaves.push(index);
        } else {
            self.intersecting_leaves(index + 1, region, leaves);
            self.intersecting_leaves(node.right, region, leaves);
        }
    }

    fn compact_geometry(
        &self,
        leaves: &[usize],
        index_count: usize,
        max_bytes: usize,
    ) -> Option<Geometry> {
        let vertex_limit = max_bytes.checked_sub(index_count * INDEX_BYTES)? / VERTEX_BYTES;
        let mut remap = HashMap::new();
        let mut vertices = Vec::new();
        let mut indices = Vec::with_capacity(index_count);
        for &leaf in leaves {
            for &face in &self.faces[self.nodes[leaf].triangles.clone()] {
                let first = face as usize * 3;
                for &index in &self.indices[first..first + 3] {
                    let local = match remap.entry(index) {
                        std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            if vertices.len() == vertex_limit {
                                return None;
                            }
                            let local = vertices.len() as u32;
                            vertices.push(self.vertices[index as usize]);
                            *entry.insert(local)
                        }
                    };
                    indices.push(local);
                }
            }
        }
        vertices.shrink_to_fit();
        Some(Geometry::TriangleMesh {
            vertices: vertices.into(),
            indices: indices.into(),
        })
    }
}

fn build_nodes(faces: &mut [Face], start: usize, nodes: &mut Vec<Node>) -> usize {
    let index = nodes.len();
    nodes.push(Node {
        bounds: faces[0].bounds,
        triangles: start..start + faces.len(),
        right: 0,
    });
    if faces.len() > LEAF_TRIANGLES {
        let (min, max) = LocalBounds::from_points(faces.iter().map(|face| Some(face.midpoint)))
            .expect("triangle bounds give finite midpoints")
            .extents();
        let axis = (0..3)
            .max_by(|&a, &b| {
                (f64::from(max[a]) - f64::from(min[a]))
                    .total_cmp(&(f64::from(max[b]) - f64::from(min[b])))
            })
            .unwrap();
        let middle = faces.len() / 2;
        faces.select_nth_unstable_by(middle, |a, b| {
            a.midpoint[axis]
                .total_cmp(&b.midpoint[axis])
                .then(a.index.cmp(&b.index))
        });
        let (left, right) = faces.split_at_mut(middle);
        let left = build_nodes(left, start, nodes);
        let right = build_nodes(right, start + middle, nodes);
        nodes[index].right = right;
        nodes[index].bounds = nodes[left].bounds.union(nodes[right].bounds);
    } else {
        nodes[index].bounds = faces[1..]
            .iter()
            .fold(faces[0].bounds, |bounds, face| bounds.union(face.bounds));
    }
    index
}
