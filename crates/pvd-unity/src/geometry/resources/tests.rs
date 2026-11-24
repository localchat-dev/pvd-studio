use super::*;
use crate::geometry::tests::frame;

impl ResourceCache {
    fn write(
        &mut self,
        state: &SceneState,
        scene: RenderScene,
        rooms: &BTreeMap<u64, u64>,
        scope: (u64, u64),
        base: u64,
    ) -> Result<Vec<u8>, String> {
        let (data, next) = self.prepare(state, scene, rooms, scope, base)?;
        *self = next;
        Ok(data)
    }
}

fn triangle(x: f32) -> Geometry {
    Geometry::TriangleMesh {
        vertices: Arc::new(vec![[x, 0., 0.]]),
        indices: Arc::new(vec![0, 0, 0]),
    }
}

fn word64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

fn word32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

#[test]
fn view_packet_preserves_shape_only_fallback_and_limits_rays_to_complete_records() {
    let mut shape_packet = vec![0; MAX_BYTES - 4];
    shape_packet[..4].copy_from_slice(b"PVS3");
    let empty = SceneState::default();
    append_rays(
        &mut shape_packet,
        &empty,
        &RoomDirectory::default(),
        0,
        None,
    );
    assert_eq!(&shape_packet[..4], b"PVS3");
    assert_eq!(shape_packet.len(), MAX_BYTES - 4);

    shape_packet.truncate(MAX_BYTES - 8 - 37);
    let tail = shape_packet.len();
    let mut state = SceneState::default();
    state.objects.insert(
        1,
        Object {
            id: 1,
            generation: 0,
            type_name: "physx3::PxScene".into(),
            properties: BTreeMap::from([(
                "SceneQueries.Raycasts".into(),
                Value::Array(
                    vec![
                        Value::Array(
                            vec![
                                Value::U64(0),
                                Value::Array(vec![Value::U64(0); 4].into()),
                                Value::U64(0),
                                Value::Vec3([0.0; 3]),
                                Value::Vec3([1.0, 0.0, 0.0]),
                                Value::F32(3.0),
                                Value::String("SceneQueries.Hits".into()),
                                Value::U64(0),
                                Value::U64(0),
                            ]
                            .into()
                        );
                        2
                    ]
                    .into(),
                ),
            )]),
        },
    );
    append_rays(
        &mut shape_packet,
        &state,
        &RoomDirectory::default(),
        0,
        None,
    );
    assert_eq!(&shape_packet[..4], b"PVS4");
    assert_eq!(shape_packet.len(), MAX_BYTES);
    assert_eq!(word32(&shape_packet, tail), 1);
    assert_eq!(word32(&shape_packet, tail + 4), 1);
    assert_eq!(shape_packet[tail + 8], 0);
    assert_eq!(word32(&shape_packet, tail + 21), 3.0f32.to_bits());
    assert_eq!(&shape_packet[tail + 33..], &[0; 12]);
}

#[test]
fn queries_packet_preserves_shape_only_fallback_and_reserves_both_headers() {
    let mut packet = vec![0; MAX_BYTES - 15];
    packet[..4].copy_from_slice(b"PVS3");
    let empty = SceneState::default();
    let rooms = RoomDirectory::default();
    append_queries(&mut packet, &empty, &rooms, 0, None, true);
    assert_eq!(&packet[..4], b"PVS3");
    assert_eq!(packet.len(), MAX_BYTES - 15);

    packet.truncate(MAX_BYTES - 16);
    append_queries(&mut packet, &empty, &rooms, 0, None, false);
    assert_eq!(&packet[..4], b"PVS5");
    assert_eq!(packet.len(), MAX_BYTES);
    assert_eq!(&packet[MAX_BYTES - 16..], &[0; 16]);
}

