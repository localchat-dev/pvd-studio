use super::*;
use crate::frame_sources::FrameSection;

fn source(id: u64) -> FrameSourceKey {
    FrameSourceKey::Captured {
        section: FrameSection::PhysX,
        source: id,
    }
}
fn point(x: f32) -> Arc<[Primitive]> {
    vec![Primitive::Point {
        instance: 7,
        position: [x, 2.0, 3.0],
        color: 27,
    }]
    .into()
}
fn xs(draw: &DrawMap) -> Vec<f32> {
    draw.values()
        .flat_map(|values| values.iter())
        .map(|primitive| match primitive {
            Primitive::Point { position, .. } => position[0],
            _ => panic!("unexpected primitive"),
        })
        .collect()
}
fn prepare(history: &mut DrawHistory, id: u64) -> DrawMap {
    history
        .prepare(source(id), 1 << 20, false, 1 << 20)
        .unwrap()
}

#[test]
fn each_source_keeps_its_own_previous_end_window() {
    let mut history = DrawHistory::default();
    history.append(7, point(1.0), 1 << 20, false).unwrap();
    let a = prepare(&mut history, 100);
    assert_eq!(xs(&a), [1.0]);
    history.commit(source(100));
    history.append(7, point(2.0), 1 << 20, false).unwrap();
    let b = prepare(&mut history, 200);
    assert_eq!(xs(&b), [1.0, 2.0]);
    history.commit(source(200));
    history.append(7, point(11.0), 1 << 20, false).unwrap();
    assert_eq!(xs(&prepare(&mut history, 100)), [2.0, 11.0]);
    history.commit(source(100));
    history.append(7, point(12.0), 1 << 20, false).unwrap();
    assert_eq!(xs(&prepare(&mut history, 200)), [11.0, 12.0]);
}

#[test]
fn projection_reuses_identical_views_and_preparation_does_not_advance_cursors() {
    let mut history = DrawHistory::default();
    history.append(7, point(1.0), 1 << 20, false).unwrap();
    let first = prepare(&mut history, 100);
    let bytes = history.retained_bytes();
    let repeated = prepare(&mut history, 100);
    assert!(Arc::ptr_eq(&first[&7], &repeated[&7]));
    assert_eq!(history.retained_bytes(), bytes);
    history.commit(source(100));
    let other = prepare(&mut history, 200);
    assert!(Arc::ptr_eq(&first[&7], &other[&7]));
    assert_eq!(history.cursors[&source(200)], 0);
}

#[test]
fn late_first_source_keeps_prefix_until_reclaimed_and_refuses_a_lost_prefix() {
    let mut history = DrawHistory::default();
    let mut held = Vec::new();
    for value in [1.0, 2.0, 3.0] {
        history.append(7, point(value), 1 << 20, false).unwrap();
        held.push(prepare(&mut history, 100));
        history.commit(source(100));
    }
    let late = prepare(&mut history, 200);
    assert_eq!(xs(&late), [1.0, 2.0, 3.0]);
    history.commit(source(200));
    let limit = history.retained_bytes() + SOURCE_BYTES - 1;
    let failure = history
        .prepare(source(300), limit, false, 1 << 20)
        .unwrap_err()
        .to_string();
    assert!(failure.contains("from the start"), "{failure}");
    assert!(history.discarded_before > 0);
    assert!(!history.cursors.contains_key(&source(300)));
}

#[test]
fn stalled_source_blocks_reclamation_instead_of_silently_losing_draws() {
    let mut history = DrawHistory::default();
    let _empty = prepare(&mut history, 200);
    history.commit(source(200));
    history.append(7, point(1.0), 1 << 20, false).unwrap();
    let held = prepare(&mut history, 100);
    history.commit(source(100));
    let extra = primitive_bytes(&point(2.0)) + BATCH_BYTES + OWNER_BYTES;
    let limit = history.retained_bytes() + extra - 1;
    let failure = history
        .append(7, point(2.0), limit, false)
        .unwrap_err()
        .to_string();
    assert!(failure.contains("without a later End"), "{failure}");
    assert_eq!(history.next, 1);
    assert_eq!(history.discarded_before, 0);
    assert_eq!(xs(&held), [1.0]);
    assert_eq!(xs(&prepare(&mut history, 200)), [1.0]);
}

