use crate::{
    Action, Decoder, Error, IngestCancellation, RecordingBuilder, SceneState,
    clip::ValidationState, error, rooms::RoomDirectory,
};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, atomic::AtomicBool},
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
    rooms_changed: bool,
}

impl LatestSceneFrame {
    /// Returns object IDs and possible room membership changes since `previous`.
    /// `None` indicates that the change window does not cover `previous`.
    pub fn changes_since(&self, previous: usize) -> Option<(Vec<u64>, bool)> {
        if previous == self.index {
            return Some((Vec::new(), false));
        }
        if previous > self.index || previous + 1 < self.recent_changes.front()?.index {
            return None;
        }
        let mut objects = BTreeSet::new();
        let mut rooms_changed = false;
        for changes in self
            .recent_changes
            .iter()
            .filter(|changes| changes.index > previous)
        {
            objects.extend(changes.objects.iter().copied());
            rooms_changed |= changes.rooms_changed;
        }
        Some((objects.into_iter().collect(), rooms_changed))
    }

    fn record_changes(&mut self, objects: BTreeSet<u64>, rooms_changed: bool) {
        const MAX_FRAMES: usize = 8;
        const MAX_IDS: usize = 65_536;
        if objects.len() > MAX_IDS {
            self.recent_changes.clear();
            self.changed_objects = None;
            return;
        }
        let mut total = objects.len()
            + self
                .recent_changes
                .iter()
                .map(|frame| frame.objects.len())
                .sum::<usize>();
        while self.recent_changes.len() >= MAX_FRAMES || total > MAX_IDS {
            total -= self.recent_changes.pop_front().unwrap().objects.len();
        }
        let objects = Arc::new(objects);
        self.recent_changes.push_back(FrameChanges {
            index: self.index,
            objects: objects.clone(),
            rooms_changed,
        });
        self.changed_objects = Some(objects);
    }
}

/// Reads the producer's one published scene without holding it between requests.
#[derive(Clone, Default)]
pub struct LatestSceneReader {
    slot: Arc<Mutex<Option<Arc<LatestSceneFrame>>>>,
}

impl LatestSceneReader {
    pub fn latest(&self) -> Option<Arc<LatestSceneFrame>> {
        self.slot.lock().unwrap().clone()
    }

    /// Borrows the published scene while excluding publication.
    pub fn with_latest<R>(&self, read: impl FnOnce(Option<&LatestSceneFrame>) -> R) -> R {
        read(self.slot.lock().unwrap().as_deref())
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

#[derive(Default)]
struct Changes {
    objects: BTreeMap<u64, ObjectChange>,
    cameras: bool,
    rooms_changed: bool,
    bytes: usize,
}

#[derive(Default)]
struct ObjectChange {
    lifetime: bool,
    properties: BTreeSet<String>,
    contacts: bool,
}

impl Publisher {
    pub(super) fn track(&mut self, action: &Action) {
        let Some(dirty) = &mut self.dirty else {
            return;
        };
        dirty.track(action);
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
            let rooms_changed = dirty.rooms_changed;
            let objects = dirty.apply(state, &mut latest.scene);
            latest.record_changes(objects, rooms_changed);
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

    fn track(&mut self, action: &Action) {
        match action {
            Action::Create(object) => {
                self.object(object.id).lifetime = true;
                self.rooms_changed = true;
            }
            Action::Destroy(id) => {
                let change = self.object(*id);
                change.lifetime = true;
                change.contacts = true;
                self.rooms_changed = true;
            }
            Action::Set(id, key, _) | Action::Push(id, key, _) | Action::Remove(id, key, _) => {
                let change = self.object(*id);
                if !change.lifetime && !change.properties.contains(key) {
                    change.properties.insert(key.clone());
                    self.bytes += key.len() + 64;
                    self.rooms_changed |= crate::room_scope::affects_membership(key);
                }
            }
            Action::Contacts(id, _) => self.object(*id).contacts = true,
            Action::Camera(..) | Action::RemoveCamera(_) => self.cameras = true,
            Action::Error(_)
            | Action::Debug(..)
            | Action::ClearDebug(_)
            | Action::ReplaceDebug(_) => {}
        }
    }

    fn apply(self, state: &SceneState, view: &mut SceneState) -> BTreeSet<u64> {
        let mut changed_objects = BTreeSet::new();
        for (id, change) in self.objects {
            if change.contacts {
                sync_entry(&state.contacts, &mut view.contacts, id);
            }
            if change.lifetime || !change.properties.is_empty() {
                changed_objects.insert(id);
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
                let mut objects = state.objects.get(&id).zip(view.objects.get_mut(&id));
                for name in change.properties {
                    let key = (id, name);
                    if let Some((source, target)) = &mut objects {
                        sync_entry(&source.properties, &mut target.properties, key.1.clone());
                    }
                    sync_entry(
                        &state.reference_lifetimes,
                        &mut view.reference_lifetimes,
                        key,
                    );
                }
            }
        }
        if self.cameras {
            view.cameras.clone_from(&state.cameras);
        }
        view.debug_draw.clone_from(&state.debug_draw);
        view.errors.clone_from(&state.errors);
        changed_objects
    }
}

fn sync_entry<K: Ord, V: Clone>(source: &BTreeMap<K, V>, target: &mut BTreeMap<K, V>, key: K) {
    if let Some(value) = source.get(&key) {
        target.insert(key, value.clone());
    } else {
        target.remove(&key);
    }
}
