use super::*;
use pvd_core::{ClassDefinition, Object, PropertyDefinition};
use pvd_protocol::{Endian, Header};
use std::sync::Arc;

fn metadata(semantic: &str) -> Recording {
    let mut recording = pvd_core::RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    })
    .finish()
    .unwrap();
    recording.classes.insert(
        "fixture::Base".into(),
        ClassDefinition {
            parent: None,
            properties: [(
                "Values".into(),
                PropertyDefinition {
                    datatype: "physx3::PvdU32".into(),
                    semantic: semantic.into(),
                    array: true,
                    named_values: vec![
                        ("Read".into(), 1),
                        ("Write".into(), 2),
                        ("Execute".into(), 4),
                    ],
                },
            )]
            .into(),
        },
    );
    recording.classes.insert(
        "fixture::Derived".into(),
        ClassDefinition {
            parent: Some("fixture::Base".into()),
            properties: Default::default(),
        },
    );
    recording
}

fn text_shapes(output: &egui::FullOutput) -> Vec<String> {
    output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect()
}

fn expanded_array_text(
    language: Language,
    recording: Recording,
    values: Vec<Value>,
) -> Vec<String> {
    inspector_text(language, recording, Value::Array(values.into()), true)
}

fn inspector_text(
    language: Language,
    recording: Recording,
    value: Value,
    expand_array: bool,
) -> Vec<String> {
    let context = egui::Context::default();
    configure(&context);
    let mut studio = Studio::default();
    studio.prefs.language = language;
    studio.recording = Some(Arc::new(recording));
    studio.selected = Some(7);
    studio.state.objects.insert(
        7,
        Object {
            id: 7,
            generation: 0,
            type_name: "fixture::Derived".into(),
            properties: [("Values".into(), value)].into(),
        },
    );
    let mut viewport = Viewport::default();
    let mut images = Assets::<Image>::default();
    let mut draw = |events| {
        context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 620.0),
                )),
                events,
                ..Default::default()
            },
            |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    Viewer {
                        studio: &mut studio,
                        viewport: &mut viewport,
                        images: &mut images,
                    }
                    .inspector(ui);
                });
            },
        )
    };
    let _ = draw(vec![]);
    let output = draw(vec![]);
    if !expand_array {
        return text_shapes(&output);
    }
    let prefix = language.text("Array · ", "数组 · ");
    let position = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.text().starts_with(prefix) => {
                Some(text.galley.rect.translate(text.pos.to_vec2()).center())
            }
            _ => None,
        })
        .expect("array expansion button");
    let _ = draw(vec![egui::Event::PointerMoved(position)]);
    for pressed in [true, false] {
        let _ = draw(vec![egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }]);
    }
    text_shapes(&draw(vec![]))
}

#[test]
fn array_elements_use_inherited_enum_flags_and_numeric_fallbacks() {
    let enumeration = metadata("Enumeration");
    let flags = metadata("Bitflag");
    assert_eq!(
        property_text(
            Some(&enumeration),
            "fixture::Derived",
            "Values",
            &Value::U64(2)
        ),
        "Write (2)"
    );
    assert_eq!(
        property_text(Some(&flags), "fixture::Derived", "Values", &Value::U64(5)),
        "Read | Execute (0x5)"
    );
    for (recording, kind, property, value, expected) in [
        (None, "fixture::Derived", "Values", Value::U64(2), "2"),
        (
            Some(&enumeration),
            "fixture::Missing",
            "Values",
            Value::U64(2),
            "2",
        ),
        (
            Some(&enumeration),
            "fixture::Derived",
            "Missing",
            Value::U64(2),
            "2",
        ),
        (
            Some(&enumeration),
            "fixture::Derived",
            "Values",
            Value::U64(99),
            "99",
        ),
        (
            Some(&enumeration),
            "fixture::Derived",
            "Values",
            Value::I64(-1),
            "-1",
        ),
        (
            Some(&flags),
            "fixture::Derived",
            "Values",
            Value::U64(8),
            "8",
        ),
    ] {
        assert_eq!(property_text(recording, kind, property, &value), expected);
    }
}

#[test]
fn expanded_array_rows_show_nonzero_small_values_in_scientific_notation() {
    for language in [Language::English, Language::Chinese] {
        let text = expanded_array_text(
            language,
            metadata(""),
            vec![
                Value::F32(1e-8),
                Value::F64(-1e-8),
                Value::F64(1e9),
                Value::F32(0.125),
            ],
        );
        for expected in ["1.000000e-8", "-1.000000e-8", "1.000000e9", "0.125000"] {
            assert!(text.iter().any(|text| text == expected), "{text:?}");
        }
        assert!(!text.iter().any(|text| text == "0.000000"));
    }
}

#[test]
fn expanded_array_rows_apply_names_to_direct_elements_without_retyping_nested_arrays() {
    for language in [Language::English, Language::Chinese] {
        let text = expanded_array_text(
            language,
            metadata("Enumeration"),
            vec![
                Value::Array(vec![Value::U64(1), Value::U64(2)].into()),
                Value::U64(2),
            ],
        );
        assert!(text.iter().any(|text| text == "[2 items] 1 2"), "{text:?}");
        assert!(text.iter().any(|text| text == "Write (2)"), "{text:?}");
        assert!(!text.iter().any(|text| text == "Read (1)"));
        let text = expanded_array_text(language, metadata("Bitflag"), vec![Value::U64(5)]);
        assert!(
            text.iter().any(|text| text == "Read | Execute (0x5)"),
            "{text:?}"
        );
    }
}

