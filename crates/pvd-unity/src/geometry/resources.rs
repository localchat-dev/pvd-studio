use super::*;
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Weak,
};

const MAX_RESOURCE_BYTES: usize = 128 * 1024 * 1024;
const HEADER_BYTES: usize = 28;
const RAY_HEADER_BYTES: usize = 8;
const RAY_RECORD_BYTES: usize = 1 + 9 * 4;
const MAX_RAYS: usize = 2048;
const SHAPE_HEADER_BYTES: usize = 8;
const SHAPE_RECORD_BYTES: usize = 1 + 6 * 4;
const MAX_SHAPE_LINES: usize = 8192;
// Very long queries would otherwise span the full Scene view.
const FULL_VIEW_QUERY_LENGTH: f32 = 10_000.0;

#[derive(Default)]
pub struct ResourceCache {
    scope: Option<(u64, u64)>,
    token: u64,
    last_id: u64,
    meshes: MeshTable,
}

#[derive(Default)]
struct MeshTable {
    by_id: HashMap<u64, Geometry>,
    by_content: HashMap<u64, Vec<u64>>,
    allocations: HashMap<(usize, usize), Alias>,
    bytes: usize,
}

struct Alias {
    id: u64,
    hash: u64,
    // Weak references prevent address reuse without retaining equal mesh payloads.
    _vertices: Weak<Vec<[f32; 3]>>,
    _indices: Weak<Vec<u32>>,
}

fn fingerprint(vertices: &[[f32; 3]], indices: &[u32]) -> u64 {
    let mut hash = DefaultHasher::new();
    vertices.len().hash(&mut hash);
    for v in vertices.iter().flatten() {
        v.to_bits().hash(&mut hash);
    }
    indices.hash(&mut hash);
    hash.finish()
}

impl MeshTable {
    fn matching_id(&self, hash: u64, geometry: &Geometry) -> Option<u64> {
        self.by_content
            .get(&hash)?
            .iter()
            .copied()
            .find(|id| same_mesh(&self.by_id[id], geometry))
    }

    fn retain(&mut self, id: u64, hash: u64, mesh: &Geometry, size: usize) -> Result<(), String> {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.by_id.entry(id) {
            if size > MAX_RESOURCE_BYTES - self.bytes {
                return Err("Scene meshes exceed the 128 MiB retained resource limit".into());
            }
            self.bytes += size;
            entry.insert(mesh.clone());
            self.by_content.entry(hash).or_default().push(id);
        }
        Ok(())
    }
}

fn packet_len(size: usize) -> Result<usize, String> {
    if size > MAX_BYTES {
        Err("Scene surfaces exceed the 32 MiB frame limit".into())
    } else {
        Ok(size)
    }
}

impl ResourceCache {
    /// The caller publishes the returned cache only if this frame job still owns the view.
    pub fn prepare(
        &self,
        state: &SceneState,
        scene: RenderScene,
        rooms: &BTreeMap<u64, u64>,
        scope: (u64, u64),
        base: u64,
    ) -> Result<(Vec<u8>, Self), String> {
        if let Some(reason) = scene
            .diagnostics
            .iter()
            .find(|s| s.starts_with("Scene display stopped at "))
        {
            return Err(reason.clone());
        }
        let previous =
            (base != 0 && self.scope == Some(scope) && base == self.token).then_some(&self.meshes);
        let mut last_id = if previous.is_some() { self.last_id } else { 0 };
        let mut next = MeshTable::default();
        let mut definitions = Vec::new();
        let mut size = packet_len(
            HEADER_BYTES.saturating_add(scene.objects.len().saturating_mul(RECORD_BYTES)),
        )?;
        for object in &scene.objects {
            let (kind, payload) = payload_layout(&object.geometry);
            size = packet_len(size + if kind == 4 { 8 } else { payload })?;
        }
        for object in &scene.objects {
            let geometry = &object.geometry;
            let Geometry::TriangleMesh { vertices, indices } = geometry else {
                continue;
            };
            let address = (
                Arc::as_ptr(vertices) as usize,
                Arc::as_ptr(indices) as usize,
            );
            if next.allocations.contains_key(&address) {
                continue;
            }
            let (_, payload) = payload_layout(geometry);
            // A single definition larger than a packet could never enter an acknowledged base.
            packet_len(
                HEADER_BYTES
                    .saturating_add(RECORD_BYTES + 16)
                    .saturating_add(payload),
            )?;
            let old = previous.and_then(|table| table.allocations.get(&address));
            let hash = old.map_or_else(|| fingerprint(vertices, indices), |alias| alias.hash);
            let matched = old
                .map(|alias| alias.id)
                .or_else(|| next.matching_id(hash, geometry))
                .or_else(|| previous.and_then(|table| table.matching_id(hash, geometry)));
            let id = if let Some(id) = matched {
                id
            } else {
                size = packet_len(size.saturating_add(8).saturating_add(payload))?;
                last_id = last_id.checked_add(1).ok_or("Mesh resource ID overflow")?;
                definitions.push(last_id);
                last_id
            };
            next.retain(id, hash, geometry, payload)?;
            next.allocations.insert(
                address,
                Alias {
                    id,
                    hash,
                    _vertices: Arc::downgrade(vertices),
                    _indices: Arc::downgrade(indices),
                },
            );
        }

        let token = next_token()?;
        let mut data = Vec::with_capacity(size);
        data.extend(b"PVS3");
        data.extend(token.to_le_bytes());
        data.extend(if previous.is_some() { base } else { 0 }.to_le_bytes());
        data.extend((definitions.len() as u32).to_le_bytes());
        for id in definitions {
            data.extend(id.to_le_bytes());
            write_payload(&mut data, &next.by_id[&id])?;
        }
        data.extend((scene.objects.len() as u32).to_le_bytes());
        for object in &scene.objects {
            let (room, actor, shape) = shape_identity(state, object, rooms)?;
            let (kind, _) = payload_layout(&object.geometry);
            write_prefix(&mut data, room, actor, shape, object, kind)?;
            if let Geometry::TriangleMesh { vertices, indices } = &object.geometry {
                let address = (
                    Arc::as_ptr(vertices) as usize,
                    Arc::as_ptr(indices) as usize,
                );
                data.extend(next.allocations[&address].id.to_le_bytes());
            } else {
                write_payload(&mut data, &object.geometry)?;
            }
        }
        debug_assert_eq!(data.len(), size);
        Ok((
            data,
            Self {
                scope: Some(scope),
                token,
                last_id,
                meshes: next,
            },
        ))
    }
}

