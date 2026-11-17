//! Static geometry from the Recast Demo/Detour NavMeshSet binary format.
//!
//! Supports MSET version 1, little endian, 32-bit tile references and Detour
//! tile version 7 with six vertices per polygon. Layouts are read explicitly,
//! never cast from Rust/C structures. TileCache/TSET is a different format.
//! See `navmesh/fixtures/README.md` for the independent compatibility fixture.

use std::{collections::HashSet, fmt};

pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TILES: usize = 65_536;
const MAX_POLYGONS: usize = 1_000_000;
const MAX_VERTICES: usize = 2_000_000;
const MAX_TRIANGLES: usize = 2_000_000;
const MAX_EDGES: usize = 3_000_000;
const MAX_COORDINATE: f32 = 1.0e12;

#[derive(Clone, Debug)]
pub struct NavMesh {
    pub vertices: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Unique polygon edges within each tile, excluding off-mesh connections.
    pub edges: Vec<[[f32; 3]; 2]>,
    pub tile_count: usize,
    /// Number of ground polygons, excluding off-mesh connections.
    pub polygon_count: usize,
    /// Bounds of the rendered triangles in the original navigation coordinates.
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Unsupported(&'static str),
    Invalid {
        offset: usize,
        message: &'static str,
    },
    Limit(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("NavMesh import cancelled"),
            Self::Unsupported(message) => write!(f, "Unsupported NavMesh: {message}"),
            Self::Invalid { offset, message } => {
                write!(f, "Invalid NavMesh at byte {offset}: {message}")
            }
            Self::Limit(message) => write!(f, "NavMesh limit exceeded: {message}"),
        }
    }
}

impl std::error::Error for Error {}

fn invalid(offset: usize, message: &'static str) -> Error {
    Error::Invalid { offset, message }
}

fn check_cancelled(cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    if cancelled() {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
    base: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], base: usize) -> Self {
        Self {
            bytes,
            offset: 0,
            base,
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], Error> {
        let start = self.offset;
        let end = start
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| invalid(self.base + start, "truncated data"))?;
        self.offset = end;
        Ok(&self.bytes[start..end])
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn count(&mut self) -> Result<usize, Error> {
        let value = self.u32()? as i32;
        usize::try_from(value).map_err(|_| invalid(self.base + self.offset - 4, "negative count"))
    }

    fn scalar(&mut self) -> Result<f32, Error> {
        let value = f32::from_bits(self.u32()?);
        if !value.is_finite() || value.abs() > MAX_COORDINATE {
            return Err(invalid(
                self.base + self.offset - 4,
                "non-finite or excessive coordinate",
            ));
        }
        Ok(value)
    }

    fn point(&mut self) -> Result<[f32; 3], Error> {
        Ok([self.scalar()?, self.scalar()?, self.scalar()?])
    }

    fn array(&mut self, count: usize, stride: usize) -> Result<&'a [u8], Error> {
        let bytes = count
            .checked_mul(stride)
            .ok_or_else(|| invalid(self.base + self.offset, "array size overflow"))?;
        self.take(bytes)
    }

    fn finish(&self) -> Result<(), Error> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(invalid(
                self.base + self.offset,
                "unexpected trailing bytes or unsupported layout",
            ))
        }
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize
}

fn u32_at(bytes: &[u8], offset: usize) -> usize {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize
}

fn budget(
    current: usize,
    additional: usize,
    limit: usize,
    name: &'static str,
) -> Result<(), Error> {
    if current
        .checked_add(additional)
        .is_none_or(|sum| sum > limit)
    {
        Err(Error::Limit(name))
    } else {
        Ok(())
    }
}

