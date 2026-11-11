use super::*;
use crate::native::{Class, ClassPacking, Property};
use pvd_protocol::Name;

fn string(metadata: &mut Metadata, value: &str) -> u32 {
    if let Some((&id, _)) = metadata
        .strings
        .iter()
        .find(|(_, text)| text.as_str() == value)
    {
        return id;
    }
    let id = metadata
        .strings
        .last_key_value()
        .map_or(0, |(&id, _)| id + 1);
    metadata.strings.insert(id, value.into());
    id
}

pub(in crate::native::export) fn install(metadata: &mut Metadata) {
    let namespace = string(metadata, "physx3");
    metadata.classes.resize_with(87, || None);
    for (id, label, size) in [
        (73, "PvdF32", 4),
        (82, "PxVec3", 12),
        (84, "PxBounds3", 24),
        (85, "PxQuat", 16),
        (86, "PxTransform", 28),
    ] {
        let name = Name {
            namespace,
            name: string(metadata, label),
        };
        metadata.classes[id] = Some(Class {
            name,
            base: u32::MAX,
            packing: ClassPacking {
                uniform_width: 4,
                class_type: 73,
            },
            size,
            properties: vec![],
            dynamic: false,
            has_pointers: false,
            string_offsets: vec![],
        });
    }
    for (old, replacement) in [(2, 86), (3, 84)] {
        if let Some(class) = metadata.classes[old].take() {
            assert_eq!(
                class.name,
                metadata.classes[replacement].as_ref().unwrap().name
            );
            for property in metadata.properties.iter_mut().flatten() {
                if property.datatype_id == old as u32 {
                    property.datatype_id = replacement as u32;
                }
            }
        }
    }
    for (owner, label, datatype, offset32) in [
        (82, "x", 73, 0),
        (82, "y", 73, 4),
        (82, "z", 73, 8),
        (84, "minimum", 82, 0),
        (84, "maximum", 82, 12),
        (85, "x", 73, 0),
        (85, "y", 73, 4),
        (85, "z", 73, 8),
        (85, "w", 73, 12),
        (86, "q", 85, 0),
        (86, "p", 82, 16),
    ] {
        let name = string(metadata, label);
        let property = metadata.properties.len() as u32;
        let owner_name = metadata.class(owner).unwrap().name;
        let datatype_name = metadata.class(datatype).unwrap().name;
        metadata.properties.push(Some(Property {
            owner: owner_name,
            name,
            datatype: datatype_name,
            datatype_id: datatype,
            kind: 1,
            semantic: 0,
            offset32,
            values: vec![],
        }));
        metadata.classes[owner as usize]
            .as_mut()
            .unwrap()
            .properties
            .push(property);
    }
}

fn metadata() -> Metadata {
    let mut metadata = Metadata {
        strings: [(0, String::new())].into(),
        classes: vec![],
        properties: vec![None],
        messages: vec![],
    };
    install(&mut metadata);
    metadata
}

fn accepted(metadata: &Metadata, id: u32) -> bool {
    fixed_layout(metadata, id, &mut |_| Ok(())).unwrap()
}

#[test]
fn fixed_layout_accepts_leaf_roots_and_exact_canonical_children() {
    for id in [84, 86] {
        let mut metadata = metadata();
        assert!(accepted(&metadata, id));
        let root = metadata.classes[id as usize].as_mut().unwrap();
        root.properties.clear();
        root.packing = ClassPacking::default();
        assert!(accepted(&metadata, id));
    }
    for id in [73, 82, 85] {
        assert!(!accepted(&metadata(), id));
    }
}