#[test]
fn renderer_destruction_removes_old_owner_batches_without_mutating_saved_views() {
    let mut history = DrawHistory::default();
    history.append(7, point(1.0), 1 << 20, false).unwrap();
    let saved = prepare(&mut history, 100);
    history.commit(source(100));
    history.invalidate_owner(7);
    history.append(7, point(9.0), 1 << 20, false).unwrap();
    assert_eq!(xs(&prepare(&mut history, 200)), [9.0]);
    assert_eq!(xs(&saved), [1.0]);
}

#[test]
fn retained_action_history_keeps_prefix_and_reports_projection_budget() {
    let mut history = DrawHistory::default();
    history.append(7, point(1.0), 1 << 20, true).unwrap();
    let view = history
        .prepare(source(100), 1 << 20, true, 1 << 20)
        .unwrap();
    history.commit(source(100));
    assert!(
        history
            .prepare(source(200), 1 << 20, true, 0)
            .unwrap_err()
            .to_string()
            .contains("projection")
    );
    assert_eq!(history.discarded_before, 0);
    assert_eq!(xs(&view), [1.0]);
    assert!(history.memory_bytes(true) < history.memory_bytes(false));
}

#[test]
fn failed_projection_does_not_register_a_source_that_blocks_reclamation() {
    let mut history = DrawHistory::default();
    history.append(7, point(1.0), 1 << 20, false).unwrap();
    let held = prepare(&mut history, 100);
    history.commit(source(100));
    let failure = history.prepare(source(200), 1 << 20, false, 0).unwrap_err();
    assert!(failure.to_string().contains("projection"));
    assert!(!history.cursors.contains_key(&source(200)));
    assert_eq!(history.cursors[&source(100)], 1);

    let extra = primitive_bytes(&point(2.0)) + BATCH_BYTES + OWNER_BYTES;
    let limit = history.retained_bytes() + extra - 1;
    history.append(7, point(2.0), limit, false).unwrap();
    assert_eq!(history.discarded_before, 1);
    assert_eq!(xs(&held), [1.0]);
    assert_eq!(xs(&prepare(&mut history, 100)), [2.0]);
}

#[test]
fn failed_first_source_projection_keeps_its_prefix_available_for_retry() {
    let mut history = DrawHistory::default();
    history.append(7, point(1.0), 1 << 20, false).unwrap();
    let held = prepare(&mut history, 100);
    history.commit(source(100));
    history.append(7, point(2.0), 1 << 20, false).unwrap();
    let limit = history.retained_bytes() + SOURCE_BYTES;
    let failure = history
        .prepare(source(200), limit, false, 1 << 20)
        .unwrap_err();
    assert!(failure.to_string().contains("retained history budget"));
    assert!(!history.cursors.contains_key(&source(200)));
    assert_eq!(history.discarded_before, 0);
    assert_eq!(xs(&held), [1.0]);
    let first = prepare(&mut history, 200);
    let repeated = prepare(&mut history, 200);
    assert_eq!(xs(&first), [1.0, 2.0]);
    assert!(Arc::ptr_eq(&first[&7], &repeated[&7]));
}

#[test]
fn repeated_two_source_reclamation_preserves_windows_and_saved_views() {
    let mut history = DrawHistory::default();
    let limit = 16 << 10;
    let mut saved = Vec::new();
    for index in 0..1200 {
        let x = index as f32;
        history.append(7, point(x), limit, false).unwrap();
        let a = history.prepare(source(100), limit, false, limit).unwrap();
        assert_eq!(xs(&a), [x]);
        history.commit(source(100));
        if index % 3 == 2 {
            let b = history.prepare(source(200), limit, false, limit).unwrap();
            assert_eq!(xs(&b), [x - 2.0, x - 1.0, x]);
            history.commit(source(200));
        }
        if index < 3 {
            saved.push(a);
        }
        assert!(history.retained_bytes() <= limit);
    }
    assert!(history.discarded_before > 1000);
    assert_eq!(history.cursors[&source(100)], 1200);
    assert_eq!(history.cursors[&source(200)], 1200);
    for (index, view) in saved.iter().enumerate() {
        assert_eq!(xs(view), [index as f32]);
    }
}