/// Decode a complete NavMeshSet without executing navigation queries or changing
/// coordinate systems. Cancellation is checked between tiles and within large arrays.
pub fn parse(bytes: &[u8], mut cancelled: impl FnMut() -> bool) -> Result<NavMesh, Error> {
    check_cancelled(&mut cancelled)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::Limit("file is larger than 128 MiB"));
    }
    let mut input = Reader::new(bytes, 0);
    match input.take(4)? {
        b"TESM" => {}
        b"TEST" | b"TSET" => {
            return Err(Error::Unsupported(
                "TileCache/TSET requires decompression and rebuilding; import a TileMesh/MSET file",
            ));
        }
        b"MSET" => {
            return Err(Error::Unsupported(
                "big endian; expected little endian MSET",
            ));
        }
        _ => return Err(Error::Unsupported("expected a Detour TileMesh/MSET file")),
    }
    if input.u32()? != 1 {
        return Err(Error::Unsupported(
            "MSET version (only version 1 is supported)",
        ));
    }
    let tile_count = input.count()?;
    budget(0, tile_count, MAX_TILES, "more than 65536 tiles")?;
    input.point()?; // dtNavMeshParams.orig
    let width = input.scalar()?;
    let height = input.scalar()?;
    let max_tiles = input.count()?;
    let max_polys = input.count()?;
    if width <= 0.0 || height <= 0.0 || max_tiles < tile_count || max_polys == 0 {
        return Err(invalid(12, "invalid navigation mesh parameters"));
    }
    if tile_count == 0 {
        return Err(invalid(8, "mesh has no tiles"));
    }
    let mut result = NavMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        edges: Vec::new(),
        tile_count: 0,
        polygon_count: 0,
        bounds_min: [f32::INFINITY; 3],
        bounds_max: [f32::NEG_INFINITY; 3],
    };
    let mut tile_refs = HashSet::new();
    let mut tile_locations = HashSet::new();
    let mut remaining_polygons = MAX_POLYGONS;
    for _ in 0..tile_count {
        check_cancelled(&mut cancelled)?;
        let reference = input.u32()?;
        let data_size = input.count()?;
        if reference == 0 || data_size == 0 {
            return Err(invalid(
                input.offset - 8,
                "empty tile header; expected complete tiles with 32-bit references",
            ));
        }
        if !tile_refs.insert(reference) {
            return Err(invalid(input.offset - 8, "duplicate tile reference"));
        }
        let base = input.offset;
        let tile = input.take(data_size)?;
        let (location, polygons) = parse_tile(
            tile,
            base,
            max_polys,
            remaining_polygons,
            &mut result,
            &mut cancelled,
        )?;
        remaining_polygons -= polygons;
        if !tile_locations.insert(location) {
            return Err(invalid(base + 8, "duplicate tile coordinates and layer"));
        }
        result.tile_count += 1;
    }
    input.finish()?;
    if result.indices.is_empty() {
        return Err(invalid(0, "mesh has no ground triangles"));
    }
    check_cancelled(&mut cancelled)?;
    Ok(result)
}

