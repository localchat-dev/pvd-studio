use super::*;
use pvd_core::{Object, Recording, RecordingBuilder, SceneState, live::SceneUpdate};
use pvd_protocol::{Endian, Event, EventGroup, Header, Name};
use std::sync::Arc;

fn object(id: u64, generation: u32) -> Object {
    Object {
        id,
        generation,
        type_name: "physx3::PxRigidDynamic".into(),
        properties: BTreeMap::new(),
    }
}

fn studio() -> Studio {
    let mut studio = Studio::default();
    for id in 1..=3 {
        studio.state.objects.insert(id, object(id, 0));
    }
    studio
}

fn ids(studio: &Studio) -> Vec<u64> {
    studio.selected_ids().into_iter().collect()
}

#[test]
fn additive_click_toggles_items_preserves_primary_and_plain_click_replaces() {
    let mut studio = studio();
    studio.select_object(Some(1), false);
    studio.select_object(Some(2), true);
    assert_eq!(ids(&studio), [1, 2]);
    assert_eq!(studio.selected, Some(2));
    let revision = studio.selection_revision();
    studio.select_object(Some(1), true);
    assert_eq!(ids(&studio), [2]);
    assert_eq!(studio.selected, Some(2));
    assert_ne!(studio.selection_revision(), revision);
    studio.select_object(Some(3), true);
    studio.select_object(Some(3), true);
    assert_eq!(studio.selected, Some(2));
    studio.select_object(None, true);
    assert_eq!(ids(&studio), [2]);
    studio.select_object(Some(1), true);
    studio.select_object(Some(2), false);
    assert_eq!(ids(&studio), [2]);
    studio.select_object(None, false);
    assert_eq!(studio.selected, None);
    assert!(ids(&studio).is_empty());
}

#[test]
fn legacy_primary_assignment_replaces_the_set_before_the_next_selection() {
    let mut studio = studio();
    studio.select_object(Some(1), false);
    studio.select_object(Some(2), true);
    studio.selected = Some(3);
    assert_eq!(ids(&studio), [3]);
    assert!(!studio.is_selected(1));
    studio.select_object(Some(1), true);
    assert_eq!(ids(&studio), [1, 3]);
    studio.selected = None;
    studio.select_object(Some(2), true);
    assert_eq!(ids(&studio), [2]);
}

#[test]
fn selection_budget_rejects_an_addition_without_dropping_existing_objects() {
    let mut studio = Studio::default();
    let limit = pvd_core::repx::MAX_SELECTED_ACTORS as u64;
    for id in 1..=limit + 1 {
        studio.state.objects.insert(id, object(id, 0));
    }
    studio.selection.objects = (1..=limit).map(|id| (id, 0)).collect();
    studio.selected = Some(limit);
    studio.selection.primary = studio.selected;
    studio.select_object(Some(limit + 1), true);
    assert_eq!(studio.selected, Some(limit));
    assert_eq!(ids(&studio).len(), limit as usize);
    assert!(!studio.is_selected(limit + 1));
    studio.select_object(Some(limit + 1), false);
    assert_eq!(ids(&studio), [limit + 1]);
}

fn recording() -> Recording {
    let mut builder = RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    });
    let class = Name {
        namespace: 1,
        name: 2,
    };
    let mut events: Vec<_> = [(1, "physx3"), (2, "PxRigidDynamic"), (3, "frame")]
        .into_iter()
        .map(|(handle, value)| Event::StringHandle {
            handle,
            value: value.into(),
        })
        .collect();
    events.push(Event::CreateClass { name: class });
    events.extend((1..=3).map(|id| Event::CreateInstance { class, id }));
    for timestamp in 0..4 {
        if timestamp == 2 {
            events.push(Event::DestroyInstance { id: 2 });
        }
        if timestamp == 3 {
            events.push(Event::CreateInstance { class, id: 2 });
        }
        events.push(Event::EndSection {
            id: 1,
            name: 3,
            timestamp,
        });
    }
    builder
        .push(EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        })
        .unwrap();
    builder.finish().unwrap()
}

#[test]
fn forward_reverse_seeks_keep_survivors_and_do_not_reselect_reused_ids() {
    let mut studio = Studio::default();
    studio.recording = Some(Arc::new(recording()));
    studio.seek(0);
    studio.select_object(Some(1), false);
    studio.select_object(Some(2), true);
    studio.select_object(Some(3), true);
    studio.seek(1);
    assert_eq!(ids(&studio), [1, 2, 3]);
    studio.seek(2);
    assert_eq!(ids(&studio), [1, 3]);
    assert_eq!(studio.selected, Some(3));
    studio.seek(3);
    assert_eq!(ids(&studio), [1, 3]);
    studio.select_object(Some(2), true);
    let generation = studio.state.objects[&2].generation;
    studio.seek(0);
    assert_ne!(studio.state.objects[&2].generation, generation);
    assert_eq!(ids(&studio), [1, 3]);
    studio.close();
    assert_eq!(studio.selected, None);
    assert!(studio.selection.objects.is_empty());
}

#[test]
fn tcp_updates_and_return_to_live_keep_only_the_displayed_generations() {
    let recording = recording();
    let mut studio = studio();
    studio.live_session = Some(9);
    studio.live_connected = true;
    studio.follow_live = true;
    studio.select_object(Some(1), false);
    studio.select_object(Some(2), true);
    let mut state = studio.state.clone();
    state.objects.insert(2, object(2, 1));
    studio.accept_live_snapshot(crate::network::LiveSnapshot {
        session: 9,
        frame: 1,
        source_frame: None,
        display_updated: true,
        recording: recording.clone(),
        update: SceneUpdate::full(&state),
    });
    assert_eq!(ids(&studio), [1]);
    studio.select_object(Some(2), true);
    studio.pause_display();
    let paused = studio.state.clone();
    let live = SceneState::default();
    studio.accept_live_snapshot(crate::network::LiveSnapshot {
        session: 9,
        frame: 2,
        source_frame: None,
        display_updated: true,
        recording,
        update: SceneUpdate::full(&live),
    });
    assert_eq!(studio.state, paused);
    assert_eq!(ids(&studio), [1, 2]);
    studio.return_to_live();
    assert!(ids(&studio).is_empty());
    studio.state = paused;
    studio.select_object(Some(1), false);
    studio.select_object(Some(2), true);
    studio.connect_live_session(10);
    assert_eq!(studio.selected, None);
    assert!(studio.selection.objects.is_empty());
}

#[test]
fn batch_export_carries_every_selected_identity() {
    let mut studio = studio();
    studio.state.objects.get_mut(&2).unwrap().generation = 7;
    studio.select_object(Some(1), false);
    studio.select_object(Some(2), true);
    let pvd_core::repx::Selection::Actors(keys) = studio.repx_selection() else {
        panic!()
    };
    assert_eq!(
        keys,
        [
            pvd_core::repx::ActorKey {
                id: 1,
                generation: 0
            },
            pvd_core::repx::ActorKey {
                id: 2,
                generation: 7
            },
        ]
    );
}
