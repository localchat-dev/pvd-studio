use pvd_core::{
    Recording,
    debug_draw::{Pose, Primitive},
    joint_geometry::{self, Line},
};
use std::collections::BTreeSet;

fn vector(value: &serde_json::Value) -> [f32; 3] {
    std::array::from_fn(|i| value[i].as_f64().unwrap() as f32)
}
fn parse_pose(value: &serde_json::Value) -> Pose {
    Pose {
        position: vector(&value["position"]),
        rotation: std::array::from_fn(|i| value["rotation"][i].as_f64().unwrap() as f32),
    }
}
fn parse_lines(value: &serde_json::Value) -> Vec<Line> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|value| Line {
            instance: 0,
            positions: [
                vector(&value["positions"][0]),
                vector(&value["positions"][1]),
            ],
            color: value["color"].as_u64().unwrap() as u32,
        })
        .collect()
}
fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.into_iter().zip(b).all(|(a, b)| (a - b).abs() <= 0.00001)
}
fn compare_geometry(actual: &[Line], expected: &[Line], label: &str) {
    assert_eq!(actual.len(), expected.len(), "{label}: line count");
    let mut matched = vec![false; expected.len()];
    for line in actual {
        let index = expected
            .iter()
            .enumerate()
            .find_map(|(index, candidate)| {
                (!matched[index]
                    && candidate.color == line.color
                    && ((near(candidate.positions[0], line.positions[0])
                        && near(candidate.positions[1], line.positions[1]))
                        || (near(candidate.positions[0], line.positions[1])
                            && near(candidate.positions[1], line.positions[0]))))
                .then_some(index)
            })
            .unwrap_or_else(|| panic!("{label}: no SDK match for {line:?}"));
        matched[index] = true;
    }
}

pub fn verify_sdk_capture(recording: &Recording, truth: &serde_json::Value) {
    for (index, reference) in truth["reference_cases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let pose = parse_pose(&reference["pose"]);
        let active = reference["active"].as_bool().unwrap();
        let number = |name: &str| reference[name].as_f64().unwrap() as f32;
        let primitive = match reference["kind"].as_u64().unwrap() {
            0 => Primitive::LinearLimit {
                instance: 3,
                parent: pose,
                child: Pose {
                    position: [0.0; 3],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                },
                value: number("value"),
                active,
            },
            1 => Primitive::AngularLimit {
                instance: 3,
                pose,
                lower: number("lower"),
                upper: number("upper"),
                active,
            },
            2 => Primitive::LimitCone {
                instance: 3,
                pose,
                y_swing: number("y_swing"),
                z_swing: number("z_swing"),
                active,
            },
            3 => Primitive::DoubleCone {
                instance: 3,
                pose,
                angle: number("angle"),
                active,
            },
            _ => unreachable!(),
        };
        compare_geometry(
            &joint_geometry::tessellate(&primitive).unwrap(),
            &parse_lines(&reference["lines"]),
            &format!("SDK helper case {index}"),
        );
    }
    assert_eq!(recording.frame_count(), 6);
    let expected_joints: BTreeSet<_> = truth["joints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|joint| joint["id"].as_u64().unwrap())
        .collect();
    let mut observed = BTreeSet::new();
    let expected_triangle = &truth["debug_triangle"];
    for frame in (0..recording.frame_count()).rev() {
        let state = recording.state_at(frame).unwrap();
        let mut lines = Vec::new();
        let mut kinds = BTreeSet::new();
        let mut triangles = 0;
        for primitive in state.debug_draw.values().flat_map(|batch| batch.iter()) {
            if let Primitive::Triangle {
                positions, colors, ..
            } = primitive
            {
                triangles += 1;
                for index in 0..3 {
                    assert!(near(
                        positions[index],
                        vector(&expected_triangle["positions"][index])
                    ));
                    assert_eq!(
                        colors[index],
                        expected_triangle["colors"][index].as_u64().unwrap() as u32
                    );
                }
            }
            let kind = match primitive {
                Primitive::LinearLimit { .. } => 0,
                Primitive::AngularLimit { .. } => 1,
                Primitive::LimitCone { .. } => 2,
                Primitive::DoubleCone { .. } => 3,
                _ => continue,
            };
            let geometry = joint_geometry::tessellate(primitive).unwrap();
            observed.extend(geometry.iter().map(|line| line.instance));
            kinds.insert(kind);
            lines.extend(geometry);
        }
        assert_eq!(kinds, BTreeSet::from([0, 1, 2, 3]), "frame {frame}");
        assert_eq!(
            triangles, 1,
            "frame {frame} must contain one SDK debug triangle"
        );
        compare_geometry(
            &lines,
            &parse_lines(&truth["frames"][frame]["lines"]),
            &format!("SDK frame {frame}"),
        );
    }
    assert_eq!(observed, expected_joints);
    recording.verify_replay().unwrap();
    println!(
        "SDK 3.4.2: 8 direct helper cases, 8 real joints, all 4 limit primitives and 6 replayed frames match independent SDK geometry"
    );
}
