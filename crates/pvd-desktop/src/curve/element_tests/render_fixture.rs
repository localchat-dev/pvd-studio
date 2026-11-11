use super::*;

fn blob(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(u32::try_from(bytes.len()).unwrap().to_le_bytes());
    out.extend_from_slice(bytes);
}

fn name(out: &mut Vec<u8>, value: Name) {
    out.extend(value.namespace.to_le_bytes());
    out.extend(value.name.to_le_bytes());
}

fn encode_fixture(events: &[Event]) -> Vec<u8> {
    let mut payload = Vec::new();
    for event in events {
        match event {
            Event::StringHandle { handle, value } => {
                payload.push(1);
                let mut text = value.as_bytes().to_vec();
                text.push(0);
                blob(&mut payload, &text);
                payload.extend(handle.to_le_bytes());
            }
            Event::CreateClass { name: value } => {
                payload.push(2);
                name(&mut payload, *value);
            }
            Event::CreateProperty {
                class,
                name: property,
                semantic,
                datatype,
                kind,
                named_values,
            } => {
                payload.push(4);
                name(&mut payload, *class);
                payload.extend(property.to_le_bytes());
                payload.extend(semantic.to_le_bytes());
                name(&mut payload, *datatype);
                payload.push(*kind);
                payload.extend(u32::try_from(named_values.len()).unwrap().to_le_bytes());
                for (label, value) in named_values {
                    payload.extend(label.to_le_bytes());
                    payload.extend(value.to_le_bytes());
                }
            }
            Event::CreateInstance { class, id } => {
                payload.push(6);
                name(&mut payload, *class);
                payload.extend(id.to_le_bytes());
            }
            Event::SetPropertyValue {
                id,
                property,
                datatype,
                items,
                data,
            } => {
                payload.push(7);
                payload.extend(id.to_le_bytes());
                payload.extend(property.to_le_bytes());
                name(&mut payload, *datatype);
                payload.extend(items.to_le_bytes());
                blob(&mut payload, data);
            }
            Event::EndSection {
                id,
                name,
                timestamp,
            } => {
                payload.push(19);
                payload.extend(id.to_le_bytes());
                payload.extend(name.to_le_bytes());
                payload.extend(timestamp.to_le_bytes());
            }
            event => panic!("unsupported array render fixture event: {event:?}"),
        }
    }
    let mut stream = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    stream.extend(1u32.to_le_bytes());
    stream.extend(1u64.to_le_bytes());
    stream.extend(1u64.to_le_bytes());
    stream.extend(1u32.to_le_bytes());
    stream.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
    stream.extend(u32::try_from(events.len()).unwrap().to_le_bytes());
    stream.extend(1u64.to_le_bytes());
    stream.extend(0u64.to_le_bytes());
    stream.extend(payload);
    stream
}

#[test]
#[ignore = "writes a deterministic constructed SDK stream to PVD_ARRAY_CURVE_FIXTURE_DIRECTORY"]
fn writes_array_curve_render_fixture() {
    let mut events = fixture_events(vec![
        vec![floats(14, 2, &[0.0, 0.0, 0.0, 10.0, 20.0, 30.0])],
        vec![floats(14, 2, &[1.0, 2.0, 3.0, 11.0, 21.0, 31.0])],
        vec![floats(14, 1, &[2.0, 4.0, 6.0])],
        vec![floats(14, 1, &[3.0, 6.0, 9.0])],
        vec![floats(14, 2, &[4.0, 8.0, 12.0, 14.0, 24.0, 34.0])],
        vec![floats(14, 2, &[5.0, 10.0, 15.0, 15.0, 25.0, 35.0])],
    ]);
    for event in &mut events {
        if let Event::CreateProperty { datatype, .. } = event {
            *datatype = Name {
                namespace: 1,
                name: 14,
            };
        }
    }
    let bytes = encode_fixture(&events);
    let recording = Recording::from_bytes(&bytes).unwrap();
    assert_eq!(recording.frame_count(), 6);
    assert_eq!(
        recording.classes["physx3::CurveFixture"].properties["Values"].datatype,
        "physx3::PxVec3"
    );
    for (component, expected) in [
        vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0],
        vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
        vec![0.0, 3.0, 6.0, 9.0, 12.0, 15.0],
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            values(&recording, &element_spec(0, ElementType::Vec3, component)),
            expected.into_iter().map(Some).collect::<Vec<_>>()
        );
    }
    for (component, expected) in [
        vec![Some(10.0), Some(11.0), None, None, Some(14.0), Some(15.0)],
        vec![Some(20.0), Some(21.0), None, None, Some(24.0), Some(25.0)],
        vec![Some(30.0), Some(31.0), None, None, Some(34.0), Some(35.0)],
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            values(&recording, &element_spec(1, ElementType::Vec3, component)),
            expected
        );
    }
    recording.verify_replay().unwrap();
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_ARRAY_CURVE_FIXTURE_DIRECTORY")
            .expect("PVD_ARRAY_CURVE_FIXTURE_DIRECTORY must name an explicit output directory"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("array-element-curves.pvdstream");
    std::fs::write(&path, bytes).unwrap();
    std::fs::write(
        directory.join("array-element-curves.expected.csv"),
        concat!(
            "frame,Values[0].x,Values[0].y,Values[0].z,Values[1].x,Values[1].y,Values[1].z\r\n",
            "0,0,0,0,10,20,30\r\n",
            "1,1,2,3,11,21,31\r\n",
            "2,2,4,6,,,\r\n",
            "3,3,6,9,,,\r\n",
            "4,4,8,12,14,24,34\r\n",
            "5,5,10,15,15,25,35\r\n",
        ),
    )
    .unwrap();
    println!("Wrote six-frame array curve fixture to {}", path.display());
}
