use super::*;
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

fn class() -> Name {
    Name {
        namespace: 1,
        name: 2,
    }
}

fn element_spec(index: usize, value_type: ElementType, component: usize) -> Spec {
    let component_name = match value_type {
        ElementType::I64 | ElementType::U64 | ElementType::F32 | ElementType::F64 => "value",
        ElementType::Vec3 | ElementType::Quat => ["x", "y", "z", "w"][component],
        ElementType::Transform => [
            "position.x",
            "position.y",
            "position.z",
            "rotation.x",
            "rotation.y",
            "rotation.z",
            "rotation.w",
        ][component],
    };
    Spec {
        source: Source::PropertyElement { index, value_type },
        object: 7,
        generation: 0,
        property: "Values".into(),
        component,
        component_name: component_name.into(),
    }
}

fn set(datatype: u32, items: u32, data: Vec<u8>) -> Event {
    Event::SetPropertyValue {
        id: 7,
        property: 3,
        datatype: Name {
            namespace: 1,
            name: datatype,
        },
        items,
        data: data.into(),
    }
}

fn floats(datatype: u32, items: u32, values: &[f32]) -> Event {
    set(
        datatype,
        items,
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect(),
    )
}

fn fixture_events(updates: Vec<Vec<Event>>) -> Vec<Event> {
    let mut events: Vec<_> = [
        (1, "physx3"),
        (2, "CurveFixture"),
        (3, "Values"),
        (4, "frame"),
        (10, "PvdI64"),
        (11, "PvdU64"),
        (12, "PvdF32"),
        (13, "PvdF64"),
        (14, "PxVec3"),
        (15, "PxQuat"),
        (16, "PxTransform"),
    ]
    .into_iter()
    .map(|(handle, value)| Event::StringHandle {
        handle,
        value: value.into(),
    })
    .collect();
    events.extend([
        Event::CreateClass { name: class() },
        Event::CreateProperty {
            class: class(),
            name: 3,
            semantic: 0,
            datatype: Name {
                namespace: 1,
                name: 12,
            },
            kind: 2,
            named_values: vec![],
        },
        Event::CreateInstance {
            class: class(),
            id: 1,
        },
        Event::CreateInstance {
            class: class(),
            id: 7,
        },
    ]);
    for (frame, updates) in updates.into_iter().enumerate() {
        events.extend(updates);
        events.push(Event::EndSection {
            id: 1,
            name: 4,
            timestamp: frame as u64,
        });
    }
    events
}

fn recording(updates: Vec<Vec<Event>>) -> Recording {
    let mut builder = pvd_core::RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events: fixture_events(updates),
        })
        .unwrap();
    builder.finish().unwrap()
}

fn values(recording: &Recording, spec: &Spec) -> Vec<Option<f64>> {
    sample(
        recording,
        spec,
        &AtomicBool::new(false),
        &AtomicUsize::new(0),
    )
    .unwrap()
    .samples
    .into_iter()
    .map(|(_, value)| value)
    .collect()
}

#[test]
fn array_element_lookup_borrows_one_fixed_index_and_rejects_shape_changes() {
    let elements = Arc::new(vec![
        Value::Vec3([1.0, 2.0, 3.0]),
        Value::Quat([4.0, 5.0, 6.0, 7.0]),
        Value::F32(8.0),
        Value::F64(9.0),
    ]);
    let array = Value::Array(elements.clone());
    let spec = element_spec(1, ElementType::Quat, 0);
    assert!(std::ptr::eq(
        spec.property_value(&array).unwrap(),
        &elements[1]
    ));
    assert_eq!(Arc::strong_count(&elements), 2);
    assert!(
        element_spec(1, ElementType::Vec3, 0)
            .property_value(&array)
            .is_none()
    );
    assert!(
        element_spec(2, ElementType::F64, 0)
            .property_value(&array)
            .is_none()
    );
    assert!(
        element_spec(usize::MAX, ElementType::F32, 0)
            .property_value(&array)
            .is_none()
    );
    assert!(spec.property_value(&Value::Quat([0.0; 4])).is_none());
    let nested = Value::Array(vec![array].into());
    assert!(
        element_spec(0, ElementType::F32, 0)
            .property_value(&nested)
            .is_none()
    );
}

