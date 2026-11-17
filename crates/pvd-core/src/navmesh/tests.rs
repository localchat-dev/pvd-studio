use super::*;

const UPSTREAM: &[u8] = include_bytes!("fixtures/recast-0.43.1.bin");
const DETAIL: &[u8] = include_bytes!("fixtures/synthetic-detail.bin");

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn expanded(mesh: &NavMesh) -> Vec<f32> {
    mesh.indices
        .iter()
        .flat_map(|index| mesh.vertices[*index as usize])
        .collect()
}

#[test]
fn matches_independent_recast_0431_export_import_and_extraction() {
    let mesh = parse(UPSTREAM, || false).unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/recast-0.43.1.json")).unwrap();
    let positions: Vec<f32> = expected["positions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap() as f32)
        .collect();
    assert_eq!(expanded(&mesh), positions);
    assert_eq!(mesh.tile_count, 1);
    assert_eq!(mesh.polygon_count, 3);
    assert_eq!(mesh.indices.len(), 18);
    assert_eq!(mesh.bounds_min, [-4.5, 0.2, -4.5]);
    assert_eq!(mesh.bounds_max, [4.5, 0.2, 4.5]);
}

#[test]
fn preserves_detail_height_and_multiple_tiles_without_triangulating_offmesh() {
    let mesh = parse(DETAIL, || false).unwrap();
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/synthetic-detail.json")).unwrap();
    let positions: Vec<f32> = expected["surface_triangles"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|triangle| triangle.as_array().unwrap())
        .flat_map(|point| point.as_array().unwrap())
        .map(|value| value.as_f64().unwrap() as f32)
        .collect();
    assert_eq!(expanded(&mesh), positions);
    assert_eq!(
        (mesh.tile_count, mesh.polygon_count, mesh.indices.len() / 3),
        (2, 2, 8)
    );
    assert_eq!(mesh.edges.len(), 8);
    assert_eq!(mesh.bounds_min, [-8.0, 0.08, -4.0]);
    assert_eq!(mesh.bounds_max, [8.0, 1.08, 4.0]);
    // The detail center differs from the coarse polygon's plane.
    assert!(
        mesh.indices
            .iter()
            .any(|i| mesh.vertices[*i as usize] == [-4.0, 0.8, 0.0])
    );
}

#[test]
fn every_truncated_prefix_fails_without_panicking() {
    for bytes in [UPSTREAM, DETAIL] {
        for end in 0..bytes.len() {
            assert!(
                parse(&bytes[..end], || false).is_err(),
                "accepted prefix {end}"
            );
        }
    }
}

#[test]
fn cancellation_works_before_and_during_decode() {
    assert_eq!(parse(UPSTREAM, || true).unwrap_err(), Error::Cancelled);
    let mut calls = 0;
    assert_eq!(
        parse(DETAIL, || {
            calls += 1;
            calls == 4
        })
        .unwrap_err(),
        Error::Cancelled
    );
    assert_eq!(calls, 4);
}

#[test]
fn rejects_unsupported_formats_and_versions() {
    for (offset, value) in [
        (0, u32::from_le_bytes(*b"TEST")),
        (0, u32::from_le_bytes(*b"MSET")),
        (4, 2),
        (52, 8),
    ] {
        let mut bytes = UPSTREAM.to_vec();
        set_u32(&mut bytes, offset, value);
        assert!(matches!(
            parse(&bytes, || false),
            Err(Error::Unsupported(_))
        ));
    }
}

#[test]
fn rejects_unbounded_or_negative_counts_before_allocating() {
    for (offset, value) in [
        (8, 65_537),
        (8, u32::MAX),
        (44, u32::MAX),
        (76, u32::MAX),
        (80, i32::MAX as u32),
        (88, 2_000_001),
    ] {
        let mut bytes = UPSTREAM.to_vec();
        set_u32(&mut bytes, offset, value);
        assert!(
            parse(&bytes, || false).is_err(),
            "accepted invalid count at {offset}"
        );
    }
}

#[test]
fn rejects_invalid_geometry_and_detail_ranges() {
    // Upstream tile: header 100, vertices 8*12, polygons 3*32,
    // links 12*12, details 3*12, then triangles.
    let polygon = 48 + 100 + 8 * 12;
    let detail = polygon + 3 * 32 + 12 * 12;
    let triangles = detail + 3 * 12;
    for (offset, replacement) in [
        (148, f32::NAN.to_bits()),
        (148, f32::INFINITY.to_bits()),
        (148, f32::MAX.to_bits()),
        (detail, 1),     // no extra detail vertices exist in this tile
        (detail + 4, 6), // triangle start past last triangle
    ] {
        let mut bytes = UPSTREAM.to_vec();
        set_u32(&mut bytes, offset, replacement);
        assert!(
            parse(&bytes, || false).is_err(),
            "accepted bad geometry at {offset}"
        );
    }
    for (offset, replacement) in [
        (polygon + 4, 255),
        (polygon + 30, 7),
        (polygon + 31, 128),
        (triangles, 255),
    ] {
        let mut bytes = UPSTREAM.to_vec();
        bytes[offset] = replacement;
        assert!(
            parse(&bytes, || false).is_err(),
            "accepted invalid index/type at {offset}"
        );
    }
}

#[test]
fn rejects_duplicate_tiles_trailing_bytes_and_reference_layout_mismatch() {
    let mut duplicate = UPSTREAM.to_vec();
    duplicate.extend_from_slice(&UPSTREAM[40..]);
    set_u32(&mut duplicate, 8, 2);
    set_u32(&mut duplicate, 32, 2);
    assert!(
        parse(&duplicate, || false)
            .unwrap_err()
            .to_string()
            .contains("duplicate tile reference")
    );
    // Distinct reference, same tile location.
    set_u32(&mut duplicate, UPSTREAM.len(), 8);
    assert!(
        parse(&duplicate, || false)
            .unwrap_err()
            .to_string()
            .contains("duplicate tile coordinates")
    );
    let mut trailing = UPSTREAM.to_vec();
    trailing.push(0);
    assert!(
        parse(&trailing, || false)
            .unwrap_err()
            .to_string()
            .contains("trailing")
    );
    let mut ref64 = UPSTREAM.to_vec();
    ref64.splice(44..44, [0; 4]);
    assert!(
        parse(&ref64, || false)
            .unwrap_err()
            .to_string()
            .contains("32-bit")
    );
}

#[test]
fn arbitrary_single_byte_mutations_cannot_panic() {
    for index in 0..UPSTREAM.len() {
        let mut bytes = UPSTREAM.to_vec();
        bytes[index] ^= 0xff;
        let _ = parse(&bytes, || false);
    }
}
