use pvd_core::{
    Recording,
    debug_draw::{Pose, Primitive},
    joint_geometry,
};
use std::{fs, path::PathBuf};

#[path = "support/joint_limits.rs"]
mod sdk_joint_limits;

fn pose() -> Pose {
    Pose {
        position: [0.0; 3],
        rotation: [0.0, 0.0, 0.0, 1.0],
    }
}

#[test]
fn limits_reject_invalid_parameters_and_bound_output() {
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert!(
            joint_geometry::tessellate(&Primitive::LinearLimit {
                instance: 3,
                parent: pose(),
                child: pose(),
                value: invalid,
                active: true
            })
            .is_err()
        );
        assert!(
            joint_geometry::tessellate(&Primitive::AngularLimit {
                instance: 3,
                pose: pose(),
                lower: -1.0,
                upper: invalid,
                active: true
            })
            .is_err()
        );
        assert!(
            joint_geometry::tessellate(&Primitive::LimitCone {
                instance: 3,
                pose: pose(),
                y_swing: invalid,
                z_swing: 0.2,
                active: true
            })
            .is_err()
        );
        assert!(
            joint_geometry::tessellate(&Primitive::DoubleCone {
                instance: 3,
                pose: pose(),
                angle: invalid,
                active: true
            })
            .is_err()
        );
    }
    for invalid in [[0.0; 4], [0.0, 0.0, 0.0, 2.0], [f32::MAX; 4]] {
        let mut invalid_pose = pose();
        invalid_pose.rotation = invalid;
        assert!(
            joint_geometry::tessellate(&Primitive::DoubleCone {
                instance: 3,
                pose: invalid_pose,
                angle: 0.4,
                active: true
            })
            .is_err()
        );
    }
    assert!(
        joint_geometry::tessellate(&Primitive::DoubleCone {
            instance: 3,
            pose: pose(),
            angle: std::f32::consts::FRAC_PI_2,
            active: true
        })
        .is_err()
    );
    assert!(
        joint_geometry::tessellate(&Primitive::AngularLimit {
            instance: 3,
            pose: pose(),
            lower: 1.0,
            upper: -1.0,
            active: false
        })
        .is_err()
    );
    assert!(
        joint_geometry::tessellate(&Primitive::LimitCone {
            instance: 3,
            pose: pose(),
            y_swing: -0.1,
            z_swing: 0.1,
            active: false
        })
        .is_err()
    );
    for angle in [0.0, 0.1, 0.5, 1.0, 2.0, 3.0] {
        let lines = joint_geometry::tessellate(&Primitive::DoubleCone {
            instance: 3,
            pose: pose(),
            angle,
            active: false,
        })
        .unwrap();
        assert_eq!(lines.len(), joint_geometry::MAX_LINES_PER_LIMIT);
        assert!(lines.iter().all(|line| line.instance == 3
            && line.color == 0xff808080
            && line.positions.iter().flatten().all(|v| v.is_finite())));
    }
}

#[test]
fn extreme_and_degenerate_linear_limits_do_not_produce_invalid_vertices() {
    for value in [0.0, f32::MIN_POSITIVE, -f32::MIN_POSITIVE] {
        let lines = joint_geometry::tessellate(&Primitive::LinearLimit {
            instance: 42,
            parent: pose(),
            child: pose(),
            value,
            active: true,
        })
        .unwrap();
        assert!(!lines.is_empty());
        assert!(lines.len() <= joint_geometry::MAX_LINES_PER_LIMIT);
        assert!(lines.iter().all(|line| line.instance == 42
            && line.color == 0xffff0000
            && line.positions.iter().flatten().all(|v| v.is_finite())));
    }
    assert!(
        joint_geometry::tessellate(&Primitive::LinearLimit {
            instance: 42,
            parent: pose(),
            child: pose(),
            value: f32::MAX,
            active: true,
        })
        .is_err()
    );
    let mut invalid_pose = pose();
    invalid_pose.position[2] = f32::NAN;
    assert!(
        joint_geometry::tessellate(&Primitive::AngularLimit {
            instance: 42,
            pose: invalid_pose,
            lower: -0.5,
            upper: 0.5,
            active: true,
        })
        .is_err()
    );
    assert!(
        joint_geometry::tessellate(&Primitive::Point {
            instance: 42,
            position: [0.0; 3],
            color: 0,
        })
        .is_err()
    );
}

#[test]
#[ignore = "requires official SDK joint-limit producer capture and independent Cm helper/render buffer truth"]
fn sdk_limits_match_official_helpers_and_real_joint_render_buffers() {
    let capture = std::env::var_os("PVD_JOINT_LIMIT_CAPTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../captures/physx-3.4-joint-limits.pvdstream")
        });
    let truth: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(capture.with_extension("joints.json")).unwrap())
            .unwrap();
    let recording = Recording::from_path(&capture).unwrap();
    sdk_joint_limits::verify_sdk_capture(&recording, &truth);
}