#[test]
fn resources_share_content_across_actors_and_keep_pose_records() {
    let (mut state, mut scene) = frame(triangle(0.));
    let mut actor = state.objects[&5].clone();
    actor.id = 7;
    state.objects.insert(7, actor);
    let mut other = scene.objects[0].clone();
    other.id = 7;
    other.geometry = triangle(0.);
    scene.objects.push(other);
    let mut cache = ResourceCache::default();
    let rooms = BTreeMap::from([(5, 1), (7, 2)]);
    let first = cache
        .write(&state, scene.clone(), &rooms, (1, 0), 0)
        .unwrap();
    assert_eq!(&first[..4], b"PVS3");
    assert_eq!(word64(&first, 12), 0);
    assert_eq!(word32(&first, 20), 1);
    let id = word64(&first, 24);
    assert_ne!(id, 0);
    assert_eq!((word32(&first, 32), word32(&first, 36)), (1, 3));
    assert_eq!(word32(&first, 64), 2);
    for (record, (room, actor)) in first[68..].chunks_exact(98).zip([(1, 5), (2, 7)]) {
        assert_eq!(word64(record, 0), room);
        assert_eq!(word64(record, 8), actor);
        assert_eq!(record[33], 4);
        assert_eq!(word64(record, 90), id);
    }
    assert_eq!(first.len(), 68 + 2 * 98);
    state.objects.get_mut(&5).unwrap().properties.insert(
        "GlobalPose".into(),
        Value::Transform {
            position: [42., 0., 0.],
            rotation: [0., 0., 0., 1.],
        },
    );
    let second = cache
        .write(&state, scene.clone(), &rooms, (1, 0), word64(&first, 4))
        .unwrap();
    assert_eq!(word64(&second, 12), word64(&first, 4));
    assert_eq!((word32(&second, 20), word32(&second, 24)), (0, 2));
    assert_eq!(word32(&second, 28 + 34), 42f32.to_bits());
    assert_eq!(word64(&second, 28 + 90), id);
    scene.objects[0].geometry = triangle(-0.);
    let changed = cache
        .write(&state, scene, &rooms, (1, 0), word64(&second, 4))
        .unwrap();
    assert_eq!(word32(&changed, 20), 1);
    assert_ne!(word64(&changed, 24), id);
    assert_eq!(word32(&changed, 40), (-0f32).to_bits());
    assert_eq!(word64(&changed, 68 + 98 + 90), id);
}

#[test]
fn resources_rollback_evict_and_reset_with_scope_and_acknowledgement() {
    let mut cache = ResourceCache::default();
    let rooms = BTreeMap::new();
    let (state, scene) = frame(triangle(1.));
    let mut request = |scene, scope, base| cache.write(&state, scene, &rooms, scope, base);
    let first = request(scene.clone(), (1, 0), 0).unwrap();
    let base = word64(&first, 4);
    let mut broken = scene.clone();
    broken.objects[0].geometry = triangle(f32::NAN);
    assert!(request(broken, (1, 0), base).is_err());
    let retry = request(scene.clone(), (1, 0), base).unwrap();
    assert_eq!(word64(&retry, 12), base);
    assert_eq!(word32(&retry, 20), 0);
    let mut empty = scene.clone();
    empty.objects.clear();
    let dropped = request(empty, (1, 0), word64(&retry, 4)).unwrap();
    let restored = request(scene.clone(), (1, 0), word64(&dropped, 4)).unwrap();
    assert_eq!(word32(&restored, 20), 1);
    let mut token = word64(&restored, 4);
    for (scope, explicit) in [
        ((1, 7), None),
        ((2, 7), None),
        ((2, 7), Some(base)),
        ((2, 7), Some(0)),
    ] {
        let reset = request(scene.clone(), scope, explicit.unwrap_or(token)).unwrap();
        assert_eq!(word64(&reset, 12), 0);
        assert_eq!(word32(&reset, 20), 1);
        token = word64(&reset, 4);
    }
}

