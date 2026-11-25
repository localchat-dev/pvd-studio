use crate::{
    Action, Decoder, Error, IngestCancellation, RecordingBuilder, SceneState, Value,
    clip::ValidationState, error, rooms::RoomDirectory,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque, btree_map::Entry},
    sync::{Arc, Mutex, Weak, atomic::AtomicBool},
};

/// One completed scene endpoint from a live SDK stream.
#[derive(Clone, Debug)]
pub struct LatestSceneFrame {
    pub index: usize,
    pub scene: SceneState,
    pub rooms: RoomDirectory,
    /// Object changes since the previous endpoint. `None` requires a full rebuild.
    pub changed_objects: Option<Arc<BTreeSet<u64>>>,
    recent_changes: VecDeque<FrameChanges>,
}

#[derive(Clone, Debug)]
struct FrameChanges {
    index: usize,
    objects: Arc<BTreeSet<u64>>,
    delta: Arc<Changes>,
}

impl LatestSceneFrame {
    /// Returns object IDs and possible room membership changes since `previous`.
    /// `None` indicates that the change window does not cover `previous`.
    pub fn changes_since(&self, previous: usize) -> Option<(Vec<u64>, bool)> {
        if previous == self.index {
            return Some((Vec::new(), false));
        }
        let mut objects = BTreeSet::new();
        let mut rooms_changed = false;
        for changes in self.changes_after(previous)? {
            objects.extend(changes.objects.iter().copied());
            rooms_changed |= changes.delta.rooms_changed;
        }
        Some((objects.into_iter().collect(), rooms_changed))
    }

    fn changes_after(&self, previous: usize) -> Option<impl Iterator<Item = &FrameChanges>> {
        let first = self.recent_changes.front()?.index;
        if previous > self.index || previous < first.saturating_sub(1) {
            return None;
        }
        Some(
            self.recent_changes
                .iter()
                .filter(move |frame| frame.index > previous),
        )
    }

    fn record_changes(&mut self, delta: Changes) {
        const MAX_FRAMES: usize = 8;
        const MAX_IDS: usize = 65_536;
        if delta.objects.len() > MAX_IDS {
            self.recent_changes.clear();
            self.changed_objects = None;
            return;
        }
        let mut total_ids = delta.objects.len();
        let mut total_bytes = delta.bytes;
        for frame in &self.recent_changes {
            total_ids += frame.delta.objects.len();
            total_bytes += frame.delta.bytes;
        }
        while self.recent_changes.len() >= MAX_FRAMES
            || total_ids > MAX_IDS
            || total_bytes > MAX_DIRTY_BYTES
        {
            let expired = self.recent_changes.pop_front().unwrap();
            total_ids -= expired.delta.objects.len();
            total_bytes -= expired.delta.bytes;
        }
        let objects = Arc::new(delta.changed_objects().collect::<BTreeSet<_>>());
        self.recent_changes.push_back(FrameChanges {
            index: self.index,
            objects: objects.clone(),
            delta: Arc::new(delta),
        });
        self.changed_objects = Some(objects);
    }
}

type PublishedScene = Mutex<Option<Arc<LatestSceneFrame>>>;

/// A scene owned by the reader. Keep it between syncs and read it outside publication.
#[derive(Default)]
pub struct LatestSceneView {
    index: Option<usize>,
    scene: SceneState,
    rooms: RoomDirectory,
    source: Weak<PublishedScene>,
}

impl LatestSceneView {
    pub fn scene(&self) -> &SceneState {
        &self.scene
    }

    pub fn rooms(&self) -> &RoomDirectory {
        &self.rooms
    }
}

pub struct LatestSceneUpdate {
    pub index: usize,
    /// Changed object IDs and room invalidation. `None` requests a full rebuild.
    pub changes: Option<(Vec<u64>, bool)>,
}

/// Reads the producer's one published scene without holding it between requests.
#[derive(Clone, Default)]
pub struct LatestSceneReader {
    slot: Arc<PublishedScene>,
}

impl LatestSceneReader {
    pub fn latest(&self) -> Option<Arc<LatestSceneFrame>> {
        self.slot.lock().unwrap().clone()
    }