pub fn append_rays(
    data: &mut Vec<u8>,
    state: &SceneState,
    rooms: &RoomDirectory,
    room: u64,
    region: Option<RenderRegion>,
) {
    let Some(available) = (MAX_BYTES - data.len()).checked_sub(RAY_HEADER_BYTES) else {
        // A shape packet that fits by itself remains readable as PVS3.
        return;
    };
    let scene = rooms
        .iter()
        .find(|entry| entry.number == room)
        .map(|entry| entry.scene);
    let limit = MAX_RAYS.min(available / RAY_RECORD_BYTES);
    let overlay = pvd_core::queries::collect_display_rays(
        state,
        scene,
        region,
        limit,
        FULL_VIEW_QUERY_LENGTH,
    );
    data[..4].copy_from_slice(b"PVS4");
    write_rays(data, overlay);
}

fn write_rays(data: &mut Vec<u8>, overlay: pvd_core::queries::DisplayRays) {
    data.reserve_exact(RAY_HEADER_BYTES + overlay.rays.len() * RAY_RECORD_BYTES);
    data.extend(u32::from(overlay.truncated).to_le_bytes());
    data.extend((overlay.rays.len() as u32).to_le_bytes());
    for ray in overlay.rays {
        data.push(u8::from(ray.has_hit) | (u8::from(ray.marker.is_some()) << 1));
        for value in ray
            .start
            .into_iter()
            .chain(ray.end)
            .chain(ray.marker.unwrap_or([0.0; 3]))
        {
            data.extend(value.to_le_bytes());
        }
    }
}

pub fn append_queries(
    data: &mut Vec<u8>,
    state: &SceneState,
    rooms: &RoomDirectory,
    room: u64,
    region: Option<RenderRegion>,
    include_rays: bool,
) {
    let Some(available) =
        (MAX_BYTES - data.len()).checked_sub(RAY_HEADER_BYTES + SHAPE_HEADER_BYTES)
    else {
        return;
    };
    let scenes = || {
        let fallback = (room == 0 && rooms.omitted_count() > 0).then(|| state.objects.values());
        let scoped = rooms
            .iter()
            .filter(move |entry| room == 0 || entry.number == room)
            .filter_map(|entry| {
                state
                    .objects
                    .get(&entry.scene.id)
                    .filter(|object| object.generation == entry.scene.generation)
            });
        scoped
            .take(if fallback.is_some() { 0 } else { rooms.len() })
            .chain(fallback.into_iter().flatten())
    };
    let rays = if include_rays {
        pvd_core::queries::collect_display_rays_in_scenes(
            scenes(),
            region,
            MAX_RAYS.min(available / RAY_RECORD_BYTES),
            FULL_VIEW_QUERY_LENGTH,
        )
    } else {
        Default::default()
    };
    let shape_budget = available - rays.rays.len() * RAY_RECORD_BYTES;
    let overlay = pvd_core::queries::collect_display_shapes(
        state,
        scenes(),
        region,
        MAX_SHAPE_LINES.min(shape_budget / SHAPE_RECORD_BYTES),
        FULL_VIEW_QUERY_LENGTH,
    );
    data.reserve_exact(
        RAY_HEADER_BYTES
            + rays.rays.len() * RAY_RECORD_BYTES
            + SHAPE_HEADER_BYTES
            + overlay.lines.len() * SHAPE_RECORD_BYTES,
    );
    data[..4].copy_from_slice(b"PVS5");
    write_rays(data, rays);
    data.extend(u32::from(overlay.truncated).to_le_bytes());
    data.extend((overlay.lines.len() as u32).to_le_bytes());
    for line in overlay.lines {
        data.push(line.style as u8);
        for value in line.start.into_iter().chain(line.end) {
            data.extend(value.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests;