#[test]
fn shared_resource_fits_when_expanded_shapes_exceed_the_transport_limit() {
    let (mut state, mut scene) = frame(Geometry::TriangleMesh {
        vertices: Arc::new(vec![[0.; 3]; 100_000]),
        indices: Arc::new(vec![0, 0, 0]),
    });
    for id in 100..130 {
        let mut actor = state.objects[&5].clone();
        actor.id = id;
        state.objects.insert(id, actor);
        let mut object = scene.objects[0].clone();
        object.id = id;
        scene.objects.push(object);
    }
    let mut cache = ResourceCache::default();
    let data = cache
        .write(&state, scene, &BTreeMap::new(), (1, 0), 0)
        .unwrap();
    assert_eq!(word32(&data, 20), 1);
    assert_eq!(data.len(), 28 + 8 + 8 + 1_200_000 + 12 + 31 * 98);
}

#[test]
fn resource_budget_counts_one_complete_payload_per_id_and_hash_collisions_compare_bits() {
    let plus = triangle(0.);
    let minus = triangle(-0.);
    let hash = 17;
    let (_, payload) = payload_layout(&plus);
    assert_eq!(payload, 8 + 12 + 12);
    let mut table = MeshTable {
        bytes: MAX_RESOURCE_BYTES - 2 * payload,
        ..Default::default()
    };
    table.retain(1, hash, &plus, payload).unwrap();
    assert_eq!(table.matching_id(hash, &minus), None);
    table.retain(2, hash, &minus, payload).unwrap();
    assert_eq!(table.matching_id(hash, &triangle(0.)), Some(1));
    assert_eq!(table.matching_id(hash, &triangle(-0.)), Some(2));
    table.retain(2, hash, &minus, payload).unwrap();
    assert_eq!(table.bytes, MAX_RESOURCE_BYTES);
    assert!(
        table
            .retain(3, hash, &triangle(1.), payload)
            .unwrap_err()
            .contains("128 MiB")
    );
    assert_eq!(table.by_id.len(), 2);
}

#[test]
fn oversized_actual_packet_and_core_display_truncation_preserve_the_acknowledged_base() {
    let (state, scene) = frame(triangle(1.));
    let rooms = BTreeMap::new();
    let mut cache = ResourceCache::default();
    let first = cache
        .write(&state, scene.clone(), &rooms, (1, 0), 0)
        .unwrap();
    let base = word64(&first, 4);
    let mut oversized = scene.clone();
    // The vertex bytes alone fit, while both shape records and the definition do not.
    oversized.objects[0].geometry = Geometry::TriangleMesh {
        vertices: Arc::new(vec![[0.; 3]; (MAX_BYTES - 180) / 12]),
        indices: Arc::new(vec![]),
    };
    oversized.objects.push(scene.objects[0].clone());
    assert!(
        cache
            .write(&state, oversized, &rooms, (1, 0), base)
            .unwrap_err()
            .contains("32 MiB")
    );
    for reason in [
        "Scene display stopped at the 100000 shape limit; recorded object data remains available.",
        "Scene display stopped at its 128 MiB unique mesh allocation limit; recorded mesh data remains available.",
    ] {
        let mut partial = scene.clone();
        partial.objects.clear();
        partial.diagnostics.push(reason.into());
        assert_eq!(
            cache
                .write(&state, partial, &rooms, (1, 0), base)
                .unwrap_err(),
            reason
        );
    }
    let retry = cache.write(&state, scene, &rooms, (1, 0), base).unwrap();
    assert_eq!(word64(&retry, 12), base);
    assert_eq!(word32(&retry, 20), 0);
}

#[test]
fn resource_frame_keeps_inline_primitive_records() {
    let geometries = [
        Geometry::Box {
            half_extents: [1., 2., 3.],
        },
        Geometry::Sphere { radius: 4. },
        Geometry::Capsule {
            radius: 5.,
            half_height: 6.,
        },
        Geometry::Plane,
    ];
    for geometry in geometries {
        let (state, scene) = frame(geometry);
        let rooms = BTreeMap::new();
        let original = encode_shapes(&state, scene.clone(), &rooms).unwrap();
        let (packet, _) = ResourceCache::default()
            .prepare(&state, scene, &rooms, (1, 0), 0)
            .unwrap();
        assert_eq!(word32(&packet, 20), 0);
        assert_eq!(&packet[24..], &original[4..]);
    }
}