    /// Borrows the published scene while excluding publication.
    pub fn with_latest<R>(&self, read: impl FnOnce(Option<&LatestSceneFrame>) -> R) -> R {
        read(self.slot.lock().unwrap().as_deref())
    }

    /// Synchronizes an owned view, falling back to the complete endpoint past the change window.
    /// An explicit frame index must match the published endpoint.
    pub fn sync_view(
        &self,
        view: &mut LatestSceneView,
        expected: Option<usize>,
    ) -> Result<LatestSceneUpdate, &'static str> {
        let slot = self.slot.lock().unwrap();
        let latest = slot.as_ref().ok_or("live session has no completed frame")?;
        if expected.is_some_and(|index| index != latest.index) {
            return Err("live session only has its latest completed frame");
        }
        let same_source = view.source.as_ptr() == Arc::as_ptr(&self.slot);
        let previous = same_source.then_some(view.index).flatten();
        if previous == Some(latest.index) {
            return Ok(LatestSceneUpdate {
                index: latest.index,
                changes: Some((Vec::new(), false)),
            });
        }

        let changes = if let Some(mut frames) =
            previous.and_then(|previous| latest.changes_after(previous))
        {
            let first = frames
                .next()
                .expect("different endpoint with retained changes");
            if first.index == latest.index {
                first.delta.apply(&latest.scene, &mut view.scene);
                Some((
                    first.objects.iter().copied().collect(),
                    first.delta.rooms_changed,
                ))
            } else {
                let mut merged = first.delta.as_ref().clone();
                for frame in frames {
                    merged.merge(&frame.delta);
                }
                merged.apply(&latest.scene, &mut view.scene);
                Some((merged.changed_objects().collect(), merged.rooms_changed))
            }
        } else {
            view.scene = latest.scene.clone();
            view.source = Arc::downgrade(&self.slot);
            None
        };
        view.index = Some(latest.index);
        view.rooms = latest.rooms.clone();
        Ok(LatestSceneUpdate {
            index: latest.index,
            changes,
        })
    }

    pub fn summary(&self) -> Option<(usize, RoomDirectory)> {
        let slot = self.slot.lock().unwrap();
        let latest = slot.as_ref()?;
        Some((latest.index, latest.rooms.clone()))
    }
}

/// Decodes a live SDK stream and publishes only the most recent completed scene.
#[derive(Default)]
pub struct LatestScene {
    decoder: Decoder,
    builder: Option<RecordingBuilder>,
    reader: LatestSceneReader,
    cancellation: IngestCancellation,
    failed: bool,
}

impl LatestScene {
    pub fn reader(&self) -> LatestSceneReader {
        self.reader.clone()
    }

    pub fn latest(&self) -> Option<Arc<LatestSceneFrame>> {
        self.reader.latest()
    }

    pub fn summary(&self) -> Option<(usize, RoomDirectory)> {
        self.reader.summary()
    }

    pub fn set_cancellation(&mut self, cancelled: Arc<AtomicBool>) {
        self.cancellation = IngestCancellation(Some(cancelled));
        if let Some(builder) = &mut self.builder {
            builder.set_cancellation(self.cancellation.clone());
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.failed {
            return Err(error(
                "live scene decoding failed; start another connection",
            ));
        }
        let result = self.feed_inner(bytes);
        self.failed = result.is_err();
        result
    }

    fn feed_inner(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.cancellation.check()?;
        let groups = self.decoder.feed(bytes)?;
        self.cancellation.check()?;
        if self.builder.is_none()
            && let Some(header) = self.decoder.header()
        {
            let mut builder = RecordingBuilder::new(header.clone());
            builder.validation = Some(ValidationState {
                skip_analysis: true,
                ..Default::default()
            });
            builder.latest_scene = Some(Publisher {
                reader: self.reader.clone(),
                dirty: None,
            });
            builder.set_cancellation(self.cancellation.clone());
            self.builder = Some(builder);
        }
        if let Some(builder) = &mut self.builder {
            for group in groups {
                builder.push(group)?;
            }
        }
        Ok(())
    }
}

const MAX_DIRTY_BYTES: usize = 16 << 20;

pub(super) struct Publisher {
    reader: LatestSceneReader,
    dirty: Option<Changes>,
}

#[derive(Clone, Debug, Default)]
struct Changes {
    objects: BTreeMap<u64, ObjectChange>,
    cameras: bool,
    rooms_changed: bool,
    bytes: usize,
}

#[derive(Clone, Debug, Default)]
struct ObjectChange {
    lifetime: bool,
    properties: BTreeMap<String, PropertyChange>,
    contacts: bool,
}

#[derive(Clone, Debug)]
enum PropertyChange {
    Replace,
    Append(Vec<ReferenceAppend>),
}

#[derive(Clone, Copy, Debug)]
struct ReferenceAppend {
    target: u64,
    generation: u32,
}

impl Publisher {
    pub(super) fn track(&mut self, action: &Action, state: &SceneState) {
        let Some(dirty) = &mut self.dirty else {
            return;
        };
        dirty.track(action, state);
        if dirty.bytes > MAX_DIRTY_BYTES {
            self.dirty = None;
        }
    }

