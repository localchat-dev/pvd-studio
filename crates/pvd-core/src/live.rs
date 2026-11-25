//! Bounded current state, coalesced display changes, and rolling disk history.
mod analysis_cache;
mod analysis_pages;
mod cache_directory;
pub(crate) mod capture;
#[cfg(test)]
mod cold_seek_tests;
#[cfg(test)]
mod disk_accounting;
#[cfg(test)]
mod frame_source_tests;
#[cfg(test)]
mod full_update_tests;
mod object_keys;
#[cfg(test)]
mod owned_apply_tests;
mod persistent;
#[cfg(test)]
mod real_seek_probe;
#[cfg(test)]
mod reclaim_tests;
#[cfg(test)]
mod reference_cow_tests;
mod replay;
mod resources;
#[cfg(test)]
mod scene_undo_real_probe;
mod segments;
mod undo;
use crate::{
    Action, Error, ErrorEvent, FrameInfo, IngestCancellation, Object, SceneState, SourceCamera,
    Value,
    analysis::{Analysis, SharedAnalysis},
    camera_entry_bytes, check_replay_cancelled,
    contacts::Contact,
    debug_draw::Primitive,
    error,
    frame_sources::{
        FrameBinding, FrameDirectory, FrameDirectoryLimits, FrameSection, FrameSourceKey,
        SourceFrame,
    },
    metrics::{Metrics, Stage},
};
use analysis_cache::AnalysisCache;
use cache_directory::CacheDirectory;
pub use cache_directory::{
    CacheCleanupReport, cleanup_stale_history, enable_background_stale_history_cleanup,
    pending_history_cleanups, queue_stale_history_cleanup,
};
pub use persistent::ReplayLookahead;
pub use replay::{ReplayCursor, ReplayFrame, ReplaySeekMetrics, SceneReplayFrame};
use resources::{ResourceHandle, ResourcePlan, ResourceReader, Resources};
use segments::{SegmentWriter, WriteState};
use serde::{Deserialize, Serialize, ser::SerializeMap};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, AtomicUsize},
    },
    time::{Duration, Instant},
};