#[test]
fn expanded_large_enum_arrays_format_only_visible_rows() {
    for language in [Language::English, Language::Chinese] {
        let text = expanded_array_text(
            language,
            metadata("Enumeration"),
            vec![Value::U64(2); 100_000],
        );
        let visible = text
            .iter()
            .filter(|text| text.as_str() == "Write (2)")
            .count();
        assert!((1..32).contains(&visible), "visible rows: {visible}");
    }
}

#[test]
#[ignore = "requires a PhysX 3.4 triangle mesh capture with packed point and index arrays"]
fn sdk_mesh_arrays_expand_and_resolve_fixed_index_menu_targets() {
    let capture = std::env::var_os("PVD_SDK34_LARGE_MESH_CAPTURE").unwrap();
    let recording = Recording::from_path(std::path::PathBuf::from(capture)).unwrap();
    let state = recording.state_at(0).unwrap();
    let object = state
        .objects
        .values()
        .find(|object| matches!(object.properties.get("Points"), Some(Value::MeshArray(_))))
        .unwrap();
    for name in ["Points", "Triangles"] {
        let array = &object.properties[name];
        let length = array.array_len().unwrap();
        let row = array.array_item(length - 1).unwrap();
        let target = PropertyMenuTarget::new(0, object, name, Some(length - 1), &row);
        assert_eq!(target.value(0, object).as_deref(), Some(row.as_ref()));
        let metadata = metadata("");
        let first = array.array_item(0).unwrap();
        let formatted = property_text(Some(&metadata), "fixture::Derived", "Values", &first);
        let labels = inspector_text(Language::English, metadata, array.clone(), true);
        assert!(labels.contains(&formatted), "{formatted} in {labels:?}");
        assert!(labels.iter().any(|label| label == "[0]"));
        assert!(!labels.contains(&format!("[{}]", length - 1)));
    }
}

fn composite_precision_examples() -> Vec<(Value, &'static str)> {
    vec![
        (
            Value::Vec3([1e-8, -1e8, 0.125]),
            "[1.000000e-8, -1.000000e8, 0.125000]",
        ),
        (
            Value::Quat([1e-8, -1e-8, 0.0, 1.0]),
            "[1.000000e-8, -1.000000e-8, 0.000000, 1.000000]",
        ),
        (
            Value::Transform {
                position: [1e-8, -1e8, 0.125],
                rotation: [0.0, 0.0, 1e-8, 1.0],
            },
            "p [1.000000e-8, -1.000000e8, 0.125000] · q [0.000000, 0.000000, 1.000000e-8, 1.000000]",
        ),
        (
            Value::Vec3([1.25, -2.5, 3.75]),
            "[1.250000, -2.500000, 3.750000]",
        ),
        (
            Value::Quat([0.0, 0.5, -0.5, 1.0]),
            "[0.000000, 0.500000, -0.500000, 1.000000]",
        ),
        (
            Value::Transform {
                position: [1.0, 2.0, 3.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
            },
            "p [1.000000, 2.000000, 3.000000] · q [0.000000, 0.000000, 0.000000, 1.000000]",
        ),
    ]
}

#[test]
fn composite_properties_keep_small_large_and_ordinary_components_readable() {
    for (value, expected) in composite_precision_examples() {
        assert_eq!(property_text(None, "", "", &value), expected);
    }
    for (value, expected) in [
        (
            Value::Vec3([-0.0, f32::NAN, f32::INFINITY]),
            "[-0.000000, NaN, inf]",
        ),
        (
            Value::Quat([f32::NEG_INFINITY, -0.0, f32::NAN, 1.0]),
            "[-inf, -0.000000, NaN, 1.000000]",
        ),
        (
            Value::Transform {
                position: [-0.0, f32::NAN, f32::INFINITY],
                rotation: [f32::NEG_INFINITY, -0.0, 0.0, 1.0],
            },
            "p [-0.000000, NaN, inf] · q [-inf, -0.000000, 0.000000, 1.000000]",
        ),
    ] {
        assert_eq!(property_text(None, "", "", &value), expected);
    }
}

#[test]
fn scalar_float_display_keeps_thresholds_nonfinite_values_and_signed_zero() {
    for (value, expected) in [
        (Value::F32(1e-4), "0.000100"),
        (Value::F32(1e6), "1.000000e6"),
        (Value::F32(0.125), "0.125000"),
        (Value::F32(0.0), "0.000000"),
        (Value::F32(-0.0), "-0.000000"),
        (Value::F32(f32::NAN), "NaN"),
        (Value::F32(f32::INFINITY), "inf"),
        (Value::F32(f32::NEG_INFINITY), "-inf"),
        (Value::F64(1e-4), "0.000100"),
        (Value::F64(1e6), "1.000000e6"),
        (Value::F64(0.125), "0.125000"),
        (Value::F64(0.0), "0.000000"),
        (Value::F64(-0.0), "-0.000000"),
        (Value::F64(f64::NAN), "NaN"),
        (Value::F64(f64::INFINITY), "inf"),
        (Value::F64(f64::NEG_INFINITY), "-inf"),
    ] {
        assert_eq!(property_text(None, "", "", &value), expected);
    }
}

#[test]
fn inspector_rows_and_expanded_arrays_show_composite_precision_in_both_languages() {
    for language in [Language::English, Language::Chinese] {
        let examples = composite_precision_examples();
        for (value, expected) in &examples {
            let text = inspector_text(language, metadata(""), value.clone(), false);
            assert!(
                text.iter().any(|text| text.as_str() == *expected),
                "{text:?}"
            );
        }
        let text = expanded_array_text(
            language,
            metadata(""),
            examples.iter().map(|(value, _)| value.clone()).collect(),
        );
        for (_, expected) in examples {
            assert!(text.iter().any(|text| text == expected), "{text:?}");
        }
    }
}