#[test]
fn array_element_samples_leave_gaps_for_missing_indices_types_and_generations() {
    let recording = recording(vec![
        vec![],
        vec![floats(14, 2, &[99.0, 99.0, 99.0, 1.0, 2.0, 3.0])],
        vec![floats(14, 1, &[77.0, 77.0, 77.0])],
        vec![floats(15, 2, &[0.0, 0.0, 0.0, 1.0, 4.0, 5.0, 6.0, 1.0])],
        vec![floats(14, 2, &[0.0, 0.0, 0.0, f32::INFINITY, 2.0, 3.0])],
        vec![floats(14, 2, &[0.0, 0.0, 0.0, 5.0, 6.0, 7.0])],
        vec![Event::DestroyInstance { id: 7 }],
        vec![
            Event::CreateInstance {
                class: class(),
                id: 7,
            },
            floats(14, 2, &[0.0, 0.0, 0.0, 7.0, 8.0, 9.0]),
        ],
        vec![],
    ]);
    let mut spec = element_spec(1, ElementType::Vec3, 0);
    assert_eq!(
        values(&recording, &spec),
        vec![
            None,
            Some(1.0),
            None,
            None,
            None,
            Some(5.0),
            None,
            None,
            None
        ]
    );
    spec.generation = 1;
    assert_eq!(
        values(&recording, &spec),
        vec![
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(7.0),
            Some(7.0)
        ]
    );
    spec.generation = 0;
    spec.source = Source::PropertyElement {
        index: 1,
        value_type: ElementType::Quat,
    };
    assert_eq!(
        values(&recording, &spec),
        vec![None, None, None, Some(4.0), None, None, None, None, None]
    );
}

#[test]
fn array_scalar_samples_do_not_coerce_changed_numeric_types_or_nonfinite_values() {
    let recording = recording(vec![
        vec![floats(12, 1, &[1.0])],
        vec![set(13, 1, 2.0f64.to_le_bytes().to_vec())],
        vec![set(10, 1, (-3i64).to_le_bytes().to_vec())],
        vec![set(11, 1, 4u64.to_le_bytes().to_vec())],
        vec![floats(12, 1, &[f32::NAN])],
        vec![floats(12, 1, &[f32::NEG_INFINITY])],
        vec![floats(12, 1, &[2.5])],
    ]);
    assert_eq!(
        values(&recording, &element_spec(0, ElementType::F32, 0)),
        vec![Some(1.0), None, None, None, None, None, Some(2.5)]
    );
    for (value_type, frame, expected) in [
        (ElementType::F64, 1, 2.0),
        (ElementType::I64, 2, -3.0),
        (ElementType::U64, 3, 4.0),
    ] {
        let actual = values(&recording, &element_spec(0, value_type, 0));
        for (index, value) in actual.into_iter().enumerate() {
            assert_eq!(value, (index == frame).then_some(expected));
        }
    }
}

#[test]
fn array_quaternion_and_transform_samples_use_recorded_components() {
    let recording = recording(vec![
        vec![floats(15, 1, &[1.0, 2.0, 3.0, 4.0])],
        vec![floats(16, 1, &[5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0])],
    ]);
    assert_eq!(
        values(&recording, &element_spec(0, ElementType::Quat, 3)),
        vec![Some(4.0), None]
    );
    assert_eq!(
        values(&recording, &element_spec(0, ElementType::Transform, 2)),
        vec![None, Some(11.0)]
    );
    assert_eq!(
        values(&recording, &element_spec(0, ElementType::Transform, 6)),
        vec![None, Some(8.0)]
    );
}