#[test]
fn canonical_layout_rejects_ambiguous_fields_and_recursive_leaf_fallback() {
    for case in 0..24 {
        let mut metadata = metadata();
        let root = metadata.class(86).unwrap().properties.clone();
        let vector = metadata.class(82).unwrap().properties.clone();
        let root_id = match case {
            0 => {
                metadata.classes[86].as_mut().unwrap().properties.swap(0, 1);
                86
            }
            1 => {
                metadata.classes[86]
                    .as_mut()
                    .unwrap()
                    .properties
                    .push(root[0]);
                86
            }
            2 => {
                metadata.classes[86].as_mut().unwrap().properties.pop();
                86
            }
            3 => {
                metadata.properties[root[1] as usize]
                    .as_mut()
                    .unwrap()
                    .offset32 = 12;
                86
            }
            4 => {
                metadata.properties[root[0] as usize]
                    .as_mut()
                    .unwrap()
                    .owner = metadata.class(85).unwrap().name;
                86
            }
            5 => {
                metadata.properties[root[0] as usize]
                    .as_mut()
                    .unwrap()
                    .datatype_id = 82;
                86
            }
            6 => {
                metadata.properties[root[1] as usize]
                    .as_mut()
                    .unwrap()
                    .datatype = metadata.class(85).unwrap().name;
                86
            }
            7 => {
                metadata.classes[82].as_mut().unwrap().properties.clear();
                86
            }
            8 => {
                metadata.classes[85].as_mut().unwrap().properties.clear();
                86
            }
            9 => {
                metadata.classes[82].as_mut().unwrap().size = 16;
                84
            }
            10 => {
                metadata.classes[85].as_mut().unwrap().base = 82;
                86
            }
            11 => {
                metadata.classes[73].as_mut().unwrap().size = 8;
                84
            }
            12 => {
                metadata.classes[73].as_mut().unwrap().has_pointers = true;
                86
            }
            13 => {
                metadata.classes[82].as_mut().unwrap().dynamic = true;
                84
            }
            14 => {
                metadata.classes[85]
                    .as_mut()
                    .unwrap()
                    .string_offsets
                    .push(0);
                86
            }
            15 => {
                metadata.classes[82].as_mut().unwrap().packing.class_type = 70;
                86
            }
            16 => {
                metadata.classes[86].as_mut().unwrap().packing.uniform_width = 8;
                86
            }
            17 => {
                metadata.properties[vector[0] as usize]
                    .as_mut()
                    .unwrap()
                    .kind = 2;
                84
            }
            18 => {
                metadata.properties[vector[0] as usize]
                    .as_mut()
                    .unwrap()
                    .semantic = 1;
                86
            }
            19 => {
                metadata.properties[vector[0] as usize]
                    .as_mut()
                    .unwrap()
                    .values
                    .push((0, 0));
                86
            }
            20 => {
                metadata.properties[vector[0] as usize]
                    .as_mut()
                    .unwrap()
                    .name = metadata.properties[vector[1] as usize]
                    .as_ref()
                    .unwrap()
                    .name;
                84
            }
            21 => {
                metadata.properties[vector[0] as usize]
                    .as_mut()
                    .unwrap()
                    .datatype_id = 86;
                86
            }
            22 => {
                let alias = metadata.classes[86].take();
                metadata.classes.push(alias);
                87
            }
            _ => {
                metadata.classes[86].as_mut().unwrap().size = 24;
                86
            }
        };
        assert!(!accepted(&metadata, root_id), "case {case}");
    }
}

#[test]
fn canonical_layout_scans_a_fixed_graph_with_exact_work_and_cancellation() {
    let metadata = metadata();
    for (id, nodes_and_fields) in [(84u32, 17usize), (86, 19)] {
        for limit in [nodes_and_fields * 64 - 1, nodes_and_fields * 64] {
            let mut work = limit;
            let result = fixed_layout(&metadata, id, &mut |bytes| {
                work = work
                    .checked_sub(bytes)
                    .ok_or_else(|| error("test work budget"))?;
                Ok(())
            });
            if limit == nodes_and_fields * 64 {
                assert!(result.unwrap());
                assert_eq!(work, 0);
            } else {
                assert!(result.unwrap_err().to_string().contains("test work budget"));
            }
        }
        for stop in 1..=nodes_and_fields {
            let mut calls = 0;
            let failure = fixed_layout(&metadata, id, &mut |_| {
                calls += 1;
                check(&mut || calls != stop)
            })
            .unwrap_err();
            assert_eq!(calls, stop);
            assert_eq!(
                failure.downcast_ref::<std::io::Error>().unwrap().kind(),
                std::io::ErrorKind::Interrupted
            );
        }
    }
}

#[test]
#[ignore = "requires original baseline/rich containers in PVD_NATIVE_ORIGIN_METADATA_DIRECTORY"]
fn original_metadata_matches_the_canonical_shift_layout() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("PVD_NATIVE_ORIGIN_METADATA_DIRECTORY")
            .expect("set PVD_NATIVE_ORIGIN_METADATA_DIRECTORY"),
    );
    for filename in ["baseline-paired-native.pxd2", "rich-paired-native.pxd2"] {
        let bytes = std::fs::read(directory.join(filename)).unwrap();
        let layout = Layout::parse(&bytes, &mut || true).unwrap();
        for id in [84, 86] {
            assert!(accepted(&layout.metadata, id));
        }
        for (id, width, expected) in [
            (73, 4, &[][..]),
            (82, 12, &[(6, 73, 0), (7, 73, 4), (8, 73, 8)][..]),
            (84, 24, &[(17, 82, 0), (18, 82, 12)][..]),
            (
                85,
                16,
                &[(13, 73, 0), (14, 73, 4), (15, 73, 8), (16, 73, 12)][..],
            ),
            (86, 28, &[(19, 85, 0), (20, 82, 16)][..]),
        ] {
            let class = layout.metadata.class(id).unwrap();
            assert_eq!(class.size, width);
            assert!(!class.has_pointers);
            assert!(layout.pointer_layouts[id as usize].is_empty());
            let actual = class
                .properties
                .iter()
                .map(|&id| {
                    let property = layout.metadata.property(id).unwrap();
                    (id, property.datatype_id, property.offset32)
                })
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{filename}: class {id}");
        }
        assert!(
            !layout
                .events
                .iter()
                .any(|event| matches!(event, HistoryEvent::OriginShift(_)))
        );
    }
}