fn parse_tile(
    bytes: &[u8],
    base: usize,
    max_polys: usize,
    remaining_polygons: usize,
    mesh: &mut NavMesh,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<([u32; 3], usize), Error> {
    let mut input = Reader::new(bytes, base);
    if input.take(4)? != b"VAND" {
        return Err(invalid(
            base,
            "expected a DNAV tile (32-bit reference layout)",
        ));
    }
    if input.u32()? != 7 {
        return Err(Error::Unsupported(
            "Detour tile version (only version 7 is supported)",
        ));
    }
    let location = [input.u32()?, input.u32()?, input.u32()?];
    input.u32()?; // userId
    let polygon_count = input.count()?;
    let vertex_count = input.count()?;
    let link_count = input.count()?;
    let detail_count = input.count()?;
    let detail_vertex_count = input.count()?;
    let triangle_count = input.count()?;
    let bv_count = input.count()?;
    let off_mesh_count = input.count()?;
    let off_mesh_base = input.count()?;
    for _ in 0..3 {
        // walkableHeight, walkableRadius, walkableClimb
        if input.scalar()? < 0.0 {
            return Err(invalid(base + input.offset - 4, "negative agent dimension"));
        }
    }
    let minimum = input.point()?;
    let maximum = input.point()?;
    if minimum.iter().zip(maximum).any(|(min, max)| *min > max) || input.scalar()? < 0.0 {
        return Err(invalid(base + 72, "invalid tile bounds"));
    }
    if polygon_count > max_polys
        || detail_count != off_mesh_base
        || off_mesh_base.checked_add(off_mesh_count) != Some(polygon_count)
        || vertex_count > 65_536
    {
        return Err(invalid(base + 24, "inconsistent tile counts"));
    }
    budget(
        0,
        polygon_count,
        remaining_polygons,
        "more than 1000000 polygons",
    )?;
    budget(
        mesh.vertices.len(),
        vertex_count,
        MAX_VERTICES,
        "more than 2000000 vertices",
    )?;
    budget(
        mesh.vertices.len() + vertex_count,
        detail_vertex_count,
        MAX_VERTICES,
        "more than 2000000 vertices",
    )?;
    budget(
        mesh.indices.len() / 3,
        triangle_count,
        MAX_TRIANGLES,
        "more than 2000000 triangles",
    )?;
    let vertex_bytes = input.array(vertex_count, 12)?;
    let polygon_bytes = input.array(polygon_count, 32)?;
    input.array(link_count, 12)?; // 32-bit dtLink, runtime connectivity is not rendered.
    let detail_bytes = input.array(detail_count, 12)?;
    let detail_vertex_bytes = input.array(detail_vertex_count, 12)?;
    let triangle_bytes = input.array(triangle_count, 4)?;
    input.array(bv_count, 16)?;
    input.array(off_mesh_count, 36)?;
    input.finish()?;

    let first_vertex = mesh.vertices.len();
    for (data, data_base) in [
        (vertex_bytes, base + 100),
        (
            detail_vertex_bytes,
            base + 100
                + vertex_bytes.len()
                + polygon_bytes.len()
                + link_count * 12
                + detail_bytes.len(),
        ),
    ] {
        let mut vertices = Reader::new(data, data_base);
        for index in 0..data.len() / 12 {
            if index % 1024 == 0 {
                check_cancelled(cancelled)?;
            }
            mesh.vertices.push(vertices.point()?);
        }
    }
    let mut edge_indices = HashSet::new();
    for polygon_index in 0..polygon_count {
        if polygon_index % 256 == 0 {
            check_cancelled(cancelled)?;
        }
        let polygon = &polygon_bytes[polygon_index * 32..][..32];
        let count = polygon[30] as usize;
        let kind = polygon[31] >> 6;
        let polygon_offset = base + 100 + vertex_bytes.len() + polygon_index * 32;
        match kind {
            0 if (3..=6).contains(&count) && polygon_index < off_mesh_base => {}
            1 if count == 2 && polygon_index >= off_mesh_base => {}
            _ => {
                return Err(invalid(
                    polygon_offset,
                    "invalid polygon type, order or vertex count",
                ));
            }
        }
        let mut vertices = [0; 6];
        for (index, vertex) in vertices.iter_mut().take(count).enumerate() {
            *vertex = u16_at(polygon, 4 + index * 2);
            if *vertex >= vertex_count {
                return Err(invalid(
                    polygon_offset + 4 + index * 2,
                    "polygon vertex index out of range",
                ));
            }
        }
        if kind == 1 {
            continue;
        }
        let detail = &detail_bytes[polygon_index * 12..][..12];
        let vertex_base = u32_at(detail, 0);
        let triangle_base = u32_at(detail, 4);
        let local_vertices = detail[8] as usize;
        let local_triangles = detail[9] as usize;
        if vertex_base
            .checked_add(local_vertices)
            .is_none_or(|end| end > detail_vertex_count)
            || triangle_base
                .checked_add(local_triangles)
                .is_none_or(|end| end > triangle_count)
            || local_triangles == 0
        {
            return Err(invalid(
                polygon_offset,
                "detail mesh range out of bounds or empty",
            ));
        }
        for triangle_index in triangle_base..triangle_base + local_triangles {
            let triangle = &triangle_bytes[triangle_index * 4..][..3];
            for &corner in triangle {
                let corner = corner as usize;
                let vertex = if corner < count {
                    first_vertex + vertices[corner]
                } else if corner - count < local_vertices {
                    first_vertex + vertex_count + vertex_base + corner - count
                } else {
                    return Err(invalid(
                        polygon_offset,
                        "detail triangle vertex index out of range",
                    ));
                };
                // Overlapping detail triangle ranges must not bypass output limits.
                budget(
                    mesh.indices.len(),
                    1,
                    MAX_TRIANGLES * 3,
                    "more than 2000000 triangles",
                )?;
                mesh.indices.push(vertex as u32);
                for axis in 0..3 {
                    mesh.bounds_min[axis] = mesh.bounds_min[axis].min(mesh.vertices[vertex][axis]);
                    mesh.bounds_max[axis] = mesh.bounds_max[axis].max(mesh.vertices[vertex][axis]);
                }
            }
        }
        for index in 0..count {
            let a = vertices[index];
            let b = vertices[(index + 1) % count];
            if edge_indices.insert((a.min(b), a.max(b))) {
                budget(
                    mesh.edges.len(),
                    1,
                    MAX_EDGES,
                    "more than 3000000 polygon edges",
                )?;
                mesh.edges.push([
                    mesh.vertices[first_vertex + a],
                    mesh.vertices[first_vertex + b],
                ]);
            }
        }
        mesh.polygon_count += 1;
    }
    Ok((location, polygon_count))
}

#[cfg(test)]
mod tests;