#[test]
fn array_curve_identity_labels_csv_and_limits_keep_the_element_index() {
    let mut workspace = Workspace::default();
    let source = element_spec(7, ElementType::Quat, 0);
    let first = workspace.add(source.clone()).unwrap();
    assert_eq!(workspace.add(source.clone()).unwrap(), first);
    assert_eq!(workspace.pending.len(), 1);
    assert!(!source.needs_analysis());
    let mut changed = source.clone();
    changed.source = Source::PropertyElement {
        index: 8,
        value_type: ElementType::Quat,
    };
    assert_ne!(workspace.add(changed).unwrap(), first);
    let mut changed = source.clone();
    changed.source = Source::PropertyElement {
        index: 7,
        value_type: ElementType::Vec3,
    };
    assert_ne!(workspace.add(changed).unwrap(), first);
    let mut changed = source.clone();
    changed.component = 1;
    changed.component_name = "y".into();
    assert_ne!(workspace.add(changed).unwrap(), first);
    let mut changed = source.clone();
    changed.generation = 1;
    assert_ne!(workspace.add(changed).unwrap(), first);
    let mut changed = source.clone();
    changed.source = Source::Property;
    assert_ne!(workspace.add(changed).unwrap(), first);
    while workspace.curves.len() < MAX_CURVES {
        let index = workspace.curves.len() + 100;
        workspace
            .add(element_spec(index, ElementType::F32, 0))
            .unwrap();
    }
    assert!(
        workspace
            .add(element_spec(999, ElementType::F32, 0))
            .is_err()
    );
    assert_eq!(workspace.curves.len(), MAX_CURVES);
    for language in [Language::Chinese, Language::English] {
        assert!(source.label().text(language).contains("Values[7].x"));
    }
    let curve = &mut workspace.curves[0];
    assert!(curve.export_path.ends_with("element-7.csv"));
    curve.data = Some(Data {
        samples: vec![(0, Some(1.25)), (2, None)],
        stride: 2,
        source_frames: 3,
        reference_frame_count: None,
        statuses: vec![],
        occurrence_counts: vec![],
        diagnostics: None,
    });
    let exported = csv(curve).unwrap();
    let rows: Vec<Vec<_>> = exported
        .lines()
        .map(|line| line.split(',').collect())
        .collect();
    assert!(rows.iter().all(|row| row.len() == 24));
    assert_eq!(&rows[0][22..], &["array_index", "element_type"]);
    assert_eq!(&rows[1][22..], &["7", "quat"]);
    assert_eq!(rows[1][4], "\"Values\"");
    assert_eq!(rows[1][8], "property_element");
    assert_eq!(rows[1][11], "recorded_fixed_array_index");
    assert_eq!(rows[2][1], "");
    assert_eq!(rows[2][10], "unavailable_element_value");
    curve.spec.property = "quoted,\"Values\"".into();
    assert!(csv(curve).unwrap().contains("\"quoted,\"\"Values\"\"\""));
}

#[test]
fn array_curve_sampling_keeps_the_sample_budget_and_observes_cancellation() {
    let mut updates = vec![vec![]; 6001];
    updates[0].push(floats(12, 2, &[99.0, 2.0]));
    let recording = recording(updates);
    let progress = AtomicUsize::new(0);
    let spec = element_spec(1, ElementType::F32, 0);
    let data = sample(&recording, &spec, &AtomicBool::new(false), &progress).unwrap();
    assert!(data.samples.len() <= MAX_SAMPLES);
    assert_eq!(data.samples.first(), Some(&(0, Some(2.0))));
    assert_eq!(data.samples.last(), Some(&(6000, Some(2.0))));
    assert!(data.samples.iter().all(|(_, value)| *value == Some(2.0)));
    assert_eq!(progress.load(Ordering::Relaxed), 6001);
    let progress = AtomicUsize::new(0);
    assert!(sample(&recording, &spec, &AtomicBool::new(true), &progress).is_err());
    assert_eq!(progress.load(Ordering::Relaxed), 0);
}

#[test]
fn studio_array_plot_validates_elements_and_deduplicates_requests() {
    let recording = recording(vec![vec![floats(14, 1, &[1.0, 2.0, 3.0])]]);
    let context = bevy_egui::egui::Context::default();
    let mut studio = crate::studio::Studio::default();
    studio.recording = Some(Arc::new(recording));
    studio.seek(0);
    studio.selected = Some(7);
    studio.plot_element("Values".into(), usize::MAX, 0, context.clone());
    studio.plot_element("Values".into(), 0, 3, context.clone());
    assert!(studio.curves.curves.is_empty());
    studio.plot_element("Values".into(), 0, 1, context.clone());
    studio.plot_element("Values".into(), 0, 1, context.clone());
    assert_eq!(studio.curves.curves.len(), 1);
    assert_eq!(
        studio.curves.curves[0].spec,
        element_spec(0, ElementType::Vec3, 1)
    );
    studio
        .state
        .objects
        .get_mut(&7)
        .unwrap()
        .properties
        .insert("Values".into(), Value::Array(vec![Value::Ref(1)].into()));
    studio.plot_element("Values".into(), 0, 0, context);
    assert_eq!(studio.curves.curves.len(), 1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !studio.poll_shutdown() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::park_timeout(std::time::Duration::from_millis(1));
    }
}

mod render_fixture;