    pub(super) fn publish(&mut self, index: usize, state: &SceneState, rooms: &RoomDirectory) {
        let dirty = self.dirty.take();
        if let Some(dirty) = dirty {
            let mut slot = self.reader.slot.lock().unwrap();
            let latest = Arc::make_mut(slot.as_mut().expect("published scene is present"));
            latest.index = index;
            latest.rooms = rooms.clone();
            dirty.apply(state, &mut latest.scene);
            latest.record_changes(dirty);
        } else {
            let latest = Arc::new(LatestSceneFrame {
                index,
                scene: state.clone(),
                rooms: rooms.clone(),
                changed_objects: None,
                recent_changes: VecDeque::new(),
            });
            *self.reader.slot.lock().unwrap() = Some(latest);
        }
        self.dirty = Some(Changes::default());
    }
}

impl Changes {
    fn object(&mut self, id: u64) -> &mut ObjectChange {
        self.objects.entry(id).or_insert_with(|| {
            self.bytes += 128;
            ObjectChange::default()
        })
    }

    fn track(&mut self, action: &Action, state: &SceneState) {
        match action {
            Action::Create(object) => {
                let change = self.object(object.id);
                change.lifetime = true;
                change.properties.clear();
                self.rooms_changed = true;
            }
            Action::Destroy(id) => {
                let change = self.object(*id);
                change.lifetime = true;
                change.properties.clear();
                change.contacts = true;
                self.rooms_changed = true;
            }
            // Reuse the array computed by the decoder instead of scanning it again for each removal.
            Action::Set(id, key, _) | Action::Remove(id, key, _) => {
                self.track_property(*id, key, None)
            }
            Action::Push(id, key, target) => self.track_property(
                *id,
                key,
                Some(ReferenceAppend {
                    target: *target,
                    generation: state.expected_generation(*target),
                }),
            ),
            Action::Contacts(id, _) => self.object(*id).contacts = true,
            Action::Camera(..) | Action::RemoveCamera(_) => self.cameras = true,
            Action::Error(_)
            | Action::Debug(..)
            | Action::ClearDebug(_)
            | Action::ReplaceDebug(_) => {}
        }
    }

    fn track_property(&mut self, id: u64, name: &str, append: Option<ReferenceAppend>) {
        let object = self.object(id);
        if object.lifetime {
            return;
        }
        let mut bytes = 0;
        let property = match object.properties.entry(name.to_owned()) {
            Entry::Vacant(entry) => {
                bytes += name.len() + 96;
                entry.insert(PropertyChange::Append(Vec::new()))
            }
            Entry::Occupied(entry) => entry.into_mut(),
        };
        match (property, append) {
            (property, None) => *property = PropertyChange::Replace,
            (PropertyChange::Append(appends), Some(append)) => {
                appends.push(append);
                bytes += 2 * std::mem::size_of::<ReferenceAppend>();
            }
            (PropertyChange::Replace, Some(_)) => {}
        }
        self.bytes += bytes;
        self.rooms_changed |= crate::room_scope::affects_membership(name);
    }