const MAGIC: &[u8; 8] = b"PVDHIS03";
const MAX_RECORD: usize = 64 << 20;
const MAX_INDEX_FRAMES: usize = 1_000_000;
const MAX_ERRORS: usize = 512;
const MAX_DISPLAY_DIRTY_BYTES: usize = 8 << 20;
const LARGE_PROPERTY_BYTES: usize = 4096;
const SERIALIZATION_CACHE_BYTES: usize = 256 << 10;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct HistoryConfig {
    pub directory: PathBuf,
    pub disk_budget_bytes: u64,
    pub segment_target_bytes: u64,
    pub segment_frames: usize,
    pub min_free_disk_bytes: u64,
}
impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            directory: PathBuf::from("pvd-cache").join("history"),
            disk_budget_bytes: 8 << 30,
            segment_target_bytes: 64 << 20,
            segment_frames: 600,
            min_free_disk_bytes: 512 << 20,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HistoryStats {
    pub disk_bytes: u64,
    /// Complete-session retention; the disk budget applies only to the hot writer.
    pub persistent: bool,
    pub cache_disk_bytes: u64,
    pub budget_bytes: u64,
    pub first_frame: usize,
    pub retained_frames: usize,
    pub segments: usize,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ObjectState {
    object: Object,
    references: BTreeMap<String, Arc<BTreeMap<u64, u32>>>,
}

#[derive(Serialize, Deserialize)]
struct StoredObject {
    object: Object,
    references: BTreeMap<String, BTreeMap<u64, u32>>,
    large_properties: BTreeMap<String, u64>,
    large_references: BTreeMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
struct KeyedObject {
    id: u64,
    generation: u32,
    type_name: String,
    keys: u64,
    #[serde(deserialize_with = "object_keys::deserialize_values")]
    values: Vec<Value>,
    references: BTreeMap<String, BTreeMap<u64, u32>>,
    large_properties: BTreeMap<String, u64>,
    large_references: BTreeMap<String, u64>,
}

#[derive(Clone)]
struct CachedValue {
    value: Value,
    offset: u64,
}

#[derive(Clone)]
struct CachedReferences {
    value: Arc<BTreeMap<u64, u32>>,
    offset: u64,
}

#[derive(Clone, Default)]
struct ObjectCache {
    generation: u32,
    properties: BTreeMap<String, CachedValue>,
    references: BTreeMap<String, CachedReferences>,
    bytes: usize,
}
impl ObjectState {
    fn apply_owned(self, state: &mut SceneState) {
        let id = self.object.id;
        state.objects.insert(id, self.object);
        state.reference_lifetimes.extend(
            self.references
                .into_iter()
                .map(|(key, value)| ((id, key), value)),
        );
    }

    fn capture(state: &SceneState, id: u64) -> Option<Self> {
        let object = state.objects.get(&id)?.clone();
        let references = state
            .reference_lifetimes
            .range((id, String::new())..)
            .take_while(|((owner, _), _)| *owner == id)
            .map(|((_, key), value)| (key.clone(), value.clone()))
            .collect();
        Some(Self { object, references })
    }
    fn apply(&self, state: &mut SceneState) {
        let id = self.object.id;
        state.objects.insert(id, self.object.clone());
        state.reference_lifetimes.extend(
            self.references
                .iter()
                .map(|(key, value)| ((id, key.clone()), value.clone())),
        );
    }
}

/// Assignment patches can be merged without losing intermediate object lifetimes.
#[derive(Clone, Debug, Default)]
pub struct SceneUpdate {
    pub replace_all: bool,
    objects: BTreeMap<u64, Option<Arc<ObjectState>>>,
    generations: BTreeMap<u64, u32>,
    removed_generations: BTreeSet<u64>,
    debug: BTreeMap<u64, Option<Arc<Vec<Primitive>>>>,
    contacts: BTreeMap<u64, Option<Arc<[Contact]>>>,
    errors: Option<Vec<ErrorEvent>>,
    cameras: BTreeMap<String, Option<SourceCamera>>,
}
impl SceneUpdate {
    fn capture(state: &SceneState, dirty: &BTreeSet<u64>, errors: bool) -> Self {
        Self {
            objects: dirty
                .iter()
                .map(|&id| (id, ObjectState::capture(state, id).map(Arc::new)))
                .collect(),
            generations: dirty
                .iter()
                .filter_map(|id| state.generations.get(id).map(|g| (*id, *g)))
                .collect(),
            debug: dirty
                .iter()
                .map(|&id| (id, state.debug_draw.get(&id).cloned()))
                .collect(),
            contacts: dirty
                .iter()
                .map(|&id| (id, state.contacts.get(&id).cloned()))
                .collect(),
            errors: errors.then(|| state.errors.clone()),
            replace_all: false,
            removed_generations: BTreeSet::new(),
            cameras: BTreeMap::new(),
        }
    }
    pub fn full(state: &SceneState) -> Self {
        let dirty = state
            .generations
            .keys()
            .chain(state.objects.keys())
            .chain(state.debug_draw.keys())
            .chain(state.contacts.keys())
            .copied()
            .collect();
        let mut update = Self::capture(state, &dirty, true);
        update.cameras = state
            .cameras
            .iter()
            .map(|(name, camera)| (name.clone(), Some(*camera)))
            .collect();
        update.replace_all = true;
        update
    }
    pub fn merge(&mut self, next: Self) {
        if next.replace_all {
            *self = next;
            return;
        }
        self.objects.extend(next.objects);
        for id in &next.removed_generations {
            self.generations.remove(id);
        }
        self.removed_generations.extend(next.removed_generations);
        for id in next.generations.keys() {
            self.removed_generations.remove(id);
        }
        self.generations.extend(next.generations);
        self.debug.extend(next.debug);
        self.contacts.extend(next.contacts);
        self.cameras.extend(next.cameras);
        if next.errors.is_some() {
            self.errors = next.errors;
        }
    }
    pub fn is_empty(&self) -> bool {
        !self.replace_all
            && self.objects.is_empty()
            && self.errors.is_none()
            && self.cameras.is_empty()
    }
    pub fn changed_objects(&self) -> impl Iterator<Item = (u64, Option<&Object>)> {
        self.objects
            .iter()
            .map(|(&id, state)| (id, state.as_ref().map(|state| &state.object)))
    }
    /// Object identity is separate from property values and their reference bindings.
    pub fn changes_object_properties(&self, state: &SceneState, id: u64, names: &[&str]) -> bool {
        let Some(next) = self.objects.get(&id) else {
            return false;
        };
        match (state.objects.get(&id), next) {
            (None, None) => false,
            (Some(_), None) | (None, Some(_)) => true,
            (Some(previous), Some(next)) => {
                if names
                    .iter()
                    .any(|&name| previous.properties.get(name) != next.object.properties.get(name))
                {
                    return true;
                }
                let previous = state
                    .reference_lifetimes
                    .range((id, String::new())..)
                    .take_while(|((owner, _), _)| *owner == id)
                    .filter_map(|((_, name), targets)| {
                        names.contains(&name.as_str()).then_some((name, targets))
                    });
                let next = next
                    .references
                    .iter()
                    .filter(|(name, _)| names.contains(&name.as_str()));
                !previous.eq(next)
            }
        }
    }
    pub fn changes_properties(&self, state: &SceneState, names: &[&str]) -> bool {
        if self.replace_all {
            return true;
        }
        self.objects.iter().any(|(&id, next)| {
            let previous = state.objects.get(&id);
            match (previous, next) {
                (None, None) => false,
                (Some(_), None) | (None, Some(_)) => true,
                (Some(previous), Some(next)) => {
                    previous.generation != next.object.generation
                        || previous.type_name != next.object.type_name
                        || self.changes_object_properties(state, id, names)
                }
            }
        })
    }
    /// Replay results already own restored objects; consuming them avoids a second graph allocation.
    pub fn apply_owned(self, state: &mut SceneState) {
        if self.replace_all {
            *state = SceneState::default();
        }
        for (id, object) in self.objects {
            let keys: Vec<_> = state
                .reference_lifetimes
                .range((id, String::new())..)
                .take_while(|((owner, _), _)| *owner == id)
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                state.reference_lifetimes.remove(&key);
            }
            if let Some(object) = object {
                Arc::unwrap_or_clone(object).apply_owned(state);
            } else {
                state.objects.remove(&id);
            }
        }
        for id in self.removed_generations {
            state.generations.remove(&id);
        }
        state.generations.extend(self.generations);
        for (id, draw) in self.debug {
            if let Some(draw) = draw {
                state.debug_draw.insert(id, draw);
            } else {
                state.debug_draw.remove(&id);
            }
        }
        for (id, contacts) in self.contacts {
            if let Some(contacts) = contacts {
                state.contacts.insert(id, contacts);
            } else {
                state.contacts.remove(&id);
            }
        }
        if let Some(errors) = self.errors {
            state.errors = errors;
        }
        for (name, camera) in self.cameras {
            if let Some(camera) = camera {
                state.cameras.insert(name, camera);
            } else {
                state.cameras.remove(&name);
            }
        }
    }
    pub fn apply(&self, state: &mut SceneState) {
        if self.replace_all {
            *state = SceneState::default();
        }
        for (&id, object) in &self.objects {
            // Removing only this owner's bindings avoids a full-map scan for each changed Actor.
            let keys: Vec<_> = state
                .reference_lifetimes
                .range((id, String::new())..)
                .take_while(|((owner, _), _)| *owner == id)
                .map(|(key, _)| key.clone())
                .collect();
            for key in keys {
                state.reference_lifetimes.remove(&key);
            }
            if let Some(object) = object {
                object.apply(state);
            } else {
                state.objects.remove(&id);
            }
        }
        for id in &self.removed_generations {
            state.generations.remove(id);
        }
        state.generations.extend(&self.generations);
        for (&id, draw) in &self.debug {
            if let Some(draw) = draw {
                state.debug_draw.insert(id, draw.clone());
            } else {
                state.debug_draw.remove(&id);
            }
        }
        for (&id, contacts) in &self.contacts {
            if let Some(contacts) = contacts {
                state.contacts.insert(id, contacts.clone());
            } else {
                state.contacts.remove(&id);
            }
        }
        if let Some(errors) = &self.errors {
            state.errors.clone_from(errors);
        }
        apply_cameras(&mut state.cameras, &self.cameras);
    }
}

fn apply_cameras(
    cameras: &mut BTreeMap<String, SourceCamera>,
    changes: &BTreeMap<String, Option<SourceCamera>>,
) {
    for (name, camera) in changes {
        if let Some(camera) = camera {
            cameras.insert(name.clone(), *camera);
        } else {
            cameras.remove(name);
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum AnalysisInput {
    Reset(usize),
    Zone(u64, String),
    Event(u64, u16, String),
    Buffer {
        id: u64,
        kind: String,
        bytes: Arc<[u8]>,
        little_endian: bool,
        frame: usize,
    },
    Retain {
        first_frame: usize,
        accounted_bytes: usize,
    },
}
impl AnalysisInput {
    fn bytes(&self) -> usize {
        256 + match self {
            Self::Reset(_) | Self::Retain { .. } => 0,
            Self::Zone(_, name) | Self::Event(_, _, name) => name.len(),
            Self::Buffer { kind, bytes, .. } => kind.len() + bytes.len(),
        }
    }
    #[cfg(test)]
    fn apply(&self, analysis: &mut Analysis) -> Result<(), Error> {
        match self {
            Self::Reset(frame) => {
                *analysis = Analysis::default();
                analysis.first_retained_frame = *frame;
                Ok(())
            }
            Self::Zone(id, name) => analysis.add_zone(*id, name.clone()),
            Self::Event(id, event, name) => analysis.add_event(*id, *event, name.clone()),
            Self::Buffer {
                id,
                kind,
                bytes,
                little_endian,
                frame,
            } => analysis.ingest(
                *id,
                kind,
                bytes,
                if *little_endian {
                    pvd_protocol::Endian::Little
                } else {
                    pvd_protocol::Endian::Big
                },
                *frame,
            ),
            Self::Retain {
                first_frame,
                accounted_bytes,
            } => analysis.retain_captured(*first_frame, *accounted_bytes),
        }
    }

    pub(super) fn apply_shared(
        &self,
        analysis: &mut SharedAnalysis,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        match self {
            Self::Reset(frame) => analysis.reset_at(*frame)?,
            Self::Zone(id, name) => analysis.add_zone(*id, name.clone())?,
            Self::Event(id, event, name) => analysis.add_event(*id, *event, name.clone())?,
            Self::Buffer {
                id,
                kind,
                bytes,
                little_endian,
                frame,
            } => analysis.ingest_with_cancel(
                *id,
                kind,
                bytes,
                if *little_endian {
                    pvd_protocol::Endian::Little
                } else {
                    pvd_protocol::Endian::Big
                },
                *frame,
                cancelled,
            )?,
            Self::Retain {
                first_frame,
                accounted_bytes,
            } => analysis.retain_captured(*first_frame, *accounted_bytes)?,
        }
        check_replay_cancelled(cancelled)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct StateIndex {
    objects: BTreeMap<u64, u64>,
    generations: BTreeMap<u64, u32>,
    debug: BTreeMap<u64, u64>,
    contacts: BTreeMap<u64, u64>,
    errors: Vec<ErrorEvent>,
    cameras: BTreeMap<String, SourceCamera>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct IndexUpdate {
    objects: Vec<(u64, Option<u64>)>,
    generations: BTreeMap<u64, u32>,
    debug: Vec<(u64, Option<u64>)>,
    contacts: Vec<(u64, Option<u64>)>,
    errors: Option<Vec<ErrorEvent>>,
    cameras: BTreeMap<String, Option<SourceCamera>>,
}
impl IndexUpdate {
    fn apply(&self, index: &mut StateIndex) {
        fn update(map: &mut BTreeMap<u64, u64>, changes: &[(u64, Option<u64>)]) {
            for &(id, offset) in changes {
                if let Some(offset) = offset {
                    map.insert(id, offset);
                } else {
                    map.remove(&id);
                }
            }
        }
        update(&mut index.objects, &self.objects);
        update(&mut index.debug, &self.debug);
        update(&mut index.contacts, &self.contacts);
        index.generations.extend(&self.generations);
        if let Some(errors) = &self.errors {
            index.errors.clone_from(errors);
        }
        apply_cameras(&mut index.cameras, &self.cameras);
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredFrame {
    info: FrameInfo,
    update: IndexUpdate,
    analysis: Vec<AnalysisInput>,
}
#[derive(Serialize, Deserialize)]
enum Record {
    Object(StoredObject),
    Debug(u64, Arc<Vec<Primitive>>),
    Contacts(u64, Arc<[Contact]>),
    Checkpoint(StateIndex),
    Frame(StoredFrame),
    Analysis(Box<Analysis>),
    Value(Value),
    References(BTreeMap<u64, u32>),
    AnalysisPages(u64),
    Undo(undo::StoredUndo),
    KeyedObject(KeyedObject),
}

// Variant and field order must match the owned records consumed by existing history readers.
#[derive(Serialize)]
enum RecordRef<'a> {
    Object(StoredObjectRef<'a>),
    Debug(u64, &'a [Primitive]),
    Contacts(u64, &'a [Contact]),
    Checkpoint(&'a StateIndex),
    Frame(StoredFrameRef<'a>),
    Analysis(&'a Analysis),
    Value(&'a Value),
    References(&'a BTreeMap<u64, u32>),
    AnalysisPages(u64),
    Undo(&'a undo::StoredUndo),
    KeyedObject(KeyedObjectRef<'a>),
}

#[derive(Serialize)]
struct KeyedObjectRef<'a> {
    id: u64,
    generation: u32,
    type_name: &'a str,
    keys: u64,
    values: SmallValues<'a>,
    references: SmallReferences<'a>,
    large_properties: &'a BTreeMap<&'a str, u64>,
    large_references: &'a BTreeMap<&'a str, u64>,
}

struct SmallValues<'a>(SmallProperties<'a>);

impl Serialize for SmallValues<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence =
            serializer.serialize_seq(Some(self.0.values.len() - self.0.external.len()))?;
        for (key, value) in self.0.values {
            if !self.0.external.contains_key(key.as_str()) {
                sequence.serialize_element(value)?;
            }
        }
        sequence.end()
    }
}

#[derive(Serialize)]
struct StoredFrameRef<'a> {
    info: &'a FrameInfo,
    update: &'a IndexUpdate,
    analysis: &'a [AnalysisInput],
}

#[derive(Serialize)]
struct StoredObjectRef<'a> {
    object: ObjectRef<'a>,
    references: SmallReferences<'a>,
    large_properties: &'a BTreeMap<&'a str, u64>,
    large_references: &'a BTreeMap<&'a str, u64>,
}

#[derive(Serialize)]
struct ObjectRef<'a> {
    id: u64,
    generation: u32,
    type_name: &'a str,
    properties: SmallProperties<'a>,
}

struct SmallProperties<'a> {
    values: &'a BTreeMap<String, Value>,
    external: &'a BTreeMap<&'a str, u64>,
}

impl Serialize for SmallProperties<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.values.len() - self.external.len()))?;
        for (key, value) in self.values {
            if !self.external.contains_key(key.as_str()) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

#[derive(Clone)]
enum ReferenceEntries<'a> {
    Object(std::collections::btree_map::Iter<'a, String, Arc<BTreeMap<u64, u32>>>),
    Scene {
        owner: u64,
        entries: std::collections::btree_map::Range<'a, (u64, String), Arc<BTreeMap<u64, u32>>>,
    },
}

impl<'a> Iterator for ReferenceEntries<'a> {
    type Item = (&'a String, &'a BTreeMap<u64, u32>);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Object(entries) => entries.next().map(|(key, value)| (key, value.as_ref())),
            Self::Scene { owner, entries } => {
                let ((id, key), value) = entries.next()?;
                (*id == *owner).then_some((key, value.as_ref()))
            }
        }
    }
}

struct SmallReferences<'a> {
    entries: ReferenceEntries<'a>,
    external: &'a BTreeMap<&'a str, u64>,
}

impl Serialize for SmallReferences<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let length = self.entries.clone().count() - self.external.len();
        let mut map = serializer.serialize_map(Some(length))?;
        for (key, value) in self.entries.clone() {
            if !self.external.contains_key(key.as_str()) {
                map.serialize_entry(key, value)?;
            }
        }
        map.end()
    }
}

#[derive(Clone, Debug)]
struct FrameLocation {
    undo: Option<NonZeroU64>,
    info: FrameInfo,
    offset: u64,
    scene_generation: Option<u32>,
}
#[derive(Clone, Debug)]
struct SegmentView {
    committed_end: u64,
    path: PathBuf,
    frames: crate::history::SharedIndex<FrameLocation>,
    checkpoints: Arc<BTreeMap<usize, u64>>,
    analysis_base: u64,
    bytes: u64,
    resources: Resources,
    lease: ResourceLease,
    readers: SegmentReaders,
    previous_frame: Option<usize>,
    analysis_continuity: bool,
}

#[derive(Clone, Debug, Default)]
struct SegmentReaders(Arc<AtomicUsize>);

impl SegmentReaders {
    fn active(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst) != 0
    }
}

struct SegmentReadGuard(SegmentReaders);

impl SegmentReadGuard {
    fn new(readers: SegmentReaders) -> Self {
        readers.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(readers)
    }
}

impl Drop for SegmentReadGuard {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

struct RetiredSegment {
    #[cfg(test)]
    committed_end: u64,
    path: PathBuf,
    bytes: u64,
    readers: SegmentReaders,
}

#[derive(Clone, Debug)]
struct ResourceLease(Arc<Mutex<Option<Vec<ResourceHandle>>>>);

impl ResourceLease {
    fn new(handles: Vec<ResourceHandle>) -> Self {
        Self(Arc::new(Mutex::new(Some(handles))))
    }

    fn extend(&self, handles: Vec<ResourceHandle>) -> Result<(), Error> {
        self.0
            .lock()
            .map_err(|_| error("history resource lease lock poisoned"))?
            .as_mut()
            .ok_or_else(|| error("history resource lease expired"))?
            .extend(handles);
        Ok(())
    }

    fn invalidate(&self) -> Result<(), Error> {
        let handles = self
            .0
            .lock()
            .map_err(|_| error("history resource lease lock poisoned"))?
            .take();
        drop(handles);
        Ok(())
    }

    fn available(&self) -> bool {
        self.0.lock().is_ok_and(|handles| handles.is_some())
    }

    fn downgrade(&self) -> WeakResourceLease {
        WeakResourceLease(Arc::downgrade(&self.0))
    }

    fn bytes(&self) -> usize {
        self.0.lock().map_or(0, |handles| {
            handles.as_ref().map_or(0, |handles| {
                handles.capacity() * std::mem::size_of::<ResourceHandle>()
            })
        })
    }
}

#[derive(Clone, Debug)]
struct WeakResourceLease(Weak<Mutex<Option<Vec<ResourceHandle>>>>);

impl WeakResourceLease {
    fn available(&self) -> bool {
        self.0
            .upgrade()
            .is_some_and(|lease| lease.lock().is_ok_and(|handles| handles.is_some()))
    }
}

struct LimitedBuffer<'a> {
    bytes: &'a mut Vec<u8>,
}

impl Write for LimitedBuffer<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let required = self
            .bytes
            .len()
            .checked_add(input.len())
            .filter(|n| *n <= MAX_RECORD)
            .ok_or_else(|| std::io::Error::other("history record exceeds 64 MiB"))?;
        if required > self.bytes.capacity() {
            let capacity = required
                .max(self.bytes.capacity().saturating_mul(2))
                .min(MAX_RECORD);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct HistoryReader {
    buffer: BufReader<File>,
    capture: Option<Arc<resources::capture::CaptureBacking>>,
    position: Option<u64>,
    resources: Option<ResourceReader>,
    _segment_guard: Option<SegmentReadGuard>,
}

fn read_record(reader: &mut HistoryReader, offset: u64, size: u64) -> Result<Record, Error> {
    read_record_with_cancel(reader, offset, size, &mut || false)
}

fn read_record_with_cancel(
    reader: &mut HistoryReader,
    offset: u64,
    size: u64,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Record, Error> {
    let bytes = read_record_bytes_with_cancel(reader, offset, size, cancelled)?;
    reader.decode(&bytes, cancelled)
}

impl HistoryReader {
    fn decode(&self, bytes: &[u8], cancelled: &mut impl FnMut() -> bool) -> Result<Record, Error> {
        if bytes.first() == Some(&object_keys::RECORD_TAG)
            && self
                .capture
                .as_ref()
                .is_some_and(|capture| capture.format == capture::Format::V1)
        {
            return Err(error("keyed objects require project capture version 02"));
        }
        if self.capture.is_some() {
            capture::record::decode(bytes, cancelled)
        } else {
            decode_record(bytes, cancelled)
        }
    }
}

fn decode_record(bytes: &[u8], cancelled: &mut impl FnMut() -> bool) -> Result<Record, Error> {
    let (record, tail) = postcard::take_from_bytes(bytes)?;
    check_replay_cancelled(cancelled)?;
    if !tail.is_empty() {
        return Err(error("history record has trailing bytes"));
    }
    Ok(record)
}

fn read_record_bytes_with_cancel(
    reader: &mut HistoryReader,
    offset: u64,
    size: u64,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, Error> {
    read_record_bytes_with_limit(reader, offset, size, MAX_RECORD, cancelled)
}

fn read_record_bytes_with_limit(
    reader: &mut HistoryReader,
    offset: u64,
    size: u64,
    maximum: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<Vec<u8>, Error> {
    check_replay_cancelled(cancelled)?;
    if offset & (1 << 63) != 0 {
        return reader
            .resources
            .as_mut()
            .ok_or_else(|| error("shared history resource directory is missing"))?
            .read_limited(offset, maximum, cancelled);
    }
    if let Some(capture) = &reader.capture {
        let mut header = [0; 36];
        capture.read_exact_at(offset, &mut header)?;
        let length = u32::from_le_bytes(header[..4].try_into()?) as usize;
        if length > maximum.min(MAX_RECORD)
            || offset
                .checked_add(36)
                .and_then(|n| n.checked_add(length as u64))
                .is_none_or(|end| end > size)
        {
            return Err(error("capture record lies outside its committed file"));
        }
        let mut bytes = vec![0; length];
        for (index, chunk) in bytes.chunks_mut(64 << 10).enumerate() {
            check_replay_cancelled(cancelled)?;
            capture.read_exact_at(offset + 36 + (index * (64 << 10)) as u64, chunk)?;
        }
        if blake3::hash(&bytes).as_bytes() != &header[4..] {
            return Err(error("capture record checksum mismatch"));
        }
        check_replay_cancelled(cancelled)?;
        return Ok(bytes);
    }
    if offset < 8 || offset.checked_add(36).is_none_or(|end| end > size) {
        return Err(error("history record offset is outside the committed file"));
    }
    let relative = reader
        .position
        .and_then(|position| i64::try_from(i128::from(offset) - i128::from(position)).ok());
    reader.position = None;
    if let Some(relative) = relative {
        reader.buffer.seek_relative(relative)?;
    } else {
        reader.buffer.seek(SeekFrom::Start(offset))?;
    }
    let mut length = [0; 4];
    reader.buffer.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length > maximum.min(MAX_RECORD) || offset + 36 + length as u64 > size {
        return Err(error("invalid history record length"));
    }
    let mut digest = [0; 32];
    reader.buffer.read_exact(&mut digest)?;
    check_replay_cancelled(cancelled)?;
    let mut bytes = vec![0; length];
    let mut checksum = blake3::Hasher::new();
    for chunk in bytes.chunks_mut(64 << 10) {
        check_replay_cancelled(cancelled)?;
        reader.buffer.read_exact(chunk)?;
        checksum.update(chunk);
    }
    reader.position = Some(offset + 36 + length as u64);
    check_replay_cancelled(cancelled)?;
    if checksum.finalize().as_bytes() != &digest {
        return Err(error("history record checksum mismatch"));
    }
    Ok(bytes)
}

#[derive(Clone, Debug)]
pub(crate) struct DiskRecording {
    _owner: Option<Arc<CacheDirectory>>,
    segments: Arc<VecDeque<Arc<SegmentView>>>,
    active: Option<SegmentView>,
    frames: crate::FrameIndex,
    pub stats: HistoryStats,
    analysis_cache: AnalysisCache,
    analysis_pages: Option<analysis_pages::AnalysisPageReader>,
    archive: Option<persistent::ArchiveReader>,
}
impl DiskRecording {
    pub(crate) fn resident_analysis_at_with_cancel(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<crate::analysis::resident::ResidentAnalysis, Error> {
        match self.resident_analysis_once(frame, cancelled) {
            Err(failure)
                if failure
                    .downcast_ref::<crate::analysis::resident::ResidentBudgetExceeded>()
                    .is_some() =>
            {
                check_replay_cancelled(cancelled)?;
                self.analysis_cache.clear_resident();
                self.analysis_pages
                    .as_ref()
                    .ok_or_else(|| error("history has no shared analysis reader"))?
                    .clone()
                    .reclaim_with_cancel(cancelled)?;
                self.resident_analysis_once(frame, cancelled)
            }
            result => result,
        }
    }

    fn resident_analysis_once(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<crate::analysis::resident::ResidentAnalysis, Error> {
        check_replay_cancelled(cancelled)?;
        let segment = self.segment(frame)?;
        let _root = segment.resources.pin(segment.analysis_base)?;
        let mut file = Self::open(segment)?;
        let Record::AnalysisPages(root) = read_record_with_cancel(
            &mut file,
            segment.analysis_base,
            segment.committed_end,
            cancelled,
        )?
        else {
            return Err(error("history baseline is not a shared analysis root"));
        };
        let mut reader = self
            .analysis_pages
            .as_ref()
            .ok_or_else(|| error("history has no shared analysis reader"))?
            .clone();
        let mut analysis = reader.read(root, &segment.resources, cancelled)?;
        let first = segment.frames.first().unwrap().info.index;
        for location in segment
            .frames
            .iter()
            .filter(|location| location.info.index > first)
            .take_while(|location| location.info.index <= frame)
        {
            let Record::Frame(stored) = read_record_with_cancel(
                &mut file,
                location.offset,
                segment.committed_end,
                cancelled,
            )?
            else {
                return Err(error("history frame record type mismatch"));
            };
            for input in &stored.analysis {
                analysis_pages::apply(input, &mut analysis, cancelled)?;
            }
        }
        check_replay_cancelled(cancelled)?;
        Ok(analysis)
    }
    pub(crate) fn is_frame_retained(&self, frame: usize) -> bool {
        let _transition = self.history_guard().ok();
        if self
            .frames
            .binary_search_by_key(&frame, |info| info.index)
            .is_ok()
            && self
                .archive
                .as_ref()
                .is_some_and(|archive| archive.contains(frame))
        {
            return true;
        }
        self.segment(frame)
            .is_ok_and(|segment| segment.lease.available())
    }
    fn locations(
        &self,
        first: usize,
        last: usize,
    ) -> impl Iterator<Item = (&SegmentView, &FrameLocation)> {
        let position = self.segments.partition_point(|segment| {
            segment
                .frames
                .last()
                .is_some_and(|frame| frame.info.index < first)
        });
        self.segments
            .iter()
            .skip(position)
            .map(Arc::as_ref)
            .chain(&self.active)
            .take_while(move |segment| {
                segment
                    .frames
                    .first()
                    .is_some_and(|frame| frame.info.index <= last)
            })
            .flat_map(move |segment| {
                let position = segment
                    .frames
                    .partition_point(|frame| frame.info.index < first);
                segment
                    .frames
                    .iter()
                    .skip(position)
                    .take_while(move |frame| frame.info.index <= last)
                    .map(move |location| (segment, location))
            })
    }

    fn segment(&self, frame: usize) -> Result<&SegmentView, Error> {
        if self
            .frames
            .binary_search_by_key(&frame, |info| info.index)
            .is_err()
        {
            return Err(error("frame is outside the visible retained history"));
        }
        let position = self.segments.partition_point(|segment| {
            segment
                .frames
                .last()
                .is_some_and(|last| last.info.index < frame)
        });
        self.segments
            .get(position)
            .map(Arc::as_ref)
            .or(self.active.as_ref())
            .filter(|segment| {
                segment
                    .frames
                    .first()
                    .is_some_and(|first| first.info.index <= frame)
                    && segment
                        .frames
                        .last()
                        .is_some_and(|last| frame <= last.info.index)
            })
            .ok_or_else(|| error(format!("frame {frame} is outside retained disk history")))
    }
    fn open(segment: &SegmentView) -> Result<HistoryReader, Error> {
        let guard = SegmentReadGuard::new(segment.readers.clone());
        if !segment.lease.available() {
            return Err(error("history segment expired"));
        }
        if let Some(capture) = segment.resources.capture_backing() {
            return Ok(HistoryReader {
                buffer: BufReader::new(capture.file.try_clone()?),
                capture: Some(capture.clone()),
                position: None,
                resources: Some(segment.resources.reader()),
                _segment_guard: Some(guard),
            });
        }
        let file = File::open(&segment.path).map_err(|e| {
            error(format!(
                "history segment unavailable (it may have expired): {e}"
            ))
        })?;
        if file.metadata()?.len() < segment.committed_end {
            return Err(error("truncated history segment"));
        }
        let mut reader = BufReader::with_capacity(256 << 10, file);
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(error("invalid internal history header"));
        }
        Ok(HistoryReader {
            buffer: reader,
            capture: None,
            position: Some(8),
            resources: Some(segment.resources.reader()),
            _segment_guard: Some(guard),
        })
    }
    pub fn frames(&self) -> crate::FrameIndex {
        self.frames.clone()
    }

    pub(crate) fn capture_format(&self) -> Option<capture::Format> {
        if let Some(format) = self
            .archive
            .as_ref()
            .and_then(persistent::ArchiveReader::capture_format)
        {
            return Some(format);
        }
        self.segments
            .front()
            .map(Arc::as_ref)
            .or(self.active.as_ref())?
            .resources
            .capture_backing()
            .map(|backing| backing.format)
    }
    pub fn scene_generation(&self, frame: usize) -> Option<u32> {
        let _transition = self.history_guard().ok()?;
        if let Some(recording) = self.archived(frame, &mut || false).ok().flatten() {
            return recording.disk.as_ref()?.scene_generation(frame);
        }
        let frames = &self.segment(frame).ok()?.frames;
        let position = frames
            .binary_search_by_key(&frame, |entry| entry.info.index)
            .ok()?;
        frames[position].scene_generation
    }
    pub fn verify_replay(&self) -> Result<(), Error> {
        let mut prior: Option<(usize, StateIndex)> = None;
        let mut position = 0;
        while let Some(info) = self.frames.get(position) {
            let _transition = self.history_guard()?;
            let owned = self.owned_segment(info.index, &mut || false)?;
            let segment = owned.as_ref();
            let last = segment
                .frames
                .last()
                .ok_or_else(|| error("history segment has no frames"))?
                .info
                .index;
            position = self.frames.partition_point(|info| info.index <= last);
            let mut reader = Self::open(segment)?;
            let first = segment
                .frames
                .first()
                .ok_or_else(|| error("history segment has no frames"))?;
            let continuous = prior.as_ref().is_some_and(|(frame, _)| {
                Some(*frame) == segment.previous_frame
                    && frame.checked_add(1) == Some(first.info.index)
            });
            let mut sequential = if continuous {
                prior.take().unwrap().1
            } else {
                let offset = segment.checkpoints[&first.info.index];
                let Record::Checkpoint(index) =
                    read_record(&mut reader, offset, segment.committed_end)?
                else {
                    return Err(error("history checkpoint record type mismatch"));
                };
                index
            };
            for location in &segment.frames {
                let Record::Frame(frame) =
                    read_record(&mut reader, location.offset, segment.committed_end)?
                else {
                    return Err(error("history frame record type mismatch"));
                };
                if frame.info.index != location.info.index
                    || frame.info.timestamp != location.info.timestamp
                    || frame.info.scene_id != location.info.scene_id
                {
                    return Err(error("history frame metadata differs from its index"));
                }
                if location.undo.is_some() {
                    let to = location
                        .info
                        .index
                        .checked_sub(1)
                        .ok_or_else(|| error("first frame cannot have undo"))?;
                    let inverse =
                        undo::read(&mut reader, segment, location, to, &mut || false)?.unwrap();
                    if continuous || location.info.index != first.info.index {
                        inverse.verify_before(&sequential, &frame.update)?;
                    }
                }
                frame.update.apply(&mut sequential);
                if let Some(&offset) = segment.checkpoints.get(&location.info.index) {
                    let Record::Checkpoint(index) =
                        read_record(&mut reader, offset, segment.committed_end)?
                    else {
                        return Err(error("history checkpoint record type mismatch"));
                    };
                    if index != sequential {
                        return Err(error(format!(
                            "sequential and checkpoint indexes differ at frame {}",
                            location.info.index
                        )));
                    }
                }
            }
            prior = Some((segment.frames.last().unwrap().info.index, sequential));
        }
        Ok(())
    }
    #[cfg(test)]
    fn state_at(&self, frame: usize) -> Result<SceneState, Error> {
        self.state_at_with_cancel(frame, &mut || false)
    }

    pub fn state_at_with_cancel(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<SceneState, Error> {
        check_replay_cancelled(cancelled)?;
        let _transition = self.history_guard()?;
        if let Some(recording) = self.archived(frame, cancelled)? {
            return recording
                .disk
                .as_ref()
                .unwrap()
                .state_at_with_cancel(frame, cancelled);
        }
        let segment = self.segment(frame)?;
        let mut reader = Self::open(segment)?;
        let (&checkpoint, &offset) = segment
            .checkpoints
            .range(..=frame)
            .next_back()
            .ok_or_else(|| error("history checkpoint missing"))?;
        let Record::Checkpoint(mut index) =
            read_record_with_cancel(&mut reader, offset, segment.committed_end, cancelled)?
        else {
            return Err(error("history checkpoint record type mismatch"));
        };
        for location in segment
            .frames
            .iter()
            .filter(|f| f.info.index > checkpoint && f.info.index <= frame)
        {
            let Record::Frame(frame) = read_record_with_cancel(
                &mut reader,
                location.offset,
                segment.committed_end,
                cancelled,
            )?
            else {
                return Err(error("history frame record type mismatch"));
            };
            frame.update.apply(&mut index);
        }
        let mut state = SceneState {
            generations: index.generations,
            errors: index.errors,
            cameras: index.cameras,
            ..Default::default()
        };
        let mut values = BTreeMap::new();
        let mut references = BTreeMap::<u64, Arc<BTreeMap<u64, u32>>>::new();
        for (id, offset) in index.objects {
            let object = replay::read_object(
                &mut reader,
                offset,
                segment.committed_end,
                &mut values,
                &mut references,
                cancelled,
            )?;
            if object.object.id != id {
                return Err(error("history object ID mismatch"));
            }
            object.apply(&mut state);
        }
        for (id, offset) in index.debug {
            let Record::Debug(owner, draw) =
                read_record_with_cancel(&mut reader, offset, segment.committed_end, cancelled)?
            else {
                return Err(error("history draw record type mismatch"));
            };
            if owner != id {
                return Err(error("history renderer ID mismatch"));
            }
            state.debug_draw.insert(id, draw);
        }
        for (id, offset) in index.contacts {
            let Record::Contacts(owner, contacts) =
                read_record_with_cancel(&mut reader, offset, segment.committed_end, cancelled)?
            else {
                return Err(error("history contacts record type mismatch"));
            };
            if owner != id {
                return Err(error("history contact owner mismatch"));
            }
            state.contacts.insert(id, contacts);
        }
        check_replay_cancelled(cancelled)?;
        Ok(state)
    }
    pub fn analysis_at_with_cancel(
        &self,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<SharedAnalysis, Error> {
        check_replay_cancelled(cancelled)?;
        let _transition = self.history_guard()?;
        if let Some(recording) = self.archived(frame, cancelled)? {
            return recording
                .disk
                .as_ref()
                .unwrap()
                .analysis_at_with_cancel(frame, cancelled);
        }
        if self.analysis_pages.is_some() {
            return self
                .resident_analysis_at_with_cancel(frame, cancelled)
                .map(Into::into);
        }
        let segment = self.segment(frame)?;
        let mut reader = Self::open(segment)?;
        let first = segment.frames.first().unwrap().info.index;
        let (start, mut analysis) =
            if let Some((start, cached)) = self.analysis_cache.best(first, frame) {
                (start, cached)
            } else {
                let Record::Analysis(mut analysis) = read_record_with_cancel(
                    &mut reader,
                    segment.analysis_base,
                    segment.committed_end,
                    cancelled,
                )?
                else {
                    return Err(error("history analysis record type mismatch"));
                };
                analysis.restore_sharing();
                check_replay_cancelled(cancelled)?;
                self.analysis_cache
                    .insert(first, (*analysis).clone().into(), &segment.lease);
                (first, (*analysis).into())
            };
        for location in segment
            .frames
            .iter()
            .filter(|location| location.info.index > start)
            .take_while(|f| f.info.index <= frame)
        {
            let Record::Frame(stored) = read_record_with_cancel(
                &mut reader,
                location.offset,
                segment.committed_end,
                cancelled,
            )?
            else {
                return Err(error("history frame record type mismatch"));
            };
            for input in &stored.analysis {
                check_replay_cancelled(cancelled)?;
                input.apply_shared(&mut analysis, cancelled)?;
            }
        }
        check_replay_cancelled(cancelled)?;
        Ok(analysis)
    }
}

pub(crate) struct HistoryWriter {
    config: HistoryConfig,
    owner: Option<Arc<CacheDirectory>>,
    resources: Option<Resources>,
    completed: Arc<VecDeque<Arc<SegmentView>>>,
    completed_bytes: u64,
    retired: Vec<RetiredSegment>,
    frame_infos: crate::FrameIndex,
    active: Option<SegmentWriter>,
    previous: Option<WriteState>,
    sequence: usize,
    analysis_continuity: bool,
    warning: Option<String>,
    retry_after: Instant,
    metrics: Metrics,
    analysis_cache: AnalysisCache,
    analysis_pages: Option<analysis_pages::HistoryPages>,
    archive: Option<persistent::ArchiveReader>,
    retention_stopped: bool,
    pending_archive_bytes: u64,
    persistent_baseline: Option<persistent::PressureBaseline>,
    bank_index: resources::capture::BankIndex,
    schema_reader_bytes: Option<usize>,
    cancellation: IngestCancellation,
}

impl HistoryWriter {
    pub(crate) fn directory(&self) -> Option<&Path> {
        self.owner.as_ref().map(|owner| owner.path.as_path())
    }

    pub fn new(mut config: HistoryConfig) -> Result<Self, Error> {
        if config.disk_budget_bytes < 4096 || config.segment_frames == 0 {
            return Err(error(
                "history budget must be at least 4096 bytes and segment frame count positive",
            ));
        }
        config.segment_target_bytes = config
            .segment_target_bytes
            .min(config.disk_budget_bytes / 4)
            .max(1024);
        let (owner, warning) = match CacheDirectory::new(&config.directory) {
            Ok(owner) => (Some(owner), None),
            Err(error) => (
                None,
                Some(format!(
                    "History directory unavailable; live decoding continues: {error}"
                )),
            ),
        };
        let resources = owner
            .as_ref()
            .map(|owner| Resources::new(owner.clone(), config.disk_budget_bytes));
        Ok(Self {
            config,
            owner,
            resources,
            completed: Default::default(),
            completed_bytes: 0,
            retired: Vec::new(),
            frame_infos: Default::default(),
            active: None,
            previous: None,
            sequence: 0,
            analysis_continuity: false,
            warning,
            retry_after: Instant::now(),
            metrics: Metrics::default(),
            analysis_cache: AnalysisCache::default(),
            analysis_pages: None,
            archive: None,
            retention_stopped: false,
            pending_archive_bytes: 0,
            persistent_baseline: None,
            bank_index: Default::default(),
            schema_reader_bytes: None,
            cancellation: IngestCancellation::default(),
        })
    }

    pub(crate) fn new_paged(config: HistoryConfig) -> Result<Self, Error> {
        let mut history = Self::new(config)?;
        let Some(resources) = &history.resources else {
            return Ok(history);
        };
        history.analysis_pages = Some(analysis_pages::HistoryPages::new(resources)?);
        Ok(history)
    }

    pub(crate) fn set_metrics(&mut self, metrics: Metrics) {
        if let Some(segment) = &mut self.active {
            segment.metrics = metrics.clone();
        }
        self.metrics = metrics;
    }

    pub(crate) fn set_cancellation(&mut self, cancellation: IngestCancellation) {
        self.cancellation = cancellation;
    }

    fn bytes(&self) -> u64 {
        self.completed_bytes
            .saturating_add(self.pending_archive_bytes)
            .saturating_add(self.active.as_ref().map_or(0, |segment| segment.view.bytes))
            .saturating_add(self.resources.as_ref().map_or(0, Resources::physical_bytes))
            .saturating_add(
                self.retired
                    .iter()
                    .map(|segment| &segment.bytes)
                    .copied()
                    .sum::<u64>(),
            )
    }

    fn lease_bytes(&self) -> usize {
        self.completed
            .iter()
            .map(|segment| segment.lease.bytes())
            .sum::<usize>()
            + self
                .active
                .as_ref()
                .map_or(0, |segment| segment.view.lease.bytes())
    }

    pub(crate) fn invalidate_schema_size(&mut self) {
        self.schema_reader_bytes = None;
    }

    pub fn cache_bytes(&self) -> usize {
        self.resources.as_ref().map_or(0, Resources::metadata_bytes)
            + self.bank_index.cache_bytes()
            + self.lease_bytes()
            + self.analysis_cache.bytes()
            + self
                .analysis_pages
                .as_ref()
                .map_or(0, analysis_pages::HistoryPages::bytes)
            + self.active.as_ref().map_or(0, |segment| {
                segment.state.cache_bytes
                    + segment.state.key_lists.manager_bytes()
                    + segment.record_buffer.capacity()
                    + segment.state.roots.len() * 96
            })
            + self.previous.as_ref().map_or(0, |state| {
                state.cache_bytes + state.roots.len() * 96 + state.key_lists.manager_bytes()
            })
    }

    pub(super) fn initial_analysis(&self) -> Option<SharedAnalysis> {
        self.analysis_pages
            .as_ref()
            .map(|pages| pages.current().clone().into())
    }

    pub(super) fn failure_analysis(
        &mut self,
        frame: usize,
    ) -> Result<Option<SharedAnalysis>, Error> {
        self.analysis_pages
            .as_mut()
            .map(|pages| pages.failure_analysis(frame))
            .transpose()
    }

    pub(super) fn reclaim_analysis(&mut self) -> Result<(), Error> {
        let _progress = self.metrics.measure(Stage::HistoryAnalysisReclaim);
        self.analysis_cache.clear_resident();
        if let Some(pages) = &mut self.analysis_pages {
            pages.reclaim()?;
        }
        Ok(())
    }

    fn frames(&self) -> usize {
        self.frame_infos.len()
    }

    fn evict_one(&mut self) -> Result<bool, Error> {
        let _timer = self.metrics.measure(Stage::HistoryEvict);
        if self.retired.len() >= 64 {
            return Err(error("history contains too many files pending removal"));
        }
        let Some(segment) = self.completed.front() else {
            return Ok(false);
        };
        if self.archive.as_ref().is_some_and(|archive| {
            !segment
                .frames
                .first()
                .is_some_and(|frame| archive.contains(frame.info.index))
                || !segment
                    .frames
                    .last()
                    .is_some_and(|frame| archive.contains(frame.info.index))
        }) {
            return Err(error(
                "complete-session history needs a sealed bank before cache reclamation",
            ));
        }
        segment.lease.invalidate()?;
        self.analysis_cache.discard_expired();
        if segment.readers.active() || self.owner.as_ref().unwrap().remove(&segment.path).is_err() {
            self.retired.push(RetiredSegment {
                #[cfg(test)]
                committed_end: segment.committed_end,
                path: segment.path.clone(),
                bytes: segment.bytes,
                readers: segment.readers.clone(),
            });
        }
        let segment = Arc::make_mut(&mut self.completed).pop_front().unwrap();
        self.completed_bytes -= segment.bytes;
        if self.archive.is_none() {
            self.frame_infos.discard_front(segment.frames.len());
        }
        if let Some(resources) = &self.resources {
            let _timer = self.metrics.measure(Stage::HistoryCollect);
            resources.collect()?;
        }
        Ok(true)
    }

    fn close_segment(&mut self) {
        if let Some(segment) = self.active.take() {
            let SegmentWriter {
                mut view,
                mut file,
                limit,
                state,
                failed_write,
                ..
            } = segment;
            {
                let _timer = self.metrics.measure(Stage::HistoryFlush);
                if !failed_write {
                    let _ = file.flush();
                }
            }
            let (file, pending) = file.into_parts();
            drop(pending);
            view.bytes = file.metadata().map_or(limit, |metadata| metadata.len());
            drop(file);
            self.previous = Some(state);
            if !view.frames.is_empty() {
                self.completed_bytes += view.bytes;
                Arc::make_mut(&mut self.completed).push_back(Arc::new(view));
                self.analysis_continuity = true;
            } else if self.owner.as_ref().unwrap().remove(&view.path).is_err() {
                self.retired.push(RetiredSegment {
                    #[cfg(test)]
                    committed_end: view.committed_end,
                    path: view.path,
                    bytes: view.bytes,
                    readers: view.readers,
                });
            }
        }
    }

    fn break_analysis_continuity(&mut self) {
        self.close_segment();
        self.analysis_continuity = false;
    }

    #[cfg(test)]
    pub fn append(
        &mut self,
        info: &FrameInfo,
        state: &SceneState,
        patch: &SceneUpdate,
        analysis: &Analysis,
        inputs: Vec<AnalysisInput>,
    ) {
        self.append_shared(info, state, patch, &analysis.clone().into(), inputs, None)
            .unwrap();
    }

    pub fn append_shared(
        &mut self,
        info: &FrameInfo,
        state: &SceneState,
        patch: &SceneUpdate,
        analysis: &SharedAnalysis,
        inputs: Vec<AnalysisInput>,
        source: Option<&crate::Recording>,
    ) -> Result<(), Error> {
        if self.retention_stopped {
            return Ok(());
        }
        let _timer = self.metrics.measure(Stage::HistoryAppend);
        let preparation = if let Some(pages) = &mut self.analysis_pages {
            match analysis.resident() {
                Some(analysis) => pages.adopt(info.index, analysis, &inputs),
                None => pages.advance(info.index, &inputs),
            }
        } else if analysis.legacy().is_none() {
            Err(error(
                "legacy history cannot store a resident analysis root",
            ))
        } else {
            Ok(())
        };
        if let Err(failure) = preparation {
            self.warning = Some(format!("Shared analysis history unavailable: {failure}"));
            self.close_segment();
            self.analysis_continuity = false;
            if self.archive.is_some() {
                self.stop_retention(failure);
            }
            return Ok(());
        }
        if Instant::now() < self.retry_after {
            return Ok(());
        }
        if let Err(error) =
            self.append_inner(info, state, patch, analysis.legacy(), &inputs, source)
        {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|failure| failure.kind() == std::io::ErrorKind::Interrupted)
            {
                return Err(error);
            }
            self.cancellation.check()?;
            self.warning = Some(format!(
                "Disk history unavailable; live decoding continues: {error}"
            ));
            self.close_segment();
            self.previous = None;
            self.analysis_continuity = false;
            if let Some(resources) = &self.resources {
                let _ = resources.collect();
            }
            self.retry_after = Instant::now() + Duration::from_secs(2);
            if self.archive.is_some() {
                self.stop_retention(error.to_string());
            }
        } else {
            self.warning = None;
        }
        Ok(())
    }

    fn reclaim(
        &mut self,
        reservation: u64,
        resources: &Resources,
        opportunistic_compaction: &mut bool,
    ) -> Result<bool, Error> {
        let usable = self
            .config
            .disk_budget_bytes
            .saturating_sub(resources.reserve_bytes());
        loop {
            self.retired.retain(|segment| {
                segment.readers.active()
                    || self.owner.as_ref().unwrap().remove(&segment.path).is_err()
            });
            if self.bytes().saturating_add(reservation) <= usable
                && self.lease_bytes() <= 128 << 20
                && resources.metadata_bytes()
                    <= resources.metadata_capacity().saturating_sub(16 << 20)
            {
                return Ok(true);
            }
            {
                let _timer = self.metrics.measure(Stage::HistoryCollect);
                resources.collect()?;
            }
            if self.bytes().saturating_add(reservation) <= usable
                && self.lease_bytes() <= 128 << 20
                && resources.metadata_bytes()
                    <= resources.metadata_capacity().saturating_sub(16 << 20)
            {
                return Ok(true);
            }
            let temporary_fits = self.bytes().saturating_add(resources.reserve_bytes())
                <= self.config.disk_budget_bytes;
            let free_fits =
                available_disk_space(&self.owner.as_ref().unwrap().path)?.is_none_or(|space| {
                    space.saturating_sub(self.config.min_free_disk_bytes)
                        >= resources.reserve_bytes()
                });
            // Completed segments provide an alternative to repeated copying. With no
            // evictable segment, compaction remains necessary for capacity recovery.
            let compacted = if temporary_fits
                && free_fits
                && (*opportunistic_compaction || self.completed.is_empty())
            {
                *opportunistic_compaction = false;
                let _timer = self.metrics.measure(Stage::HistoryCompact);
                resources.compact_one()?
            } else {
                false
            };
            if compacted {
                continue;
            }
            if !self.evict_one()? {
                return Ok(false);
            }
        }
    }

    fn append_inner(
        &mut self,
        info: &FrameInfo,
        state: &SceneState,
        patch: &SceneUpdate,
        analysis: Option<&Analysis>,
        inputs: &[AnalysisInput],
        source: Option<&crate::Recording>,
    ) -> Result<(), Error> {
        if self.owner.is_none() {
            let owner = CacheDirectory::new(&self.config.directory)?;
            self.resources = Some(Resources::new(owner.clone(), self.config.disk_budget_bytes));
            self.owner = Some(owner);
        }
        if patch.replace_all || !patch.removed_generations.is_empty() {
            // Index deltas have no generation tombstones; replacements need an independent checkpoint.
            self.close_segment();
            self.previous = None;
        }
        let resources = self.resources.as_ref().unwrap().clone();
        self.retired.retain(|segment| {
            segment.readers.active() || self.owner.as_ref().unwrap().remove(&segment.path).is_err()
        });
        if self.retired.len() >= 64 {
            return Err(error("history contains too many files pending removal"));
        }
        let planned_records = patch
            .objects
            .len()
            .saturating_mul(3)
            .saturating_add(patch.debug.len())
            .saturating_add(patch.contacts.len())
            .saturating_add(2);
        // Complete-session pressure is handled by sealing before this append;
        // rolling eviction cannot retire an unsealed persistent segment.
        if self.archive.is_none() && resources.needs_reclaim(planned_records) {
            self.close_segment();
            resources.collect()?;
            while resources.needs_reclaim(planned_records) && self.evict_one()? {}
        }
        if self.active.as_ref().is_some_and(|segment| {
            segment
                .payload_bytes()
                .saturating_sub(segment.baseline_bytes)
                >= self.config.segment_target_bytes
                || segment.view.frames.len() >= self.config.segment_frames
        }) {
            self.close_segment();
        }
        let mut opportunistic_compaction = true;
        loop {
            if self.active.is_none() {
                let previous = self.previous.take().filter(|state| {
                    state.last_frame.and_then(|frame| frame.checked_add(1)) == Some(info.index)
                });
                self.active = Some(SegmentWriter::with_state(
                    self.owner.as_ref().unwrap(),
                    self.sequence,
                    resources.clone(),
                    previous,
                    self.analysis_continuity,
                )?);
                self.sequence += 1;
            }
            let segment = self.active.as_mut().unwrap();
            segment.metrics = self.metrics.clone();
            let prepared = if let Some(pages) = &mut self.analysis_pages {
                segment.prepare_paged(info, state, patch, inputs, pages)
            } else {
                segment.prepare(
                    info,
                    state,
                    patch,
                    analysis.ok_or_else(|| error("legacy analysis baseline is unavailable"))?,
                    inputs,
                )
            };
            let mut prepared = match prepared {
                Err(failure)
                    if self.analysis_pages.is_some()
                        && failure
                            .downcast_ref::<crate::analysis::resident::ResidentBudgetExceeded>()
                            .is_some() =>
                {
                    self.reclaim_analysis()?;
                    resources.collect()?;
                    self.active.as_mut().unwrap().prepare_paged(
                        info,
                        state,
                        patch,
                        inputs,
                        self.analysis_pages.as_mut().unwrap(),
                    )?
                }
                result => result?,
            };
            let reservation = prepared.reservation(&resources);
            {
                let _timer = self.metrics.measure(Stage::HistoryReservation);
                if !self.reclaim(reservation, &resources, &mut opportunistic_compaction)? {
                    drop(prepared);
                    if self.active.as_ref().unwrap().view.frames.is_empty() {
                        return Err(error(
                            "current scene and shared history resources cannot fit within the disk history budget",
                        ));
                    }
                    // The new lease keeps current roots while the completed lease can release older versions.
                    self.close_segment();
                    continue;
                }
                loop {
                    let Some(space) = available_disk_space(&self.owner.as_ref().unwrap().path)?
                    else {
                        if self.archive.is_some() && self.config.min_free_disk_bytes != 0 {
                            return Err(error(
                                "free disk space cannot verify the configured reserve on this platform",
                            ));
                        }
                        break;
                    };
                    if space.saturating_sub(self.config.min_free_disk_bytes)
                        >= reservation.saturating_add(resources.reserve_bytes())
                    {
                        break;
                    }
                    if !self.evict_one()? {
                        return Err(error(
                            "free disk space cannot preserve the configured reserve",
                        ));
                    }
                }
            }
            if let (Some(archive), Some(source)) = (&self.archive, source)
                && prepared.exceeds_bank_target(
                    &mut self.bank_index,
                    self.active.as_ref().unwrap(),
                    archive.index_limit.saturating_sub(
                        *self
                            .schema_reader_bytes
                            .get_or_insert_with(|| capture::schema_reader_bytes(&source.classes)),
                    ),
                )
            {
                drop(prepared);
                self.seal_bank(source)?;
                continue;
            }
            let before = resources.physical_bytes();
            prepared.persist_resources(&resources)?;
            if let Some(publication) = prepared.take_analysis() {
                self.analysis_pages
                    .as_mut()
                    .ok_or_else(|| error("shared analysis publisher is unavailable"))?
                    .publish(publication)?;
            }
            let added = resources.physical_bytes().saturating_sub(before);
            let segment = self.active.as_mut().unwrap();
            let first = segment.view.frames.is_empty();
            segment.limit = self.config.disk_budget_bytes.saturating_sub(
                self.completed_bytes
                    .saturating_add(resources.physical_bytes())
                    .saturating_add(
                        self.retired
                            .iter()
                            .map(|segment| segment.bytes)
                            .sum::<u64>(),
                    ),
            );
            segment.commit(prepared, info, added)?;
            self.frame_infos.push(info.clone());
            self.metrics.committed(info.index);
            if first && self.analysis_pages.is_none() {
                self.analysis_cache.insert(
                    info.index,
                    analysis
                        .ok_or_else(|| error("legacy analysis baseline is unavailable"))?
                        .clone()
                        .into(),
                    &segment.view.lease,
                );
            }
            break;
        }
        while (self.archive.is_none() && self.frames() > MAX_INDEX_FRAMES)
            || self.completed.len() > 4096
            || self.lease_bytes() > 128 << 20
        {
            if !self.evict_one()? {
                break;
            }
        }
        Ok(())
    }

    fn snapshot(&self) -> DiskRecording {
        let active = self.active.as_ref().map(|segment| segment.view.clone());
        let first_frame = self.frame_infos.first().map_or(0, |frame| frame.index);
        DiskRecording {
            _owner: self.owner.clone(),
            segments: self.completed.clone(),
            active,
            frames: self.frame_infos.clone(),
            analysis_cache: self.analysis_cache.clone(),
            analysis_pages: self
                .analysis_pages
                .as_ref()
                .map(analysis_pages::HistoryPages::reader),
            archive: self.archive.clone(),
            stats: HistoryStats {
                disk_bytes: self
                    .bytes()
                    .saturating_add(self.archive.as_ref().map_or(0, |archive| archive.bytes())),
                persistent: self.archive.is_some(),
                cache_disk_bytes: self.bytes(),
                budget_bytes: self.config.disk_budget_bytes,
                first_frame,
                retained_frames: self.frames(),
                segments: self.completed.len()
                    + usize::from(self.active.is_some())
                    + self.archive.as_ref().map_or(0, |archive| archive.len()),
                warning: self.warning.clone(),
            },
        }
    }
}

/// None means this platform has no free-space probe; write errors still remain recoverable.
pub fn available_disk_space(path: &Path) -> Result<Option<u64>, Error> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut available = 0;
        unsafe {
            windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
                windows::core::PCWSTR(wide.as_ptr()),
                Some(&mut available),
                None,
                None,
            )?;
        }
        Ok(Some(available))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        Ok(None)
    }
}

pub(crate) struct Streaming {
    pub history: HistoryWriter,
    dirty: BTreeSet<u64>,
    errors_dirty: bool,
    cameras_dirty: BTreeSet<String>,
    pub pending: SceneUpdate,
    pub frame: Option<FrameInfo>,
    pub frames_seen: usize,
    requested_source: Option<FrameSourceKey>,
    automatic_source: Option<FrameSourceKey>,
    pub selected_source: Option<FrameSourceKey>,
    pub display_analysis: Option<SharedAnalysis>,
    pub display_source_frame: Option<(FrameSourceKey, SourceFrame)>,
    display_dirty: BTreeSet<u64>,
    display_cameras_dirty: BTreeSet<String>,
    display_errors_dirty: bool,
    display_dirty_bytes: usize,
    display_requires_full: bool,
    display_crossed_source: bool,
    pub state_bytes: usize,
    pub analysis_inputs: Vec<AnalysisInput>,
    analysis_input_bytes: usize,
    pub analysis_failure: Option<String>,
    pub errors: VecDeque<(usize, ErrorEvent)>,
}
impl Streaming {
    pub(crate) fn prepare_persistent(&mut self, source: &crate::Recording) -> Result<(), Error> {
        let planned_records = self.dirty.len().saturating_mul(3).saturating_add(2);
        self.history.prepare_persistent(source, planned_records)
    }
    pub(crate) fn directory_bytes(
        &mut self,
        index: usize,
        binding: FrameBinding,
        directory: &FrameDirectory,
    ) -> Result<usize, Error> {
        match directory.index_bytes_after_observe(index, binding, self.history.accepting_frames()) {
            Err(failure)
                if self.history.archive.is_some()
                    && failure.to_string().contains("limit exceeded") =>
            {
                self.history
                    .stop_retention(format!("complete-session navigation directory: {failure}"));
                directory.index_bytes_after_observe(index, binding, false)
            }
            result => result,
        }
    }
    pub fn new(history: HistoryWriter) -> Self {
        Self {
            history,
            dirty: BTreeSet::new(),
            errors_dirty: false,
            cameras_dirty: BTreeSet::new(),
            pending: SceneUpdate::default(),
            frame: None,
            frames_seen: 0,
            requested_source: None,
            automatic_source: None,
            selected_source: None,
            display_analysis: None,
            display_source_frame: None,
            display_dirty: BTreeSet::new(),
            display_cameras_dirty: BTreeSet::new(),
            display_errors_dirty: false,
            display_dirty_bytes: 0,
            display_requires_full: true,
            display_crossed_source: false,
            state_bytes: 0,
            analysis_inputs: Vec::new(),
            analysis_input_bytes: 0,
            analysis_failure: None,
            errors: VecDeque::new(),
        }
    }
    pub fn frame_directory() -> FrameDirectory {
        FrameDirectory::new(FrameDirectoryLimits {
            // The history writer retires its oldest segment after committing the new frame.
            frames: MAX_INDEX_FRAMES + 1,
            ..FrameDirectoryLimits::default()
        })
    }
    pub fn memory_bytes(&self) -> usize {
        self.state_bytes.saturating_add(self.display_dirty_bytes)
    }
    pub fn set_frame_source(&mut self, source: Option<FrameSourceKey>) -> bool {
        self.requested_source = source;
        self.select_frame_source()
    }
    fn select_frame_source(&mut self) -> bool {
        let selected = self.requested_source.or(self.automatic_source);
        if selected == self.selected_source {
            return false;
        }
        self.selected_source = selected;
        self.refresh_frame_source();
        true
    }
    pub fn refresh_frame_source(&mut self) {
        self.pending = SceneUpdate::default();
        self.frame = None;
        self.display_source_frame = None;
        self.display_requires_full = true;
        self.clear_display_changes();
    }
    fn clear_display_changes(&mut self) {
        self.display_dirty.clear();
        self.display_cameras_dirty.clear();
        self.display_errors_dirty = false;
        self.display_dirty_bytes = 0;
        self.display_crossed_source = false;
    }
    fn observe_frame_source(&mut self, source: FrameSourceKey) {
        let promotes_physx = matches!(
            self.automatic_source,
            Some(FrameSourceKey::Captured {
                section: FrameSection::Apex,
                ..
            })
        ) && matches!(
            source,
            FrameSourceKey::Captured {
                section: FrameSection::PhysX,
                ..
            }
        );
        if self.automatic_source.is_none() || promotes_physx {
            self.automatic_source = Some(source);
            self.select_frame_source();
        }
    }
    pub fn prune_frame_sources(&self, directory: &mut FrameDirectory) {
        let first = self
            .history
            .frame_infos
            .first()
            .map_or(self.frames_seen, |info| info.index);
        directory.discard_before(first);
    }
    fn track_display_changes(&mut self, action: &Action, ids: &[u64], limit: usize) {
        if self.display_requires_full {
            return;
        }
        for &id in ids {
            if self.display_dirty.insert(id) {
                self.display_dirty_bytes = self.display_dirty_bytes.saturating_add(64);
            }
        }
        match action {
            Action::Error(_) => self.display_errors_dirty = true,
            Action::Camera(name, _) | Action::RemoveCamera(name)
                if self.display_cameras_dirty.insert(name.clone()) =>
            {
                self.display_dirty_bytes = self.display_dirty_bytes.saturating_add(name.len() + 64);
            }
            _ => {}
        }
        if self.display_dirty_bytes > MAX_DISPLAY_DIRTY_BYTES || self.memory_bytes() > limit {
            // Bookkeeping may collapse to a replacement without retaining unselected object payloads.
            self.display_requires_full = true;
            self.clear_display_changes();
        }
    }
    pub fn analysis_input(&mut self, input: AnalysisInput) {
        let bytes = input.bytes();
        if self.analysis_input_bytes.saturating_add(bytes) > 64 << 20 {
            // A fresh segment anchors analysis after unusually long gaps between scene frames.
            self.history.break_analysis_continuity();
            self.analysis_inputs.clear();
            self.analysis_input_bytes = 0;
        }
        self.analysis_input_bytes += bytes;
        self.analysis_inputs.push(input);
    }
    pub fn action(
        &mut self,
        action: Action,
        state: &mut SceneState,
        limit: usize,
    ) -> Result<(), Error> {
        let mut replaced_ids = Vec::new();
        if let Action::ReplaceDebug(draw) = &action {
            replaced_ids.extend(state.debug_draw.keys().chain(draw.keys()).copied());
            replaced_ids.sort_unstable();
            replaced_ids.dedup();
        }
        let ids: &[u64] = match &action {
            Action::Create(object) => std::slice::from_ref(&object.id),
            Action::Destroy(id)
            | Action::Set(id, ..)
            | Action::Push(id, ..)
            | Action::Remove(id, ..)
            | Action::Debug(id, ..)
            | Action::Contacts(id, ..) => std::slice::from_ref(id),
            Action::ClearDebug(ids) => ids,
            Action::ReplaceDebug(_) => &replaced_ids,
            Action::Error(_) | Action::Camera(..) | Action::RemoveCamera(_) => &[],
        };
        let before = action.scene_bytes(state);
        if let Action::Error(event) = &action {
            self.errors_dirty = true;
            if self.errors.len() == MAX_ERRORS {
                self.errors.pop_front();
            }
            self.errors.push_back((self.frames_seen, event.clone()));
        }
        if let Action::Camera(name, _) | Action::RemoveCamera(name) = &action {
            self.cameras_dirty.insert(name.clone());
        }
        action.apply(state);
        if state.errors.len() > MAX_ERRORS {
            state.errors.drain(..state.errors.len() - MAX_ERRORS);
        }
        let after = action.scene_bytes(state);
        self.state_bytes = crate::updated_state_bytes(self.state_bytes, before, after);
        if self.state_bytes > limit || state.generations.len() > 1_000_000 {
            return Err(error(
                "current live scene exceeds its bounded object/resource budget",
            ));
        }
        self.track_display_changes(&action, ids, limit);
        self.dirty.extend(ids.iter().copied());
        Ok(())
    }
    pub fn complete(
        &mut self,
        info: FrameInfo,
        binding: FrameBinding,
        state: &SceneState,
        recording: &mut crate::Recording,
    ) -> Result<(), Error> {
        let _timer = self.history.metrics.measure(Stage::FrameComplete);
        if info.index != self.frames_seen {
            return Err(error("live frame boundary is not in capture order"));
        }
        let next_frame = self
            .frames_seen
            .checked_add(1)
            .ok_or_else(|| error("live frame count overflow"))?;
        let patch = {
            let _timer = self.history.metrics.measure(Stage::FrameCapture);
            let mut patch = SceneUpdate::capture(state, &self.dirty, self.errors_dirty);
            patch.cameras = self
                .cameras_dirty
                .iter()
                .map(|name| (name.clone(), state.cameras.get(name).copied()))
                .collect();
            patch
        };
        self.history.append_shared(
            &info,
            state,
            &patch,
            &recording.analysis,
            std::mem::take(&mut self.analysis_inputs),
            Some(recording),
        )?;
        let retained = self
            .history
            .frame_infos
            .last()
            .is_some_and(|last| last.index == info.index);
        let directory = &mut recording.frame_sources;
        directory.observe(info.index, binding, retained)?;
        self.frames_seen = next_frame;
        self.prune_frame_sources(directory);
        self.analysis_input_bytes = 0;
        self.observe_frame_source(binding.source);
        if self.selected_source == Some(binding.source) {
            let display_patch = if self.display_requires_full {
                SceneUpdate::full(state)
            } else if self.display_crossed_source {
                let mut display =
                    SceneUpdate::capture(state, &self.display_dirty, self.display_errors_dirty);
                display.cameras = self
                    .display_cameras_dirty
                    .iter()
                    .map(|name| (name.clone(), state.cameras.get(name).copied()))
                    .collect();
                display
            } else {
                patch
            };
            self.pending.merge(display_patch);
            self.display_source_frame = Some((
                binding.source,
                SourceFrame {
                    index: info.index,
                    ordinal: directory
                        .source(binding.source)
                        .ok_or_else(|| {
                            error("live frame source is absent from its observed directory")
                        })?
                        .observed_frames()
                        .checked_sub(1)
                        .ok_or_else(|| error("live source frame ordinal is absent"))?,
                    source_object: binding.source_object,
                    scene: binding.scene,
                },
            ));
            self.frame = Some(info);
            self.display_analysis = Some(recording.analysis.clone());
            self.display_requires_full = false;
            self.clear_display_changes();
        } else {
            self.display_crossed_source = true;
        }
        self.dirty.clear();
        self.errors_dirty = false;
        self.cameras_dirty.clear();
        Ok(())
    }
    pub fn snapshot(&self) -> DiskRecording {
        self.history.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_state_bytes;
    use std::{fs, sync::atomic::Ordering};

    fn encoded(value: &impl Serialize) -> Vec<u8> {
        postcard::to_io(value, Vec::<u8>::new()).unwrap()
    }

    #[test]
    fn event_readers_keep_retired_files_in_the_physical_budget_until_closed() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-reader-budget-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            segment_frames: 1,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        for index in 0..2 {
            history.append(
                &FrameInfo {
                    index,
                    timestamp: index as u64,
                    scene_id: 1,
                },
                &SceneState::default(),
                &SceneUpdate::default(),
                &Analysis::default(),
                Vec::new(),
            );
        }
        let snapshot = history.snapshot();
        let path = snapshot.segments[0].path.clone();
        let reader = DiskRecording::open(&snapshot.segments[0]).unwrap();
        assert!(history.evict_one().unwrap());
        assert!(path.exists());
        assert_eq!(history.retired.len(), 1);
        assert!(DiskRecording::open(&snapshot.segments[0]).is_err());
        let physical = || {
            history
                .owner
                .as_ref()
                .unwrap()
                .files
                .lock()
                .unwrap()
                .iter()
                .map(|path| fs::metadata(path).unwrap().len())
                .sum::<u64>()
        };
        assert_eq!(history.bytes(), physical());
        drop(reader);
        let mut opportunistic_compaction = true;
        history
            .reclaim(
                0,
                &history.resources.as_ref().unwrap().clone(),
                &mut opportunistic_compaction,
            )
            .unwrap();
        assert!(!path.exists());
        assert!(history.retired.is_empty());
        let physical: u64 = history
            .owner
            .as_ref()
            .unwrap()
            .files
            .lock()
            .unwrap()
            .iter()
            .map(|path| fs::metadata(path).unwrap().len())
            .sum();
        assert_eq!(history.bytes(), physical);
        drop((snapshot, history));
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cursor_crosses_boundary_removals_reuse_and_expiration_without_pinning_old_history() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-boundary-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            segment_frames: 2,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut state = SceneState::default();
        let mut expected = Vec::new();
        for frame in 0..8 {
            if frame == 0 || frame == 4 {
                Action::Create(Object {
                    id: 7,
                    generation: u32::from(frame == 4),
                    type_name: "Actor".into(),
                    properties: BTreeMap::new(),
                })
                .apply(&mut state);
                state.debug_draw.insert(
                    9,
                    Arc::new(vec![Primitive::Point {
                        instance: 7,
                        position: [frame as f32, 0.0, 0.0],
                        color: 42,
                    }]),
                );
                state.contacts.insert(
                    7,
                    vec![Contact {
                        position: [1.0; 3],
                        normal: [0.0, 1.0, 0.0],
                        shapes: [7, 8],
                        separation: -0.1,
                        normal_force: 1.0,
                        faces: [0, 1],
                        normal_force_available: true,
                    }]
                    .into(),
                );
            }
            if frame == 2 {
                Action::Destroy(7).apply(&mut state);
                state.debug_draw.remove(&9);
                state.contacts.remove(&7);
            }
            if state.objects.contains_key(&7) {
                Action::Set(7, "value".into(), Value::U64(frame as u64)).apply(&mut state);
            }
            let patch = SceneUpdate::capture(&state, &[7, 9].into(), false);
            history.append(
                &FrameInfo {
                    index: frame,
                    timestamp: frame as u64,
                    scene_id: 1,
                },
                &state,
                &patch,
                &Analysis::default(),
                Vec::new(),
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            expected.push(state.clone());
        }
        let mut recording = crate::RecordingBuilder::new(pvd_protocol::Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: pvd_protocol::Endian::Little,
        })
        .recording;
        recording.disk = Some(history.snapshot());
        recording.frames = recording.disk.as_ref().unwrap().frames();
        assert!(recording.is_frame_retained(0));
        recording.disk.as_ref().unwrap().verify_replay().unwrap();
        let mut cursor = ReplayCursor::default();
        let mut displayed = SceneState::default();
        for frame in (0..8).chain((0..7).rev()).chain([1, 6, 0]) {
            let replay = cursor.seek(&recording, frame, || false).unwrap();
            replay.update.apply(&mut displayed);
            assert_eq!(displayed, expected[frame], "frame {frame}");
        }
        history.close_segment();
        assert!(history.evict_one().unwrap());
        recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
        let replay = cursor.seek(&recording, 7, || false).unwrap();
        replay.update.apply(&mut displayed);
        assert_eq!(displayed, expected[7]);
        while history.evict_one().unwrap() {}
        assert!(!recording.is_frame_retained(0));
        assert!(recording.frame_info(0).is_some());
        assert!(cursor.seek(&recording, 0, || false).is_err());
        drop((cursor, recording, history));
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn borrowed_history_records_preserve_owned_wire_bytes() {
        let nan = f32::from_bits(0x7fc0_1234);
        let object = Object {
            id: 77,
            generation: 3,
            type_name: "physx3::PxScene".into(),
            properties: [
                ("scalar".into(), Value::F32(nan)),
                ("zero".into(), Value::F64(-0.0)),
                ("large".into(), Value::Bytes(vec![7; 8192].into())),
            ]
            .into(),
        };
        let references: BTreeMap<String, BTreeMap<u64, u32>> = [
            ("small".into(), [(1, 2)].into()),
            ("large_refs".into(), (1..100).map(|id| (id, 1)).collect()),
        ]
        .into();
        let large_properties = [("large", 128)].into();
        let large_references = [("large_refs", 256)].into();
        let mut stored_object = object.clone();
        stored_object.properties.remove("large");
        let mut stored_references = references.clone();
        stored_references.remove("large_refs");
        let owned = Record::Object(StoredObject {
            object: stored_object,
            references: stored_references,
            large_properties: [("large".into(), 128)].into(),
            large_references: [("large_refs".into(), 256)].into(),
        });
        let shared_references = references
            .iter()
            .map(|(key, targets)| (key.clone(), Arc::new(targets.clone())))
            .collect::<BTreeMap<_, _>>();
        let borrowed = RecordRef::Object(StoredObjectRef {
            object: ObjectRef {
                id: object.id,
                generation: object.generation,
                type_name: &object.type_name,
                properties: SmallProperties {
                    values: &object.properties,
                    external: &large_properties,
                },
            },
            references: SmallReferences {
                entries: ReferenceEntries::Object(shared_references.iter()),
                external: &large_references,
            },
            large_properties: &large_properties,
            large_references: &large_references,
        });
        assert_eq!(encoded(&owned), encoded(&borrowed));

        let draw = vec![Primitive::Text {
            instance: 77,
            position: [nan, -0.0, 1.0],
            size: 2.0,
            color: 0xff112233,
            text: "关节".into(),
        }];
        let contacts = vec![Contact {
            position: [1.0, 2.0, 3.0],
            normal: [0.0, 1.0, 0.0],
            shapes: [4, 5],
            separation: -0.0,
            normal_force: nan,
            faces: [1, 2],
            normal_force_available: false,
        }];
        let index = StateIndex {
            objects: [(77, 1024)].into(),
            generations: [(77, 3)].into(),
            ..Default::default()
        };
        let frame = StoredFrame {
            info: FrameInfo {
                index: 15,
                timestamp: 8000,
                scene_id: 77,
            },
            update: IndexUpdate {
                objects: vec![(77, Some(1024)), (78, None)],
                ..Default::default()
            },
            analysis: vec![
                AnalysisInput::Reset(15),
                AnalysisInput::Zone(9, "Worker".into()),
            ],
        };
        let analysis = Analysis::default();
        let value = Value::Array(vec![Value::Ref(77), Value::F32(nan), Value::F32(-0.0)].into());
        let bindings: BTreeMap<u64, u32> = [(77, 3), (78, 0)].into();
        for (owned, borrowed) in [
            (
                Record::Debug(7, draw.clone().into()),
                RecordRef::Debug(7, &draw),
            ),
            (
                Record::Contacts(8, contacts.clone().into()),
                RecordRef::Contacts(8, &contacts),
            ),
            (
                Record::Checkpoint(index.clone()),
                RecordRef::Checkpoint(&index),
            ),
            (
                Record::Frame(frame.clone()),
                RecordRef::Frame(StoredFrameRef {
                    info: &frame.info,
                    update: &frame.update,
                    analysis: &frame.analysis,
                }),
            ),
            (
                Record::Analysis(Box::new(analysis.clone())),
                RecordRef::Analysis(&analysis),
            ),
            (Record::Value(value.clone()), RecordRef::Value(&value)),
            (
                Record::References(bindings.clone()),
                RecordRef::References(&bindings),
            ),
        ] {
            assert_eq!(encoded(&owned), encoded(&borrowed));
        }
        let mut scene = SceneState::default();
        scene.reference_lifetimes.extend(
            references
                .iter()
                .map(|(key, value)| ((77, key.clone()), Arc::new(value.clone()))),
        );
        scene
            .reference_lifetimes
            .insert((78, "next_owner".into()), Arc::new([(7, 2)].into()));
        let scene_references = SmallReferences {
            entries: ReferenceEntries::Scene {
                owner: 77,
                entries: scene.reference_lifetimes.range((77, String::new())..),
            },
            external: &large_references,
        };
        assert_eq!(
            encoded(&scene_references),
            encoded(
                &references
                    .iter()
                    .filter(|(key, _)| key.as_str() != "large_refs")
                    .collect::<BTreeMap<_, _>>()
            )
        );
    }

    #[test]
    fn shared_assignment_copies_preserve_held_lifetimes_and_generations() {
        let mut state = SceneState::default();
        for id in [1, 2] {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: "Actor".into(),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
        }
        Action::Set(1, "Target".into(), Value::Ref(2)).apply(&mut state);
        let initial = state.clone();
        let mut update = SceneUpdate::full(&state);
        let held = update.clone();
        assert!(Arc::ptr_eq(
            update.objects[&1].as_ref().unwrap(),
            held.objects[&1].as_ref().unwrap()
        ));
        Action::Set(1, "Name".into(), Value::String("changed".into())).apply(&mut state);
        Action::Destroy(2).apply(&mut state);
        update.merge(SceneUpdate::capture(&state, &[1, 2].into(), false));
        Action::Create(Object {
            id: 2,
            generation: 1,
            type_name: "Replacement".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
        update.merge(SceneUpdate::capture(&state, &[2].into(), false));
        let mut previous = SceneState::default();
        held.apply(&mut previous);
        assert_eq!(previous, initial);
        assert!(previous.resolve_reference(1, "Target", 2).is_some());
        update.apply(&mut previous);
        assert_eq!(previous, state);
        assert!(previous.resolve_reference(1, "Target", 2).is_none());
    }

    #[test]
    fn serialization_buffer_reuse_keeps_limits_and_releases_large_allocations() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-buffer-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let owner = CacheDirectory::new(&directory).unwrap();
        let mut writer = SegmentWriter::new(&owner, 0).unwrap();
        let value = Value::Bytes(vec![4; 2048].into());
        let first = writer.write(&RecordRef::Value(&value)).unwrap();
        let pointer = writer.record_buffer.as_ptr();
        let capacity = writer.record_buffer.capacity();
        assert!((2048..=SERIALIZATION_CACHE_BYTES).contains(&capacity));
        let second = writer.write(&RecordRef::Value(&value)).unwrap();
        assert_eq!(writer.record_buffer.as_ptr(), pointer);
        let committed = writer.view.bytes;
        writer.limit = committed + 36;
        assert!(writer.write(&RecordRef::Value(&value)).is_err());
        assert_eq!(writer.view.bytes, committed);
        assert_eq!(writer.record_buffer.capacity(), capacity);
        writer.limit = u64::MAX;
        let large = Value::Bytes(vec![5; SERIALIZATION_CACHE_BYTES + 1].into());
        writer.write(&RecordRef::Value(&large)).unwrap();
        assert_eq!(writer.record_buffer.capacity(), 0);
        writer.file.flush().unwrap();
        let mut reader = HistoryReader {
            buffer: BufReader::new(File::open(&writer.view.path).unwrap()),
            capture: None,
            position: Some(0),
            resources: None,
            _segment_guard: None,
        };
        for offset in [first, second] {
            let Record::Value(actual) =
                read_record(&mut reader, offset, writer.view.bytes).unwrap()
            else {
                panic!("expected value record")
            };
            assert_eq!(actual, value);
        }
        let mut bytes = vec![0; MAX_RECORD];
        assert!(LimitedBuffer { bytes: &mut bytes }.write(&[1]).is_err());
        assert_eq!(bytes.len(), MAX_RECORD);
        drop(reader);
        drop(writer);
        drop(owner);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cancelled_record_read_can_restart_at_an_exact_record_boundary() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-cancel-read-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let owner = CacheDirectory::new(&directory).unwrap();
        let mut writer = SegmentWriter::new(&owner, 0).unwrap();
        let value = Value::Bytes(vec![71; 256 << 10].into());
        let offset = writer.write(&RecordRef::Value(&value)).unwrap();
        writer.file.flush().unwrap();
        let mut reader = HistoryReader {
            capture: None,
            buffer: BufReader::new(File::open(&writer.view.path).unwrap()),
            position: Some(0),
            resources: None,
            _segment_guard: None,
        };
        let mut checkpoints = 0;
        let failure = read_record_with_cancel(&mut reader, offset, writer.view.bytes, &mut || {
            checkpoints += 1;
            checkpoints == 4
        })
        .err()
        .expect("the read must stop before consuming the complete record");
        assert_eq!(
            failure.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert_eq!(reader.position, None);
        assert!(reader.buffer.stream_position().unwrap() < writer.view.bytes);
        let Record::Value(restored) = read_record(&mut reader, offset, writer.view.bytes).unwrap()
        else {
            panic!("expected value record");
        };
        assert_eq!(restored, value);
        drop(reader);
        drop(writer);
        drop(owner);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn incremental_property_accounting_matches_complete_scene_scan() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-accounting-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut streaming = Streaming::new(history);
        let mut state = SceneState::default();
        let mut validation = crate::clip::ValidationState::default();
        let mut validated = SceneState::default();
        let exact = |state: &SceneState| {
            state
                .generations
                .keys()
                .copied()
                .chain(state.reference_lifetimes.keys().map(|(id, _)| *id))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|id| object_state_bytes(state, id))
                .sum::<usize>()
                + state
                    .cameras
                    .keys()
                    .map(|name| camera_entry_bytes(name))
                    .sum::<usize>()
        };
        let mut apply = |action: Action| {
            let probe = matches!(&action,
                Action::Push(1, key, _) | Action::Remove(1, key, _) if key == "Triangles");
            validation
                .apply(action.clone(), &mut validated, usize::MAX)
                .unwrap();
            streaming.action(action, &mut state, usize::MAX).unwrap();
            assert_eq!(streaming.state_bytes, exact(&state));
            assert_eq!(validation.state_bytes, exact(&validated));
            if probe {
                let values = &state.objects[&1].properties["Triangles"];
                assert_eq!(*values.array_item(255).unwrap(), Value::U64(42));
                let present = state.reference_lifetimes[&(1, "Triangles".into())].contains_key(&2);
                assert_eq!(
                    values.array_item(256).as_deref(),
                    present.then_some(&Value::Ref(2))
                );
            }
        };
        for id in 1..=4 {
            apply(Action::Create(Object {
                id,
                generation: 0,
                type_name: "Actor".into(),
                properties: BTreeMap::new(),
            }));
        }
        let mut mixed = Vec::with_capacity(2048);
        mixed.resize(1024, Value::U64(0));
        mixed.extend([
            Value::Array(vec![Value::Ref(2)].into()),
            Value::Ref(2),
            Value::Ref(2),
        ]);
        let held = Arc::new(mixed);
        apply(Action::Set(1, "Mixed".into(), Value::Array(held.clone())));
        apply(Action::Remove(1, "Mixed".into(), 2));
        apply(Action::Push(1, "Mixed".into(), 3));
        assert_eq!(held.len(), 1027);
        apply(Action::Push(17, "Orphan".into(), 2));
        apply(Action::Destroy(17));
        let packed = crate::MeshArray::from_le_bytes(
            crate::mesh_array::Element::Index16,
            [42, 0].repeat(256),
        )
        .unwrap();
        apply(Action::Set(1, "Triangles".into(), Value::MeshArray(packed)));
        apply(Action::Push(1, "Triangles".into(), 2));
        apply(Action::Push(1, "Triangles".into(), 2));
        apply(Action::Remove(1, "Triangles".into(), 2));
        for value in 0..25 {
            apply(Action::Camera(
                format!("camera-{}", value % 3),
                SourceCamera {
                    position: [value as f32, 0.0, 0.0],
                    up: [0.0, 1.0, 0.0],
                    target: [0.0, 0.0, 0.0],
                },
            ));
        }
        for index in 0..512 {
            let value = match index % 6 {
                0 => Value::String("场景".repeat(index % 83)),
                1 => Value::Bytes(vec![index as u8; index * 11].into()),
                2 => Value::Array(
                    vec![
                        Value::Ref(2),
                        Value::Array(vec![Value::Ref(3), Value::String("nested".into())].into()),
                    ]
                    .into(),
                ),
                3 => Value::Ref(4),
                4 => Value::F32(index as f32),
                _ => Value::Array(vec![Value::U64(1); index % 107].into()),
            };
            apply(Action::Set(
                1 + (index % 4) as u64,
                format!("property_{}", index % 23),
                value,
            ));
        }
        apply(Action::Push(1, "Shapes".into(), 2));
        apply(Action::Push(1, "Shapes".into(), 3));
        apply(Action::Remove(1, "Shapes".into(), 2));
        apply(Action::Destroy(2));
        apply(Action::Create(Object {
            id: 2,
            generation: 1,
            type_name: "Replacement".into(),
            properties: BTreeMap::new(),
        }));
        apply(Action::Set(
            2,
            "value".into(),
            Value::String("replacement".into()),
        ));
        let limit = streaming.state_bytes;
        let failure = streaming.action(
            Action::Set(
                2,
                "value".into(),
                Value::String("replacement is larger".into()),
            ),
            &mut state,
            limit,
        );
        assert!(failure.is_err());
        assert_eq!(
            streaming.state_bytes,
            state
                .generations
                .keys()
                .map(|&id| object_state_bytes(&state, id))
                .sum::<usize>()
                + state
                    .cameras
                    .keys()
                    .map(|name| camera_entry_bytes(name))
                    .sum::<usize>()
        );
        drop(streaming);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cached_frame_reservation_preserves_rotation_and_checkpoint_states() {
        for (segment_frames, budget, frames, payload) in [
            (3, 32 << 10, 40, 2048),
            (600, 32 << 10, 40, 2048),
            (600, 2 << 20, 130, 128),
        ] {
            let directory = std::env::temp_dir().join(format!(
                "pvd-reservation-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            let mut history = HistoryWriter::new(HistoryConfig {
                directory: directory.clone(),
                disk_budget_bytes: budget,
                segment_target_bytes: u64::MAX,
                segment_frames,
                min_free_disk_bytes: 0,
            })
            .unwrap();
            let mut state = SceneState::default();
            Action::Create(Object {
                id: 1,
                generation: 0,
                type_name: "Actor".into(),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
            let mut expected = Vec::new();
            for frame in 0..frames {
                if frame == 17 {
                    Action::Destroy(1).apply(&mut state);
                    Action::Create(Object {
                        id: 1,
                        generation: 1,
                        type_name: "Replacement".into(),
                        properties: BTreeMap::new(),
                    })
                    .apply(&mut state);
                }
                Action::Set(
                    1,
                    "data".into(),
                    Value::Bytes(vec![frame as u8; payload].into()),
                )
                .apply(&mut state);
                let patch = SceneUpdate::capture(&state, &[1].into(), false);
                history.append(
                    &FrameInfo {
                        index: frame,
                        timestamp: frame as u64,
                        scene_id: 1,
                    },
                    &state,
                    &patch,
                    &Analysis::default(),
                    Vec::new(),
                );
                assert!(history.warning.is_none(), "{:?}", history.warning);
                assert!(history.bytes() <= budget);
                expected.push(state.clone());
                let snapshot = history.snapshot();
                for retained in snapshot.frames().iter() {
                    assert_eq!(
                        snapshot.state_at(retained.index).unwrap(),
                        expected[retained.index]
                    );
                }
            }
            if frames == 130 {
                assert!(
                    history
                        .active
                        .as_ref()
                        .unwrap()
                        .view
                        .checkpoints
                        .contains_key(&120)
                );
            } else {
                assert!(history.sequence > 1);
                assert!(history.snapshot().stats.first_frame > 0);
            }
            drop(history);
            fs::remove_dir(directory).unwrap();
        }
    }

    #[test]
    fn property_change_queries_distinguish_identity_from_reference_rebinding() {
        let mut state = SceneState::default();
        for (id, type_name, properties) in [
            (
                1,
                "physx::PxShape",
                [("Geometry".into(), Value::Ref(2))].into(),
            ),
            (2, "physx::PxBoxGeometry", BTreeMap::new()),
        ] {
            state.objects.insert(
                id,
                Object {
                    id,
                    generation: 0,
                    type_name: type_name.into(),
                    properties,
                },
            );
        }
        state
            .reference_lifetimes
            .insert((1, "Geometry".into()), Arc::new([(2, 0)].into()));
        let mut next = state.clone();
        next.objects.get_mut(&2).unwrap().generation = 1;
        let patch = SceneUpdate::capture(&next, &[2].into(), false);
        assert!(patch.changes_properties(&state, &["Name"]));
        assert!(!patch.changes_object_properties(&state, 2, &["Name"]));
        assert_eq!(
            patch
                .changed_objects()
                .next()
                .unwrap()
                .1
                .unwrap()
                .generation,
            1
        );

        next.reference_lifetimes
            .insert((1, "Geometry".into()), Arc::new([(2, 1)].into()));
        let patch = SceneUpdate::capture(&next, &[1].into(), false);
        assert_eq!(state.objects[&1].properties, next.objects[&1].properties);
        assert!(patch.changes_object_properties(&state, 1, &["Geometry"]));
        assert!(!patch.changes_object_properties(&state, 1, &["Name"]));
        assert!(!patch.changes_object_properties(&state, 2, &["Geometry"]));
        let bound = next.clone();
        next.reference_lifetimes.remove(&(1, "Geometry".into()));
        let removed = SceneUpdate::capture(&next, &[1].into(), false);
        let added = SceneUpdate::capture(&bound, &[1].into(), false);
        assert!(removed.changes_object_properties(&bound, 1, &["Name", "Geometry"]));
        assert!(added.changes_object_properties(&next, 1, &["Name", "Geometry"]));
        next.objects.remove(&1);
        let patch = SceneUpdate::capture(&next, &[1].into(), false);
        assert!(patch.changes_object_properties(&state, 1, &["Geometry"]));
        assert!(patch.changed_objects().next().unwrap().1.is_none());
    }

    #[test]
    fn closed_segment_accounts_for_uncommitted_tail_without_masking_truncation() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-history-tail-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        history.append(
            &FrameInfo {
                index: 0,
                timestamp: 0,
                scene_id: 1,
            },
            &SceneState::default(),
            &SceneUpdate::default(),
            &Analysis::default(),
            Vec::new(),
        );
        let total = history.bytes();
        let active = history.active.as_mut().unwrap();
        let committed = active.view.bytes;
        active.file.write_all(&[37; 13]).unwrap();
        history.close_segment();
        assert_eq!(history.bytes(), total + 13);
        let snapshot = history.snapshot();
        assert_eq!(snapshot.stats.disk_bytes, total + 13);
        assert_eq!(snapshot.state_at(0).unwrap(), SceneState::default());
        let path = &snapshot.segments[0].path;
        assert_eq!(fs::metadata(path).unwrap().len(), committed + 13);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(committed - 1)
            .unwrap();
        assert!(
            snapshot
                .state_at(0)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
        drop(snapshot);
        drop(history);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn shared_disk_snapshots_preserve_indices_without_pinning_expired_files() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-shared-index-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            segment_frames: 2,
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut snapshots = Vec::new();
        for index in 0..4 {
            history.append(
                &FrameInfo {
                    index,
                    timestamp: index as u64,
                    scene_id: 1,
                },
                &SceneState::default(),
                &SceneUpdate::default(),
                &Analysis::default(),
                Vec::new(),
            );
            assert!(history.warning.is_none(), "{:?}", history.warning);
            snapshots.push(history.snapshot());
        }
        assert!(Arc::ptr_eq(&snapshots[2].segments, &snapshots[3].segments));
        assert_eq!(snapshots[2].active.as_ref().unwrap().frames.len(), 1);
        assert_eq!(snapshots[3].active.as_ref().unwrap().frames.len(), 2);
        for (position, snapshot) in snapshots.iter().enumerate() {
            assert_eq!(snapshot.frames().len(), position + 1);
            assert_eq!(snapshot.state_at(position).unwrap(), SceneState::default());
            assert!(snapshot.state_at(position + 1).is_err());
        }
        let expired = snapshots[3].segments[0].path.clone();
        assert!(history.evict_one().unwrap());
        assert!(!expired.exists());
        assert!(snapshots[0].state_at(0).is_err());
        let retained = history.snapshot();
        assert_eq!(retained.frames().first().unwrap().index, 2);
        assert_eq!(retained.stats.retained_frames, 2);
        assert_eq!(retained.state_at(3).unwrap(), SceneState::default());
        drop(retained);
        drop(snapshots);
        drop(history);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[ignore = "synthetic million-frame index benchmark; no SDK or disk I/O throughput claim"]
    fn synthetic_million_frame_snapshot_profile() {
        let directory = std::env::temp_dir().join(format!(
            "pvd-index-profile-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut history = HistoryWriter::new(HistoryConfig {
            directory: directory.clone(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut segment = SegmentView {
            committed_end: 8,
            path: PathBuf::new(),
            frames: Default::default(),
            checkpoints: Default::default(),
            analysis_base: 0,
            bytes: 0,
            resources: history.resources.as_ref().unwrap().clone(),
            lease: ResourceLease::new(Vec::new()),
            readers: SegmentReaders::default(),
            previous_frame: None,
            analysis_continuity: false,
        };
        for index in 0..1_000_000 {
            if segment.frames.len() == 600 {
                Arc::make_mut(&mut history.completed).push_back(Arc::new(segment.clone()));
                segment.frames.clear();
            }
            let info = FrameInfo {
                index,
                timestamp: index as u64,
                scene_id: 1,
            };
            history.frame_infos.push(info.clone());
            segment.frames.push(FrameLocation {
                undo: None,
                info,
                offset: 0,
                scene_generation: None,
            });
        }
        let mut active = SegmentWriter::new(history.owner.as_ref().unwrap(), 0).unwrap();
        segment.path = active.view.path;
        active.view = segment;
        history.active = Some(active);
        let start = Instant::now();
        for _ in 0..600 {
            std::hint::black_box(history.snapshot());
        }
        let shared_us = start.elapsed().as_secs_f64() * 1e6 / 600.0;
        let start = Instant::now();
        for _ in 0..60 {
            let copied: Vec<_> = history
                .completed
                .iter()
                .map(|segment| segment.frames.iter().cloned().collect::<Vec<_>>())
                .chain(
                    history
                        .active
                        .as_ref()
                        .map(|active| active.view.frames.iter().cloned().collect()),
                )
                .collect();
            let frames: Vec<_> = copied
                .iter()
                .flatten()
                .map(|location| location.info.clone())
                .collect();
            std::hint::black_box((copied, frames));
        }
        let copied_us = start.elapsed().as_secs_f64() * 1e6 / 60.0;
        let start = Instant::now();
        for index in 1_000_000..1_000_600 {
            let snapshot = history.snapshot();
            let info = FrameInfo {
                index,
                timestamp: index as u64,
                scene_id: 1,
            };
            history.frame_infos.push(info.clone());
            history
                .active
                .as_mut()
                .unwrap()
                .view
                .frames
                .push(FrameLocation {
                    undo: None,
                    info,
                    offset: 0,
                    scene_generation: None,
                });
            std::hint::black_box(snapshot);
        }
        let publish_append_us = start.elapsed().as_secs_f64() * 1e6 / 600.0;
        eprintln!(
            "synthetic_index_profile frames=1000000 shared_snapshot_us={shared_us:.3} copied_index_snapshot_us={copied_us:.3} shared_publish_append_us={publish_append_us:.3} shared_60hz_us_per_second={:.3}",
            publish_append_us * 60.0
        );
        assert!(Arc::ptr_eq(
            &history.snapshot().segments,
            &history.completed
        ));
        drop(history);
        fs::remove_dir(directory).unwrap();
    }
}
