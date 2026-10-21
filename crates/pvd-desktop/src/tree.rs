use std::collections::{BTreeSet, HashSet};

use pvd_core::{Object, SceneState, Value};

use crate::i18n::Language;

const CHILD_PROPERTIES: &[&str] = &[
    "Aggregates",
    "Articulations",
    "RigidStatics",
    "RigidDynamics",
    "Actors",
    "ParticleSystems",
    "ParticleFluids",
    "Cloths",
    "Joints",
    "Constraints",
    "Links",
    "InboundJoint",
    "Shapes",
    "Geometry",
    "Materials",
    "ConvexMesh",
    "TriangleMesh",
    "HeightField",
];

pub struct Row {
    pub id: Option<u64>,
    pub generation: u32,
    pub depth: usize,
    pub label: String,
    pub detail: String,
    pub children: bool,
}

pub struct TreeCache {
    pub rows: Vec<Row>,
    pub collapsed: HashSet<(u64, u32)>,
    revision: u64,
    filter: String,
    language: Language,
    pub dirty: bool,
}

impl Default for TreeCache {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            collapsed: HashSet::new(),
            revision: u64::MAX,
            filter: String::new(),
            language: Language::English,
            dirty: true,
        }
    }
}

impl TreeCache {
    pub fn retain_for_update(
        &mut self,
        update: &pvd_core::live::SceneUpdate,
        state: &SceneState,
        revision: u64,
    ) {
        if self.revision != revision || self.dirty || update.replace_all {
            return;
        }
        let mut generations = Vec::new();
        for (id, next) in update.changed_objects() {
            let (previous, next) = match (state.objects.get(&id), next) {
                (None, None) => continue,
                (Some(previous), Some(next)) => (previous, next),
                _ => return,
            };
            if previous.type_name != next.type_name
                || update.changes_object_properties(state, id, CHILD_PROPERTIES)
                || update.changes_object_properties(state, id, &["Name"])
            {
                return;
            }
            if previous.generation == next.generation {
                continue;
            }
            if !self.filter.is_empty()
                || CHILD_PROPERTIES
                    .iter()
                    .any(|name| previous.properties.contains_key(*name))
            {
                return;
            }
            let Some(resources) = self.rows.iter().position(|row| row.id.is_none()) else {
                return;
            };
            let mut matching = self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.id == Some(id));
            let Some((index, row)) = matching.next() else {
                return;
            };
            if matching.next().is_some()
                || index <= resources
                || row.depth != 1
                || row.children
                || row.generation != previous.generation
            {
                return;
            }
            generations.push((index, next.generation));
        }
        for (index, generation) in generations {
            self.rows[index].generation = generation;
        }
        self.revision = revision.wrapping_add(1);
    }
    pub fn update(&mut self, state: &SceneState, revision: u64, filter: &str, language: Language) {
        if self.revision == revision
            && self.filter == filter
            && self.language == language
            && !self.dirty
        {
            return;
        }
        self.revision = revision;
        self.filter = filter.into();
        self.language = language;
        self.dirty = false;
        self.rows.clear();
        if !filter.is_empty() {
            let query = filter.to_lowercase();
            for object in state.objects.values() {
                let row = row(object, 0, false);
                if format!("{} {} {}", row.label, row.detail, object.id)
                    .to_lowercase()
                    .contains(&query)
                {
                    self.rows.push(row);
                }
            }
            return;
        }
        let mut builder = TreeBuilder {
            state,
            collapsed: &self.collapsed,
            visited: BTreeSet::new(),
            rows: &mut self.rows,
            language,
        };
        for object in state
            .objects
            .values()
            .filter(|o| o.type_name.ends_with("::PxScene"))
        {
            builder.append(object.id, 0, false);
        }
        let remaining: Vec<_> = state
            .objects
            .keys()
            .filter(|id| !builder.visited.contains(id))
            .copied()
            .collect();
        if !remaining.is_empty() {
            builder.rows.push(Row {
                id: None,
                generation: 0,
                depth: 0,
                label: language
                    .text("Resources & other objects", "资源及其他对象")
                    .into(),
                detail: String::new(),
                children: false,
            });
            for id in remaining {
                if !builder.visited.contains(&id) {
                    builder.append(id, 1, false);
                }
            }
        }
    }
}