    fn changed_objects(&self) -> impl Iterator<Item = u64> + '_ {
        self.objects.iter().filter_map(|(&id, change)| {
            (change.lifetime || !change.properties.is_empty()).then_some(id)
        })
    }

    fn merge(&mut self, next: &Self) {
        self.cameras |= next.cameras;
        self.rooms_changed |= next.rooms_changed;
        for (&id, next) in &next.objects {
            let current = self.objects.entry(id).or_default();
            current.contacts |= next.contacts;
            current.lifetime |= next.lifetime;
            if current.lifetime {
                current.properties.clear();
                continue;
            }
            for (name, next) in &next.properties {
                match current.properties.entry(name.clone()) {
                    Entry::Vacant(entry) => {
                        entry.insert(next.clone());
                    }
                    Entry::Occupied(mut entry) => match (entry.get_mut(), next) {
                        (change, PropertyChange::Replace) => *change = PropertyChange::Replace,
                        (PropertyChange::Append(appends), PropertyChange::Append(next)) => {
                            appends.extend_from_slice(next)
                        }
                        (PropertyChange::Replace, _) => {}
                    },
                }
            }
        }
    }

    fn apply(&self, state: &SceneState, view: &mut SceneState) {
        for (&id, change) in &self.objects {
            if change.contacts {
                sync_entry(&state.contacts, &mut view.contacts, id);
            }
            if change.lifetime {
                sync_entry(&state.objects, &mut view.objects, id);
                sync_entry(&state.generations, &mut view.generations, id);
                let previous: Vec<_> = view
                    .reference_lifetimes
                    .range((id, String::new())..)
                    .take_while(|((owner, _), _)| *owner == id)
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in previous {
                    view.reference_lifetimes.remove(&key);
                }
                view.reference_lifetimes.extend(
                    state
                        .reference_lifetimes
                        .range((id, String::new())..)
                        .take_while(|((owner, _), _)| *owner == id)
                        .map(|(key, value)| (key.clone(), value.clone())),
                );
            } else {
                let source = state.objects.get(&id);
                let mut target = view.objects.get_mut(&id);
                for (name, property) in &change.properties {
                    let key = (id, name.clone());
                    match property {
                        PropertyChange::Replace => {
                            if let (Some(source), Some(target)) = (source, target.as_deref_mut()) {
                                sync_entry(
                                    &source.properties,
                                    &mut target.properties,
                                    name.clone(),
                                );
                            }
                            sync_entry(
                                &state.reference_lifetimes,
                                &mut view.reference_lifetimes,
                                key,
                            );
                        }
                        PropertyChange::Append(appends) => {
                            apply_reference_appends(
                                appends,
                                key,
                                target.as_deref_mut().map(|object| &mut object.properties),
                                &mut view.reference_lifetimes,
                            );
                        }
                    }
                }
            }
        }
        if self.cameras {
            view.cameras.clone_from(&state.cameras);
        }
        view.debug_draw.clone_from(&state.debug_draw);
        view.errors.clone_from(&state.errors);
    }
}

fn apply_reference_appends(
    appends: &[ReferenceAppend],
    key: (u64, String),
    mut properties: Option<&mut BTreeMap<String, Value>>,
    bindings: &mut BTreeMap<(u64, String), Arc<BTreeMap<u64, u32>>>,
) {
    // A binding can predate the target's final lifetime, including a forward reference.
    let targets = Arc::make_mut(bindings.entry(key.clone()).or_default());
    for &ReferenceAppend { target, generation } in appends {
        let inserted = targets.insert(target, generation).is_none();
        if inserted && let Some(properties) = properties.as_deref_mut() {
            properties
                .entry(key.1.clone())
                .or_insert_with(|| Value::Array(Arc::new(Vec::new())))
                .append_reference(target);
        }
    }
}

fn sync_entry<K: Ord, V: Clone>(source: &BTreeMap<K, V>, target: &mut BTreeMap<K, V>, key: K) {
    if let Some(value) = source.get(&key) {
        target.insert(key, value.clone());
    } else {
        target.remove(&key);
    }
}
