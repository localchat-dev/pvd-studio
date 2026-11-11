use super::*;

fn source(created_in_range: bool) -> Fixture {
    let mut source = fixture();
    let sections = [
        source.events[3].clone(),
        source.events[5].clone(),
        source.events[9].clone(),
        source.events[15].clone(),
        source.events[16].clone(),
        source.events[22].clone(),
        source.events[24].clone(),
        source.events[25].clone(),
    ];
    let target = if created_in_range {
        [28u32.to_le_bytes().as_slice(), &[0; 28], &[0; 4]].concat()
    } else {
        body(70, &[], 1, 0xbb112233)
    };
    source.records = vec![
        Record {
            id: 1,
            class: 0,
            position: 128,
            body: 0u32.to_le_bytes().to_vec(),
            deleted: false,
        },
        Record {
            id: 2,
            class: 4,
            position: 256,
            body: body(7, &[], if created_in_range { 1 } else { 6 }, 0xaa112233),
            deleted: false,
        },
        Record {
            id: 6,
            class: 4,
            position: 512,
            body: target.clone(),
            deleted: false,
        },
        Record {
            id: 6,
            class: 4,
            position: 1024,
            body: target,
            deleted: true,
        },
        Record {
            id: 2,
            class: 4,
            position: 1280,
            body: body(7, &[], 6, 0xaa112233),
            deleted: true,
        },
    ];
    source.initial = if created_in_range {
        source.records.remove(2);
        vec![1, 0]
    } else {
        vec![2, 1, 0]
    };
    source.events = vec![
        9u32.to_le_bytes().to_vec(),
        sections[0].clone(),
        sections[1].clone(),
        sections[2].clone(),
    ];
    if created_in_range {
        source.events.push(create(6, 4));
        source.events.push(set(3, 2, 3, &6u32.to_le_bytes(), &[5]));
    }
    source.events.extend([
        saved(6, 1024),
        destroy(6),
        saved(2, 1280),
        destroy(2),
        sections[3].clone(),
        sections[4].clone(),
        sections[5].clone(),
        sections[6].clone(),
        sections[7].clone(),
    ]);
    source.ids = vec![1, 2, 6];
    source.alive = vec![1];
    source
}

#[test]
fn saved_bodies_preserve_references_to_earlier_destroyed_retained_lifetimes() {
    for created_in_range in [false, true] {
        let source = source(created_in_range);
        let bytes = trim(&source.bytes(), 1, 1);
        let actual = raw(&bytes);
        assert_eq!(actual.initial.contains_key(&6), !created_in_range);
        assert_eq!(
            actual.initial[&2],
            (
                4,
                body(7, &[], if created_in_range { 1 } else { 6 }, 0xaa112233)
            )
        );
        assert_eq!(actual.deleted[&2].2, body(7, &[], 6, 0xaa112233));
        assert_eq!(
            actual.deleted[&6].2,
            source
                .records
                .iter()
                .find(|record| record.id == 6 && record.deleted)
                .unwrap()
                .body
        );
        let saved_events: Vec<_> = actual
            .events
            .iter()
            .filter(|event| event[..4] == 10u32.to_le_bytes())
            .cloned()
            .collect();
        assert_eq!(
            saved_events,
            [
                saved(6, actual.deleted[&6].0),
                saved(2, actual.deleted[&2].0)
            ]
        );
        let state = Recording::from_bytes(&bytes).unwrap().state_at(0).unwrap();
        assert_eq!(state.objects.keys().copied().collect::<Vec<_>>(), [1]);
    }
}

#[test]
fn a_cut_after_the_destruction_chain_drops_its_saved_bodies_and_lifetimes() {
    let source = source(false).bytes();
    let direct = trim(&source, 2, 3);
    let middle = trim(&source, 1, 3);
    let repeated = trim(&middle, 1, 2);
    for bytes in [&direct, &repeated] {
        let actual = raw(bytes);
        assert_eq!(actual.initial.keys().copied().collect::<Vec<_>>(), [1]);
        assert!(actual.deleted.is_empty());
        assert!(
            actual
                .events
                .iter()
                .all(|event| event[..4] != 10u32.to_le_bytes())
        );
        assert_eq!(actual.reverse, [1]);
    }
    assert_eq!(raw(&direct).initial, raw(&repeated).initial);
}

#[test]
fn saved_references_exclude_pre_cut_destroyed_future_and_unknown_lifetimes() {
    for variant in 0..3 {
        let mut source = source(false);
        match variant {
            0 => {
                let destroyed = source.events.drain(4..6).collect::<Vec<_>>();
                source.events.splice(2..2, destroyed);
            }
            1 => {
                source.records[4].body = body(7, &[], 5, 0xaa112233);
                source.events.insert(8, create(5, 4));
                source.ids.push(5);
                source.alive.push(5);
            }
            2 => source.records[4].body = body(7, &[], 999, 0xaa112233),
            _ => unreachable!(),
        }
        let bytes = source.bytes();
        let layout = Layout::parse(&bytes, &mut || true).unwrap();
        let failure = Scope::prepare(&layout, 1, 1, &mut || true).err().unwrap();
        assert!(
            failure.to_string().contains("saved packed reference"),
            "{failure}"
        );
        let mut output = Vec::new();
        assert!(trim_scene_frames(&bytes, 1, 1, &mut output, || true).is_err());
        assert!(output.is_empty());
    }
}

#[test]
fn ordinary_writes_still_require_a_currently_live_reference_target() {
    let mut source = source(false);
    source
        .events
        .insert(6, set(3, 2, 3, &6u32.to_le_bytes(), &[6]));
    fail(&source.bytes(), "event packed reference");
}