fn row(object: &Object, depth: usize, children: bool) -> Row {
    let short_type = object
        .type_name
        .rsplit("::")
        .next()
        .unwrap_or(&object.type_name);
    let name = match object.properties.get("Name") {
        Some(Value::String(name)) if !name.is_empty() => name.as_str(),
        _ => short_type,
    };
    Row {
        id: Some(object.id),
        generation: object.generation,
        depth,
        label: name.into(),
        detail: format!("{short_type} · 0x{:x}", object.id),
        children,
    }
}

fn children(state: &SceneState, object: &Object) -> Vec<u64> {
    let mut children = Vec::new();
    let mut visited = BTreeSet::new();
    for &name in CHILD_PROPERTIES {
        if let Some(value) = object.properties.get(name) {
            let targets: Vec<_> = match value {
                Value::Ref(id) => vec![*id],
                Value::Array(values) => values
                    .iter()
                    .filter_map(|value| match value {
                        Value::Ref(id) => Some(*id),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            for id in targets {
                if state.resolve_reference(object.id, name, id).is_some() && visited.insert(id) {
                    children.push(id);
                }
            }
        }
    }
    children
}

struct TreeBuilder<'a> {
    state: &'a SceneState,
    collapsed: &'a HashSet<(u64, u32)>,
    visited: BTreeSet<u64>,
    rows: &'a mut Vec<Row>,
    language: Language,
}

impl TreeBuilder<'_> {
    fn append(&mut self, id: u64, depth: usize, hidden: bool) {
        if self.rows.len() >= 100_000 || depth > 128 {
            return;
        }
        let Some(object) = self.state.objects.get(&id) else {
            return;
        };
        let repeated = !self.visited.insert(id);
        let descendants = if repeated {
            Vec::new()
        } else {
            children(self.state, object)
        };
        if !hidden {
            let mut row = row(object, depth, !descendants.is_empty());
            if repeated {
                row.detail
                    .push_str(self.language.text(" · shared reference", " · 共享引用"));
            }
            self.rows.push(row);
        }
        let hidden = hidden || self.collapsed.contains(&(id, object.generation));
        for child in descendants {
            self.append(child, depth + 1, hidden);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_leaf_generation_updates_preserve_rows_but_metadata_changes_rebuild() {
        let mut state = SceneState::default();
        state.objects.insert(
            7,
            Object {
                id: 7,
                generation: 0,
                type_name: "physx::PxBoxGeometry".into(),
                properties: [("HalfExtents".into(), Value::Vec3([1.0; 3]))].into(),
            },
        );
        let patch = |next: &SceneState| {
            let mut update = pvd_core::live::SceneUpdate::full(next);
            update.replace_all = false;
            update
        };
        let mut next = state.clone();
        next.objects.get_mut(&7).unwrap().generation = 3;
        let mut cache = TreeCache::default();
        cache.update(&state, 10, "", Language::English);
        let label = cache.rows[1].label.as_ptr();
        cache.retain_for_update(&patch(&next), &state, 10);
        assert_eq!(cache.revision, 11);
        assert_eq!(cache.rows[1].generation, 3);
        cache.update(&next, 11, "", Language::English);
        assert_eq!(cache.rows[1].label.as_ptr(), label);
        let mut rebuilt = TreeCache::default();
        rebuilt.update(&next, 11, "", Language::English);
        assert_eq!(cache.rows[1].detail, rebuilt.rows[1].detail);
        assert_eq!(cache.rows[1].generation, rebuilt.rows[1].generation);

        for change in 0..5 {
            let mut changed = next.clone();
            match change {
                0 => {
                    changed
                        .objects
                        .get_mut(&7)
                        .unwrap()
                        .properties
                        .insert("Name".into(), Value::String("renamed".into()));
                }
                1 => {
                    changed.objects.get_mut(&7).unwrap().type_name =
                        "physx::PxSphereGeometry".into();
                }
                2 => {
                    changed
                        .objects
                        .get_mut(&7)
                        .unwrap()
                        .properties
                        .insert("Geometry".into(), Value::Ref(9));
                }
                3 => {
                    let mut created = changed.objects[&7].clone();
                    created.id = 9;
                    changed.objects.insert(9, created);
                }
                4 => {}
                _ => unreachable!(),
            }
            let filter = if change == 4 { "Box" } else { "" };
            let mut cache = TreeCache::default();
            cache.update(&state, 10, filter, Language::English);
            cache.retain_for_update(&patch(&changed), &state, 10);
            assert_eq!(
                cache.revision, 10,
                "change {change} must invalidate tree rows"
            );
        }
        let mut cache = TreeCache::default();
        cache.update(&state, 10, "", Language::English);
        cache.rows.push(row(&state.objects[&7], 1, false));
        cache.retain_for_update(&patch(&next), &state, 10);
        assert_eq!(
            cache.revision, 10,
            "a shared leaf cannot be refreshed as an independent resource"
        );
    }

    #[test]
    #[ignore = "requires official PhysX 3.4 baseline stream"]
    fn live_motion_reuses_tree_rows_without_hiding_structural_changes() {
        use std::time::Instant;
        let custom_capture = std::env::var_os("PVD_TREE_CAPTURE");
        let motion_limit = custom_capture.is_some();
        let path = custom_capture
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4.pvdstream")
            });
        let bytes = std::fs::read(&path).unwrap();
        let mut live =
            pvd_core::LiveRecording::with_history(pvd_core::live::HistoryConfig::default())
                .unwrap();
        let mut state = SceneState::default();
        let mut cache = TreeCache::default();
        let mut revision = 0;
        let mut reused = 0;
        let mut compared_motion = 0;
        let mut retained_times = Vec::new();
        let mut rebuild_times = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let chunk_size = if motion_limit { 64 * 1024 } else { 1024 };
            let end = (offset + chunk_size).min(bytes.len());
            live.feed(&bytes[offset..end]).unwrap();
            offset = end;
            if let Some((frame, update)) = live.take_update() {
                let start = Instant::now();
                cache.retain_for_update(&update, &state, revision);
                let retain_duration = start.elapsed();
                revision += 1;
                let retained = cache.revision == revision;
                reused += usize::from(retained);
                update.apply(&mut state);
                let start = Instant::now();
                cache.update(&state, revision, "", Language::Chinese);
                if retained {
                    retained_times.push((retain_duration + start.elapsed()).as_secs_f64() * 1000.0);
                }
                if !retained || compared_motion < 40 {
                    let mut rebuilt = TreeCache::default();
                    let start = Instant::now();
                    rebuilt.update(&state, revision, "", Language::Chinese);
                    rebuild_times.push(start.elapsed().as_secs_f64() * 1000.0);
                    fn signature(row: &Row) -> (Option<u64>, u32, usize, &str, &str, bool) {
                        (
                            row.id,
                            row.generation,
                            row.depth,
                            row.label.as_str(),
                            row.detail.as_str(),
                            row.children,
                        )
                    }
                    assert!(
                        cache
                            .rows
                            .iter()
                            .map(signature)
                            .eq(rebuilt.rows.iter().map(signature))
                    );
                    compared_motion += usize::from(retained);
                }
                if motion_limit && revision % 10 == 0 {
                    eprintln!(
                        "tree profile progress: frame={frame} updates={revision} reused={reused} objects={}",
                        state.objects.len()
                    );
                }
                if motion_limit && compared_motion == 40 {
                    break;
                }
            }
        }
        assert!(
            reused >= if motion_limit { 5 } else { 31 },
            "stable tree structure must reuse rows across motion or isolated leaf generations"
        );
        let old_revision = cache.revision;
        cache.retain_for_update(&pvd_core::live::SceneUpdate::full(&state), &state, revision);
        assert_eq!(cache.revision, old_revision, "replacement must rebuild");
        let average = |times: &[f64]| {
            if times.is_empty() {
                "n/a".to_owned()
            } else {
                format!("{:.3}", times.iter().sum::<f64>() / times.len() as f64)
            }
        };
        eprintln!(
            "Object tree motion: source={} objects={} rows={} updates={} reused={} structural_updates={} compared_motion={} retain_average_ms={} retain_max_ms={:?} full_average_ms={} full_max_ms={:.3}",
            path.display(),
            state.objects.len(),
            cache.rows.len(),
            revision,
            reused,
            revision as usize - reused,
            compared_motion,
            average(&retained_times),
            retained_times.iter().copied().reduce(f64::max),
            average(&rebuild_times),
            rebuild_times.iter().copied().fold(0.0, f64::max)
        );
    }

    #[test]
    #[ignore = "requires official SDK 3.4 pvd-objects fixture"]
    fn aggregate_link_tree_tracks_membership_language_and_generation() {
        let path = std::env::var_os("PVD_OBJECTS_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4-objects.pvdstream")
            });
        let recording = pvd_core::Recording::from_path(path).unwrap();
        let mut cache = TreeCache::default();
        let mut articulation_id = 0;
        for frame in [0, 60, 90, 120, 150, 209, 0] {
            let state = recording.state_at(frame).unwrap();
            let articulation = state
                .objects
                .values()
                .find(|object| object.type_name.ends_with("::PxArticulation"))
                .unwrap();
            articulation_id = articulation.id;
            let aggregate = state
                .objects
                .values()
                .find(|object| object.type_name.ends_with("::PxAggregate"))
                .unwrap();
            let attached = children(&state, aggregate).contains(&articulation.id);
            for language in [Language::English, Language::Chinese] {
                cache.update(&state, frame as u64, "", language);
                let mut ancestors = Vec::<u64>::new();
                let mut unique = BTreeSet::new();
                for row in &cache.rows {
                    let Some(id) = row.id else {
                        ancestors.clear();
                        continue;
                    };
                    if !unique.insert(id) {
                        continue;
                    }
                    let object = &state.objects[&id];
                    assert_eq!(row.generation, object.generation);
                    ancestors.truncate(row.depth);
                    if id == articulation.id {
                        assert_eq!(row.depth, if attached { 2 } else { 1 });
                        if attached {
                            assert_eq!(ancestors.last(), Some(&aggregate.id));
                        }
                    }
                    if object.type_name.ends_with("::PxArticulationLink") {
                        let Some(Value::Ref(parent)) = object.properties.get("Parent") else {
                            panic!("missing parent");
                        };
                        assert_eq!(ancestors.last(), Some(parent));
                    }
                    if object.type_name.ends_with("::PxArticulationJoint") {
                        let Some(Value::Ref(link)) = object.properties.get("Link") else {
                            panic!("missing link");
                        };
                        assert_eq!(ancestors.last(), Some(link));
                    }
                    ancestors.push(id);
                }
            }
        }
        cache.collapsed.insert((articulation_id, 0));
        cache.dirty = true;
        let first = recording.state_at(0).unwrap();
        cache.update(&first, 0, "", Language::Chinese);
        let root = first
            .objects
            .values()
            .find(|object| {
                object.properties.get("Name") == Some(&Value::String("Root link".into()))
            })
            .unwrap();
        assert!(!cache.rows.iter().any(|row| row.id == Some(root.id)));
        let replaced = recording.state_at(150).unwrap();
        cache.update(&replaced, 150, "", Language::Chinese);
        assert!(
            cache
                .rows
                .iter()
                .any(|row| row.id == Some(root.id) && row.generation == 1)
        );
    }

    #[test]
    fn language_change_refreshes_labels_without_changing_object_identity_or_collapse() {
        let mut state = SceneState::default();
        state.objects.insert(
            42,
            Object {
                id: 42,
                generation: 3,
                type_name: "physx::PxRigidDynamic".into(),
                properties: [("Name".into(), Value::String("SDK actor 名称".into()))].into(),
            },
        );
        let mut cache = TreeCache::default();
        cache.collapsed.insert((42, 3));
        cache.update(&state, 8, "", Language::English);
        assert_eq!(cache.rows[0].label, "Resources & other objects");
        let object_detail = cache.rows[1].detail.clone();

        cache.update(&state, 8, "", Language::Chinese);
        assert_eq!(cache.rows[0].label, "资源及其他对象");
        assert_eq!(cache.rows[1].id, Some(42));
        assert_eq!(cache.rows[1].generation, 3);
        assert_eq!(cache.rows[1].label, "SDK actor 名称");
        assert_eq!(cache.rows[1].detail, object_detail);
        assert!(cache.collapsed.contains(&(42, 3)));

        cache.update(&state, 8, "SDK actor", Language::Chinese);
        assert_eq!(cache.rows.len(), 1);
        assert_eq!(cache.rows[0].id, Some(42));
        cache.update(&state, 8, "SDK actor", Language::English);
        assert_eq!(cache.rows.len(), 1);
        assert_eq!(cache.rows[0].label, "SDK actor 名称");
    }
}
