use std::collections::{HashMap, HashSet};

use pvd_core::{Object, SceneState, Value, live::SceneUpdate};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityCounts {
    pub total_actors: usize,
    pub awake_dynamic: usize,
    pub sleeping_dynamic: usize,
    pub kinematic: usize,
    pub static_actors: usize,
    pub unknown: usize,
    pub articulation_links: usize,
    pub sdk_scene_count: usize,
    pub sdk_reporting_scenes: usize,
    pub sdk_active_dynamic: Option<u64>,
    pub sdk_active_kinematic: Option<u64>,
}

impl ActivityCounts {
    pub fn sdk_active_total(&self) -> Option<u64> {
        self.sdk_active_dynamic?
            .checked_add(self.sdk_active_kinematic?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Awake,
    Sleeping,
    Kinematic,
    Static,
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
struct Actor {
    category: Category,
    articulation_link: bool,
    dependencies: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SceneStatistics {
    dynamic: Option<u64>,
    kinematic: Option<u64>,
}

#[derive(Debug, Default)]
pub struct ActivityCache {
    initialized: bool,
    counts: ActivityCounts,
    actors: HashMap<u64, Actor>,
    dependents: HashMap<u64, HashSet<u64>>,
    scenes: HashMap<u64, SceneStatistics>,
    dynamic_sum: u128,
    kinematic_sum: u128,
    missing_dynamic: usize,
    missing_kinematic: usize,
}

impl ActivityCache {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn rebuild(&mut self, state: &SceneState) {
        self.clear();
        for (&id, object) in &state.objects {
            if let Some(actor) = classify(state, object) {
                self.insert_actor(id, actor);
            }
            if let Some(statistics) = scene_statistics(object) {
                self.insert_scene(id, statistics);
            }
        }
        self.initialized = true;
    }

    /// The state must already contain the assignments in this update.
    #[cfg(test)]
    pub fn update(&mut self, update: &SceneUpdate, state: &SceneState) {
        if !self.initialized || update.replace_all {
            self.rebuild(state);
        } else {
            self.update_objects(update.changed_objects(), state);
        }
    }

    pub fn apply_update(&mut self, update: SceneUpdate, state: &mut SceneState) {
        if !self.initialized || update.replace_all {
            update.apply_owned(state);
            self.rebuild(state);
        } else {
            let changed: Vec<_> = update.changed_objects().map(|(id, _)| id).collect();
            update.apply_owned(state);
            self.update_ids(changed, state);
        }
    }

    fn update_ids(&mut self, changed: impl IntoIterator<Item = u64>, state: &SceneState) {
        self.update_objects(
            changed.into_iter().map(|id| (id, state.objects.get(&id))),
            state,
        );
    }

    fn update_objects<'a>(
        &mut self,
        changed: impl IntoIterator<Item = (u64, Option<&'a Object>)>,
        state: &SceneState,
    ) {
        let mut affected = HashSet::new();
        for (id, object) in changed {
            if let Some(links) = self.dependents.get(&id) {
                affected.extend(links);
            }
            self.replace_actor(id, object.and_then(|object| classify(state, object)));
            let statistics = object.and_then(scene_statistics);
            if self.scenes.get(&id).copied() != statistics {
                self.remove_scene(id);
                if let Some(statistics) = statistics {
                    self.insert_scene(id, statistics);
                }
            }
        }
        for id in affected {
            let next = state
                .objects
                .get(&id)
                .and_then(|object| classify(state, object));
            self.replace_actor(id, next);
        }
    }

    fn replace_actor(&mut self, id: u64, next: Option<Actor>) {
        if self.actors.get(&id) == next.as_ref() {
            return;
        }
        self.remove_actor(id);
        if let Some(actor) = next {
            self.insert_actor(id, actor);
        }
    }

    pub fn summary(&self) -> ActivityCounts {
        let mut counts = self.counts;
        counts.sdk_scene_count = self.scenes.len();
        // SDK step statistics remain distinct from captured Actor sleep state.
        // A missing scene's statistics must not turn a partial sum into a total.
        if !self.scenes.is_empty() {
            counts.sdk_active_dynamic = (self.missing_dynamic == 0)
                .then(|| u64::try_from(self.dynamic_sum).ok())
                .flatten();
            counts.sdk_active_kinematic = (self.missing_kinematic == 0)
                .then(|| u64::try_from(self.kinematic_sum).ok())
                .flatten();
        }
        counts
    }

    pub fn summary_scoped(&self, scope: &crate::room_scope::RoomScope) -> ActivityCounts {
        if scope.room().is_none() {
            return self.summary();
        }
        let mut counts = ActivityCounts::default();
        for actor in scope.actors().iter().filter_map(|id| self.actors.get(id)) {
            counts.total_actors += 1;
            counts.articulation_links += usize::from(actor.articulation_link);
            match actor.category {
                Category::Awake => counts.awake_dynamic += 1,
                Category::Sleeping => counts.sleeping_dynamic += 1,
                Category::Kinematic => counts.kinematic += 1,
                Category::Static => counts.static_actors += 1,
                Category::Unknown => counts.unknown += 1,
            }
        }
        for statistics in scope.scenes().iter().filter_map(|id| self.scenes.get(id)) {
            counts.sdk_scene_count += 1;
            counts.sdk_reporting_scenes +=
                usize::from(statistics.dynamic.is_some() && statistics.kinematic.is_some());
            counts.sdk_active_dynamic = statistics.dynamic;
            counts.sdk_active_kinematic = statistics.kinematic;
        }
        counts
    }

    fn category_count(&mut self, category: Category) -> &mut usize {
        match category {
            Category::Awake => &mut self.counts.awake_dynamic,
            Category::Sleeping => &mut self.counts.sleeping_dynamic,
            Category::Kinematic => &mut self.counts.kinematic,
            Category::Static => &mut self.counts.static_actors,
            Category::Unknown => &mut self.counts.unknown,
        }
    }

    fn insert_actor(&mut self, id: u64, actor: Actor) {
        *self.category_count(actor.category) += 1;
        self.counts.total_actors += 1;
        self.counts.articulation_links += usize::from(actor.articulation_link);
        for &dependency in &actor.dependencies {
            self.dependents.entry(dependency).or_default().insert(id);
        }
        self.actors.insert(id, actor);
    }

    fn remove_actor(&mut self, id: u64) {
        let Some(actor) = self.actors.remove(&id) else {
            return;
        };
        *self.category_count(actor.category) -= 1;
        self.counts.total_actors -= 1;
        self.counts.articulation_links -= usize::from(actor.articulation_link);
        for dependency in actor.dependencies {
            if let Some(links) = self.dependents.get_mut(&dependency) {
                links.remove(&id);
                if links.is_empty() {
                    self.dependents.remove(&dependency);
                }
            }
        }
    }

    fn insert_scene(&mut self, id: u64, statistics: SceneStatistics) {
        self.dynamic_sum += u128::from(statistics.dynamic.unwrap_or(0));
        self.kinematic_sum += u128::from(statistics.kinematic.unwrap_or(0));
        self.missing_dynamic += usize::from(statistics.dynamic.is_none());
        self.missing_kinematic += usize::from(statistics.kinematic.is_none());
        self.counts.sdk_reporting_scenes +=
            usize::from(statistics.dynamic.is_some() && statistics.kinematic.is_some());
        self.scenes.insert(id, statistics);
    }

    fn remove_scene(&mut self, id: u64) {
        let Some(statistics) = self.scenes.remove(&id) else {
            return;
        };
        self.dynamic_sum -= u128::from(statistics.dynamic.unwrap_or(0));
        self.kinematic_sum -= u128::from(statistics.kinematic.unwrap_or(0));
        self.missing_dynamic -= usize::from(statistics.dynamic.is_none());
        self.missing_kinematic -= usize::from(statistics.kinematic.is_none());
        self.counts.sdk_reporting_scenes -=
            usize::from(statistics.dynamic.is_some() && statistics.kinematic.is_some());
    }
}

fn class_name(object: &Object) -> &str {
    object.type_name.rsplit("::").next().unwrap_or("")
}

fn unsigned_u32(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::U64(value) => u32::try_from(*value).ok().map(u64::from),
        Value::I64(value) => u32::try_from(*value).ok().map(u64::from),
        _ => None,
    }
}

fn sleeping(object: &Object) -> Category {
    match object.properties.get("IsSleeping") {
        Some(Value::Bool(false)) => Category::Awake,
        Some(Value::Bool(true)) => Category::Sleeping,
        _ => Category::Unknown,
    }
}

fn classify(state: &SceneState, object: &Object) -> Option<Actor> {
    let mut dependencies = Vec::new();
    let articulation_link = class_name(object) == "PxArticulationLink";
    let category = match class_name(object) {
        "PxRigidStatic" => Category::Static,
        "PxRigidDynamic" => match unsigned_u32(object.properties.get("RigidBodyFlags")) {
            Some(flags) if flags & 1 != 0 => Category::Kinematic,
            Some(_) => sleeping(object),
            None => Category::Unknown,
        },
        "PxArticulationLink" => {
            let mut current = object;
            let mut category = Category::Unknown;
            // The SDK limits an articulation to 64 links. Reference generations
            // must still be checked when a parent ID has been destroyed and reused.
            for _ in 0..64 {
                let Some(Value::Ref(parent_id)) = current.properties.get("Parent") else {
                    break;
                };
                if dependencies.contains(parent_id) {
                    break;
                }
                dependencies.push(*parent_id);
                let Some(parent) = state.resolve_reference(current.id, "Parent", *parent_id) else {
                    break;
                };
                match class_name(parent) {
                    "PxArticulation" | "PxArticulationBase" => {
                        category = sleeping(parent);
                        break;
                    }
                    "PxArticulationLink" => current = parent,
                    _ => break,
                }
            }
            category
        }
        _ => return None,
    };
    Some(Actor {
        category,
        articulation_link,
        dependencies,
    })
}

fn scene_statistics(object: &Object) -> Option<SceneStatistics> {
    (class_name(object) == "PxScene").then(|| SceneStatistics {
        dynamic: unsigned_u32(
            object
                .properties
                .get("SimulationStatistics.NbActiveDynamicBodies"),
        ),
        kinematic: unsigned_u32(
            object
                .properties
                .get("SimulationStatistics.NbActiveKinematicBodies"),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pvd_core::{Recording, RecordingBuilder};
    use pvd_protocol::{Endian, Event, EventGroup, Header, Name};

    fn object(id: u64, class: &str, properties: &[(&str, Value)]) -> Object {
        Object {
            id,
            generation: 0,
            type_name: format!("physx3::{class}"),
            properties: properties
                .iter()
                .map(|(name, value)| ((*name).into(), value.clone()))
                .collect(),
        }
    }

    fn dynamic(id: u64, sleeping: bool) -> Object {
        object(
            id,
            "PxRigidDynamic",
            &[
                ("RigidBodyFlags", Value::U64(0)),
                ("IsSleeping", Value::Bool(sleeping)),
            ],
        )
    }

    fn scene(id: u64, dynamic: u64, kinematic: u64) -> Object {
        object(
            id,
            "PxScene",
            &[
                (
                    "SimulationStatistics.NbActiveDynamicBodies",
                    Value::U64(dynamic),
                ),
                (
                    "SimulationStatistics.NbActiveKinematicBodies",
                    Value::U64(kinematic),
                ),
            ],
        )
    }

    fn state(objects: impl IntoIterator<Item = Object>) -> SceneState {
        let mut state = SceneState::default();
        state
            .objects
            .extend(objects.into_iter().map(|object| (object.id, object)));
        state
    }

    fn assert_matches_rebuild(cache: &ActivityCache, state: &SceneState) {
        let mut fresh = ActivityCache::default();
        fresh.rebuild(state);
        assert_eq!(cache.summary(), fresh.summary());
        let counts = cache.summary();
        assert_eq!(
            counts.total_actors,
            counts.awake_dynamic
                + counts.sleeping_dynamic
                + counts.kinematic
                + counts.static_actors
                + counts.unknown
        );
    }

    #[test]
    fn shapes_do_not_multiply_actors_and_missing_fields_remain_unknown() {
        let state = state([
            dynamic(1, false),
            dynamic(2, true),
            object(3, "PxRigidDynamic", &[("RigidBodyFlags", Value::U64(1))]),
            object(4, "PxRigidStatic", &[]),
            object(5, "PxRigidDynamic", &[("IsSleeping", Value::Bool(false))]),
            object(6, "PxRigidDynamic", &[("RigidBodyFlags", Value::U64(0))]),
            object(
                7,
                "PxRigidDynamic",
                &[
                    ("RigidBodyFlags", Value::I64(-1)),
                    ("IsSleeping", Value::Bool(false)),
                ],
            ),
            object(
                8,
                "PxRigidDynamic",
                &[
                    ("RigidBodyFlags", Value::U64(0)),
                    ("IsSleeping", Value::U64(0)),
                ],
            ),
            object(9, "PxShape", &[("Actor", Value::Ref(1))]),
            object(10, "PxShape", &[("Actor", Value::Ref(1))]),
            object(11, "PxParticleSystem", &[]),
            object(12, "PxCloth", &[]),
            object(13, "PxArticulationLink", &[]),
        ]);
        let mut cache = ActivityCache::default();
        cache.update(&SceneUpdate::full(&state), &state);
        assert_eq!(
            cache.summary(),
            ActivityCounts {
                total_actors: 9,
                awake_dynamic: 1,
                sleeping_dynamic: 1,
                kinematic: 1,
                static_actors: 1,
                unknown: 5,
                articulation_links: 1,
                ..ActivityCounts::default()
            }
        );
        assert_matches_rebuild(&cache, &state);
    }

    #[test]
    fn scene_totals_require_complete_statistics_and_never_fall_back_to_sleep() {
        let mut state = state([scene(100, 2, 0), scene(101, 0, 2), dynamic(1, false)]);
        let mut cache = ActivityCache::default();
        cache.rebuild(&state);
        assert_eq!(cache.summary().sdk_active_total(), Some(4));
        assert_eq!(cache.summary().awake_dynamic, 1);
        state
            .objects
            .get_mut(&101)
            .unwrap()
            .properties
            .remove("SimulationStatistics.NbActiveDynamicBodies");
        cache.update_ids([101], &state);
        assert_eq!(cache.summary().sdk_active_dynamic, None);
        assert_eq!(cache.summary().sdk_active_kinematic, Some(2));
        assert_eq!(cache.summary().sdk_reporting_scenes, 1);
        assert_eq!(cache.summary().sdk_active_total(), None);
        state.objects.get_mut(&101).unwrap().properties.insert(
            "SimulationStatistics.NbActiveKinematicBodies".into(),
            Value::I64(-1),
        );
        cache.update_ids([101], &state);
        assert_eq!(cache.summary().sdk_active_kinematic, None);
        state.objects.remove(&101);
        cache.update_ids([101], &state);
        assert_eq!(cache.summary().sdk_active_total(), Some(2));
        assert_eq!(cache.summary().sdk_scene_count, 1);
        state.objects.remove(&100);
        cache.update_ids([100], &state);
        assert_eq!(cache.summary().sdk_active_total(), None);
        assert_matches_rebuild(&cache, &state);
    }

    #[test]
    fn incremental_destruction_type_reuse_and_replace_all_remove_previous_counts() {
        let mut state = state([dynamic(1, false), dynamic(2, true), scene(100, 1, 0)]);
        let mut cache = ActivityCache::default();
        cache.rebuild(&state);
        state.objects.remove(&1);
        cache.update_ids([1], &state);
        assert_eq!(cache.summary().awake_dynamic, 0);
        let mut reused = object(1, "PxRigidStatic", &[]);
        reused.generation = 1;
        state.objects.insert(1, reused);
        state.objects.insert(2, object(2, "PxShape", &[]));
        state.objects.insert(100, dynamic(100, false));
        cache.update_ids([1, 2, 100, 1], &state);
        assert_eq!(cache.summary().total_actors, 2);
        assert_eq!(cache.summary().static_actors, 1);
        assert_eq!(cache.summary().sdk_scene_count, 0);
        assert_matches_rebuild(&cache, &state);
        state.objects.clear();
        cache.update(&SceneUpdate::full(&state), &state);
        assert_eq!(cache.summary(), ActivityCounts::default());
        cache.clear();
        assert_eq!(cache.summary(), ActivityCounts::default());
    }

    fn articulation_recording(root_type: &str) -> Recording {
        let mut builder = RecordingBuilder::new(Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        });
        let name = |namespace, name| Name { namespace, name };
        let parent = |id, target: u64| Event::SetPropertyValue {
            id,
            property: 4,
            datatype: name(0, 7),
            items: 1,
            data: target.to_le_bytes().to_vec().into(),
        };
        let sleep = |id, value| Event::SetPropertyValue {
            id,
            property: 5,
            datatype: name(0, 6),
            items: 1,
            data: vec![value].into(),
        };
        let create = |id, class| Event::CreateInstance {
            class: name(1, class),
            id,
        };
        let mut initial: Vec<_> = [
            "physx3",
            root_type,
            "PxArticulationLink",
            "Parent",
            "IsSleeping",
            "PvdBool",
            "ObjectRef",
            "frame",
            "PxRigidStatic",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, value)| Event::StringHandle {
            handle: index as u32 + 1,
            value: value.into(),
        })
        .collect();
        initial.extend([
            create(10, 2),
            create(20, 3),
            create(21, 3),
            parent(20, 10),
            parent(21, 20),
            sleep(10, 0),
            sleep(20, 1),
        ]);
        let frames = [
            initial,
            vec![sleep(10, 1)],
            vec![
                Event::DestroyInstance { id: 10 },
                create(10, 2),
                sleep(10, 0),
            ],
            vec![parent(20, 10)],
            vec![Event::DestroyInstance { id: 20 }, create(20, 9)],
            vec![parent(21, 10)],
            vec![parent(21, 21)],
        ];
        for (frame, mut events) in frames.into_iter().enumerate() {
            events.push(Event::EndSection {
                id: 100,
                name: 8,
                timestamp: frame as u64,
            });
            builder
                .push(EventGroup {
                    stream_id: 1,
                    timestamp: frame as u64,
                    offset: 0,
                    events,
                })
                .unwrap();
        }
        builder.finish().unwrap()
    }

    #[test]
    fn consuming_updates_match_borrowed_activity_for_parent_reuse_and_multiple_scenes() {
        let recording = articulation_recording("PxArticulation");
        let mut borrowed = SceneState::default();
        let mut owned = SceneState::default();
        let mut borrowed_cache = ActivityCache::default();
        let mut owned_cache = ActivityCache::default();
        for (sample, frame) in [0, 1, 2, 3, 4, 5, 6, 3, 1, 0].into_iter().enumerate() {
            let mut next = recording.state_at(frame).unwrap();
            next.objects.insert(1000, scene(1000, sample as u64, 1));
            next.objects.insert(1001, scene(1001, 2, 3));
            if sample == 5 {
                next.objects
                    .get_mut(&1001)
                    .unwrap()
                    .properties
                    .remove("SimulationStatistics.NbActiveDynamicBodies");
            }
            let mut update = SceneUpdate::full(&next);
            update.replace_all = sample == 0 || sample == 7;
            update.apply(&mut borrowed);
            borrowed_cache.update(&update, &borrowed);
            let retained = (sample % 2 == 0).then(|| update.clone());
            owned_cache.apply_update(update, &mut owned);
            assert_eq!(owned, borrowed, "frame {frame}");
            assert_eq!(owned_cache.summary(), borrowed_cache.summary());
            assert_eq!(owned_cache.actors, borrowed_cache.actors);
            assert_eq!(owned_cache.dependents, borrowed_cache.dependents);
            assert_eq!(owned_cache.scenes, borrowed_cache.scenes);
            assert_matches_rebuild(&owned_cache, &owned);
            drop(retained);
        }
        let initial = recording.state_at(0).unwrap();
        owned_cache.apply_update(SceneUpdate::full(&initial), &mut owned);
        let sleeping = recording.state_at(1).unwrap();
        let changed_parent = state([sleeping.objects[&10].clone()]);
        let mut update = SceneUpdate::full(&changed_parent);
        update.replace_all = false;
        assert_eq!(update.changed_objects().count(), 1);
        owned_cache.apply_update(update, &mut owned);
        assert_eq!(owned_cache.summary().sleeping_dynamic, 2);
        assert_eq!(owned, sleeping);
        assert_matches_rebuild(&owned_cache, &owned);
        owned_cache.apply_update(SceneUpdate::full(&SceneState::default()), &mut owned);
        assert_eq!(owned_cache.summary(), ActivityCounts::default());
        assert!(owned.objects.is_empty());
    }

    #[test]
    fn articulation_owner_changes_propagate_through_parent_chains_and_generations() {
        for root_type in ["PxArticulation", "PxArticulationBase"] {
            let recording = articulation_recording(root_type);
            let mut cache = ActivityCache::default();
            cache.rebuild(&recording.state_at(0).unwrap());
            assert_eq!(cache.summary().awake_dynamic, 2);
            assert_eq!(cache.summary().sleeping_dynamic, 0);
            let mut unknown = recording.state_at(0).unwrap();
            unknown
                .objects
                .get_mut(&10)
                .unwrap()
                .properties
                .remove("IsSleeping");
            cache.update_ids([10], &unknown);
            assert_eq!(cache.summary().unknown, 2);
            assert_eq!(cache.summary().awake_dynamic, 0);
            assert_matches_rebuild(&cache, &unknown);
            cache.rebuild(&recording.state_at(0).unwrap());
            let expected = [
                (1, 10, 0, 2, 0),
                (2, 10, 0, 0, 2),
                (3, 20, 2, 0, 0),
                (4, 20, 0, 0, 1),
                (5, 21, 1, 0, 0),
                (6, 21, 0, 0, 1),
            ];
            for (frame, changed, awake, sleeping, unknown) in expected {
                let state = recording.state_at(frame).unwrap();
                cache.update_ids([changed], &state);
                assert_eq!(
                    (
                        cache.summary().awake_dynamic,
                        cache.summary().sleeping_dynamic,
                        cache.summary().unknown
                    ),
                    (awake, sleeping, unknown),
                    "frame {frame}"
                );
                assert_matches_rebuild(&cache, &state);
            }
            assert_eq!(cache.summary().static_actors, 1);
            assert_eq!(cache.summary().articulation_links, 1);
        }
    }

    #[test]
    #[ignore = "requires tools/physx41-articulation capture and same-run SDK getter truth"]
    fn captured_sdk41_articulation_activity_uses_captured_base_sleep() {
        let capture = std::path::PathBuf::from(
            std::env::var_os("PVD_ARTICULATION41_CAPTURE").expect("set PVD_ARTICULATION41_CAPTURE"),
        );
        let truth: serde_json::Value = serde_json::from_slice(
            &std::fs::read(capture.with_extension("articulation.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(truth["sdk"], "4.1.2");
        let frames = truth["frames"].as_array().unwrap();
        assert_eq!(frames.len(), 12);
        let recording = Recording::from_path(capture).unwrap();
        assert_eq!(recording.frame_count(), frames.len());
        let mut cache = ActivityCache::default();
        cache.rebuild(&recording.state_at(0).unwrap());
        let mut prior = std::collections::BTreeSet::new();
        for frame in (0..frames.len())
            .chain((0..frames.len()).rev())
            .chain([2, 9, 5, 11, 0, 7])
        {
            let state = recording.state_at(frame).unwrap();
            let current: std::collections::BTreeSet<_> = state.objects.keys().copied().collect();
            let changed: Vec<_> = prior.union(&current).copied().collect();
            cache.update_ids(changed, &state);
            prior = current;
            let expected =
                (!frames[frame]["after_fetch"].is_null()).then_some(&frames[frame]["after_fetch"]);
            let (awake, sleeping) = expected.map_or((0, 0), |expected| {
                let count = expected["links"].as_array().unwrap().len();
                if expected["sleeping"].as_bool().unwrap() {
                    (0, count)
                } else {
                    (count, 0)
                }
            });
            let counts = cache.summary();
            assert_eq!(
                (counts.awake_dynamic, counts.sleeping_dynamic),
                (awake, sleeping),
                "frame {frame}"
            );
            assert_eq!(counts.unknown, 0, "frame {frame}");
            assert_eq!(counts.total_actors, awake + sleeping);
            assert_eq!(counts.articulation_links, awake + sleeping);
            assert_matches_rebuild(&cache, &state);
        }
    }

    #[test]
    #[ignore = "requires generated real PhysX 3.4 baseline and CCT captures"]
    fn captured_sdk_statistics_and_actor_sleep_are_distinct() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let recording = Recording::from_path(root.join("captures/physx-3.4.pvdstream")).unwrap();
        let mut cache = ActivityCache::default();
        for frame in [77, 103] {
            cache.rebuild(&recording.state_at(frame).unwrap());
            assert_eq!(cache.summary().awake_dynamic, 1, "frame {frame}");
            assert_eq!(cache.summary().sdk_active_dynamic, Some(2), "frame {frame}");
        }
        let cct = Recording::from_path(root.join("captures/physx-3.4-cct.pvdstream")).unwrap();
        cache.rebuild(&cct.state_at(1).unwrap());
        assert_eq!(cache.summary().sdk_active_dynamic, Some(0));
        assert_eq!(cache.summary().sdk_active_kinematic, Some(2));
        assert_eq!(cache.summary().kinematic, 2);

        let mut live = pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig {
            directory: root.join("output/activity-evidence/history"),
            min_free_disk_bytes: u64::MAX,
            ..Default::default()
        })
        .unwrap();
        let mut current = SceneState::default();
        cache.clear();
        let mut updates = 0;
        for bytes in std::fs::read(root.join("captures/physx-3.4.pvdstream"))
            .unwrap()
            .chunks(1024)
        {
            live.feed(bytes).unwrap();
            if let Some((_, patch)) = live.take_update() {
                patch.apply(&mut current);
                cache.update(&patch, &current);
                assert_matches_rebuild(&cache, &current);
                updates += 1;
            }
        }
        live.finish().unwrap();
        assert!(updates > 10, "received only {updates} assignment patches");
    }

    #[test]
    #[ignore = "release-only activity-cache microbenchmark; excludes decoding, applying patches and rendering"]
    fn activity_cache_microbenchmark() {
        use std::{hint::black_box, time::Instant};
        if cfg!(debug_assertions) {
            panic!("run with --release");
        }
        for (actors, shapes, changed, rounds) in [
            (16_002_u64, 32_004_u64, 2_u64, 10_000),
            (10_000, 20_000, 10_000, 100),
        ] {
            let mut state = state((1..=actors).map(|id| dynamic(id, false)));
            for id in actors + 1..=actors + shapes {
                state.objects.insert(id, object(id, "PxShape", &[]));
            }
            state
                .objects
                .insert(actors + shapes + 1, scene(actors + shapes + 1, actors, 0));
            let mut cache = ActivityCache::default();
            let start = Instant::now();
            for _ in 0..20 {
                cache.rebuild(black_box(&state));
            }
            let rebuild_ms = start.elapsed().as_secs_f64() * 1_000.0 / 20.0;
            let mut subset = SceneState::default();
            subset.objects.extend(
                state
                    .objects
                    .iter()
                    .take(changed as usize)
                    .map(|(&id, object)| (id, object.clone())),
            );
            let mut patch = SceneUpdate::full(&subset);
            patch.replace_all = false;
            let start = Instant::now();
            for _ in 0..rounds {
                cache.update(black_box(&patch), black_box(&state));
            }
            let update_ms = start.elapsed().as_secs_f64() * 1_000.0 / f64::from(rounds);
            let mut sleeping_state = state.clone();
            for id in 1..=changed {
                sleeping_state
                    .objects
                    .get_mut(&id)
                    .unwrap()
                    .properties
                    .insert("IsSleeping".into(), Value::Bool(true));
            }
            let mut sleeping_subset = SceneState::default();
            sleeping_subset.objects.extend(
                sleeping_state
                    .objects
                    .iter()
                    .take(changed as usize)
                    .map(|(&id, object)| (id, object.clone())),
            );
            let mut sleeping_patch = SceneUpdate::full(&sleeping_subset);
            sleeping_patch.replace_all = false;
            let start = Instant::now();
            for iteration in 0..rounds {
                let (patch, state) = if iteration % 2 == 0 {
                    (&sleeping_patch, &sleeping_state)
                } else {
                    (&patch, &state)
                };
                cache.update(black_box(patch), black_box(state));
            }
            let transition_ms = start.elapsed().as_secs_f64() * 1_000.0 / f64::from(rounds);
            let start = Instant::now();
            for _ in 0..1_000_000 {
                black_box(black_box(&cache).summary());
            }
            let summary_ns = start.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(cache.summary().total_actors, actors as usize);
            println!(
                "activity objects={} actors={actors} changed={changed} rebuild_mean_ms={rebuild_ms:.6} unchanged_category_update_mean_ms={update_ms:.6} sleep_transition_update_mean_ms={transition_ms:.6} summary_mean_ns={summary_ns:.3} rounds={rounds}",
                state.objects.len()
            );
        }
    }
}
