//! Dynamic PVD object history and deterministic checkpoint replay.
pub mod analysis;
pub mod analysis_export;
pub mod capture;
mod capture_equality;
pub mod clip;
pub mod cloth;
pub mod contacts;
pub mod debug_draw;
pub mod frame_metrics;
mod history;
pub mod joint_geometry;
pub mod live;
mod mesh;
pub use mesh::MeshCache;
pub mod metrics;
pub mod native;
mod origin_shift;
pub mod particles;
pub mod profile_curves;
pub mod profile_disk_index;
pub use profile_disk_index::catalog as profile_catalog_index;
pub mod profile_timeline;
#[cfg(test)]
mod property_message_tests;
pub mod queries;
pub mod repx;
use metrics::{Metrics, Stage};
use pvd_protocol::{Decoder, Endian, Event, EventGroup, Header, MessageArg, Name};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt, fs,
    io::Read,
    path::Path,
    sync::Arc,
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type FrameIndex = history::SharedIndex<FrameInfo>;
fn error(text: impl Into<String>) -> Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, text.into()).into()
}

fn check_replay_cancelled(cancelled: &mut impl FnMut() -> bool) -> Result<(), Error> {
    if cancelled() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "recording replay cancelled",
        )
        .into());
    }
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Value {
    Bool(bool),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    String(String),
    Vec3([f32; 3]),
    Quat([f32; 4]),
    Transform {
        position: [f32; 3],
        rotation: [f32; 4],
    },
    Ref(u64),
    Array(Arc<Vec<Value>>),
    Bytes(Arc<[u8]>),
}
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(v) => write!(f, "{v}"),
            Self::I64(v) => write!(f, "{v}"),
            Self::U64(v) => write!(f, "{v}"),
            Self::F32(v) => write!(f, "{v:.6}"),
            Self::F64(v) => write!(f, "{v:.6}"),
            Self::String(v) => write!(f, "{v}"),
            Self::Vec3(v) => write!(f, "[{:.4}, {:.4}, {:.4}]", v[0], v[1], v[2]),
            Self::Quat(v) => write!(f, "[{:.4}, {:.4}, {:.4}, {:.4}]", v[0], v[1], v[2], v[3]),
            Self::Transform { position, rotation } => {
                write!(f, "p {position:.4?} · q {rotation:.4?}")
            }
            Self::Ref(v) => write!(f, "0x{v:X}"),
            Self::Array(v) => {
                write!(f, "[{} items]", v.len())?;
                for item in v.iter().take(4) {
                    write!(f, " {item}")?;
                }
                Ok(())
            }
            Self::Bytes(v) => write!(f, "{} opaque bytes", v.len()),
        }
    }
}
impl Value {
    fn reference_count(&self) -> usize {
        match self {
            Self::Ref(_) => 1,
            Self::Array(values) => values.iter().map(Self::reference_count).sum(),
            _ => 0,
        }
    }
    fn collect_references(&self, targets: &mut Vec<u64>) {
        match self {
            Self::Ref(id) => targets.push(*id),
            Self::Array(values) => values
                .iter()
                .for_each(|value| value.collect_references(targets)),
            _ => {}
        }
    }
    fn retained_bytes(&self) -> usize {
        match self {
            Self::String(v) => v.len(),
            Self::Bytes(v) => v.len(),
            Self::Array(values) => {
                values.capacity() * std::mem::size_of::<Value>()
                    + values.iter().map(Self::retained_bytes).sum::<usize>()
            }
            _ => 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Object {
    pub id: u64,
    pub generation: u32,
    pub type_name: String,
    pub properties: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ErrorEvent {
    pub code: u32,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub timestamp: u64,
}
/// Saved database errors carry receive order but no timestamp or affected Actor ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntimedErrorEvent {
    pub code: u32,
    pub message: String,
    pub file: String,
    pub line: u32,
    pub index: u32,
    pub history_event: u64,
    pub received_frame: usize,
    pub container_instance: u64,
    pub container_generation: u32,
}
/// SDK camera vectors use capture coordinates; the event carries no projection parameters.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct SourceCamera {
    pub position: [f32; 3],
    pub up: [f32; 3],
    pub target: [f32; 3],
}

impl PartialEq for SourceCamera {
    fn eq(&self, other: &Self) -> bool {
        // Invalid vectors remain captured data even when a viewer cannot use their orientation.
        self.position
            .iter()
            .chain(&self.up)
            .chain(&self.target)
            .zip(other.position.iter().chain(&other.up).chain(&other.target))
            .all(|(left, right)| left.to_bits() == right.to_bits())
    }
}

pub(crate) fn camera_entry_bytes(name: &str) -> usize {
    name.len() + std::mem::size_of::<SourceCamera>() + 128
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneState {
    pub objects: BTreeMap<u64, Object>,
    pub cameras: BTreeMap<String, SourceCamera>,
    pub errors: Vec<ErrorEvent>,
    pub debug_draw: BTreeMap<u64, Arc<Vec<debug_draw::Primitive>>>,
    pub contacts: BTreeMap<u64, Arc<[contacts::Contact]>>,
    reference_lifetimes: BTreeMap<(u64, String), Arc<BTreeMap<u64, u32>>>,
    generations: BTreeMap<u64, u32>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct FrameInfo {
    pub index: usize,
    pub timestamp: u64,
    pub scene_id: u64,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Geometry {
    Box {
        half_extents: [f32; 3],
    },
    Sphere {
        radius: f32,
    },
    Capsule {
        radius: f32,
        half_height: f32,
    },
    Plane,
    TriangleMesh {
        vertices: Arc<Vec<[f32; 3]>>,
        indices: Arc<Vec<u32>>,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub struct RenderObject {
    pub id: u64,
    pub shape_id: u64,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub geometry: Geometry,
    pub dynamic: bool,
    pub sleeping: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RenderScene {
    pub objects: Vec<RenderObject>,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PropertyDefinition {
    pub datatype: String,
    pub semantic: String,
    pub array: bool,
    pub named_values: Vec<(String, u32)>,
}
#[derive(Clone, Debug, Default)]
pub struct ClassDefinition {
    pub parent: Option<String>,
    pub properties: BTreeMap<String, PropertyDefinition>,
}

#[derive(Clone, Debug)]
enum Action {
    Create(Object),
    Destroy(u64),
    Set(u64, String, Value),
    Push(u64, String, u64),
    Remove(u64, String, u64),
    Error(ErrorEvent),
    Debug(u64, Arc<[debug_draw::Primitive]>),
    ClearDebug(Vec<u64>),
    Contacts(u64, Arc<[contacts::Contact]>),
    Camera(String, SourceCamera),
}
impl Action {
    fn set_is_redundant(state: &SceneState, id: u64, key: &str, value: &Value) -> bool {
        if state
            .objects
            .get(&id)
            .and_then(|object| object.properties.get(key))
            != Some(value)
        {
            return false;
        }
        let bindings = state
            .reference_lifetimes
            .range((id, String::new())..)
            .take_while(|((owner, name), _)| *owner == id && name.as_str() <= key)
            .find_map(|((_, name), bindings)| (name == key).then_some(bindings));
        // A reference push can leave bindings on a scalar property, so equality
        // alone cannot establish that assigning the scalar has no effect.
        let reference_count = value.reference_count();
        if reference_count == 0 {
            return bindings.is_none_or(|bindings| bindings.is_empty());
        }
        if let Value::Ref(target) = value {
            return bindings.is_some_and(|bindings| {
                bindings.len() == 1
                    && bindings.get(target) == Some(&state.expected_generation(*target))
            });
        }
        let mut targets = Vec::with_capacity(reference_count);
        value.collect_references(&mut targets);
        let expected = targets
            .into_iter()
            .map(|target| (target, state.expected_generation(target)))
            .collect::<BTreeMap<_, _>>();
        bindings.map(AsRef::as_ref) == Some(&expected)
    }
    fn is_redundant(&self, state: &SceneState) -> bool {
        match self {
            Self::Set(id, key, value) => Self::set_is_redundant(state, *id, key, value),
            Self::Push(id, key, target) => {
                state
                    .reference_lifetimes
                    .get(&(*id, key.clone()))
                    .and_then(|bindings| bindings.get(target))
                    == Some(&state.expected_generation(*target))
            }
            Self::Contacts(id, contacts) => state.contacts.get(id) == Some(contacts),
            Self::Camera(name, camera) => state.cameras.get(name) == Some(camera),
            Self::ClearDebug(ids) => ids.iter().all(|id| !state.debug_draw.contains_key(id)),
            _ => false,
        }
    }
    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + match self {
                Self::Create(o) => o.type_name.len() * 3 + 256,
                Self::Set(_, key, value) => {
                    key.len() + value.retained_bytes() + value.reference_count() * 80
                }
                Self::Push(_, key, _) => key.len() + 192,
                Self::Remove(_, key, _) => key.len() + 64,
                Self::Error(e) => {
                    e.message.len() + e.file.len() + std::mem::size_of::<ErrorEvent>()
                }
                Self::Debug(_, values) => {
                    values.len() * std::mem::size_of::<debug_draw::Primitive>() * 3
                        + values
                            .iter()
                            .map(|v| {
                                if let debug_draw::Primitive::Text { text, .. } = v {
                                    text.len() * 2
                                } else {
                                    0
                                }
                            })
                            .sum::<usize>()
                }
                Self::Contacts(_, values) => std::mem::size_of_val(values.as_ref()),
                Self::ClearDebug(ids) => std::mem::size_of_val(ids.as_slice()),
                Self::Destroy(_) => 0,
                Self::Camera(name, _) => camera_entry_bytes(name) + name.len(),
            }
    }
    fn apply(&self, state: &mut SceneState) {
        match self {
            Self::Create(o) => {
                state.generations.insert(o.id, o.generation);
                state.objects.insert(o.id, o.clone());
            }
            Self::Destroy(id) => {
                state.objects.remove(id);
                state.contacts.remove(id);
                state.debug_draw.remove(id);
                let owned: Vec<_> = state
                    .reference_lifetimes
                    .range((*id, String::new())..)
                    .take_while(|((owner, _), _)| owner == id)
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in owned {
                    state.reference_lifetimes.remove(&key);
                }
            }
            Self::Set(id, key, value) => {
                let mut references = Vec::with_capacity(value.reference_count());
                value.collect_references(&mut references);
                let targets = references
                    .into_iter()
                    .map(|target| (target, state.expected_generation(target)))
                    .collect::<BTreeMap<_, _>>();
                if targets.is_empty() {
                    state.reference_lifetimes.remove(&(*id, key.clone()));
                } else {
                    state
                        .reference_lifetimes
                        .insert((*id, key.clone()), Arc::new(targets));
                }
                if let Some(o) = state.objects.get_mut(id) {
                    o.properties.insert(key.clone(), value.clone());
                }
            }
            Self::Push(id, key, target) => {
                let generation = state.expected_generation(*target);
                let targets = state
                    .reference_lifetimes
                    .entry((*id, key.clone()))
                    .or_default();
                let inserted = Arc::make_mut(targets).insert(*target, generation).is_none();
                if inserted && let Some(o) = state.objects.get_mut(id) {
                    let v = o
                        .properties
                        .entry(key.clone())
                        .or_insert_with(|| Value::Array(Arc::new(Vec::new())));
                    if let Value::Array(a) = v {
                        Arc::make_mut(a).push(Value::Ref(*target));
                    }
                }
            }
            Self::Remove(id, key, target) => {
                if let Some(targets) = state.reference_lifetimes.get_mut(&(*id, key.clone())) {
                    Arc::make_mut(targets).remove(target);
                }
                if let Some(o) = state.objects.get_mut(id)
                    && let Some(Value::Array(a)) = o.properties.get_mut(key)
                {
                    Arc::make_mut(a).retain(|v| *v != Value::Ref(*target));
                }
            }
            Self::Error(e) => state.errors.push(e.clone()),
            Self::Debug(id, primitives) => {
                let batch = state.debug_draw.entry(*id).or_default();
                Arc::make_mut(batch).extend(primitives.iter().cloned());
            }
            Self::ClearDebug(ids) => {
                for id in ids {
                    state.debug_draw.remove(id);
                }
            }
            Self::Contacts(id, contacts) => {
                state.contacts.insert(*id, contacts.clone());
            }
            Self::Camera(name, camera) => {
                state.cameras.insert(name.clone(), *camera);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingSource {
    SdkStream,
    Pvd3Database,
    ProjectCapture,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryMemoryUsage {
    pub estimated_history_bytes: usize,
    pub estimated_checkpoint_bytes: usize,
    pub limit_bytes: usize,
    pub checkpoint_count: usize,
    pub retained_actions: usize,
    pub skipped_redundant_actions: usize,
}

#[derive(Clone, Debug)]
pub struct Recording {
    pub source: RecordingSource,
    pub header: Header,
    pub analysis: analysis::SharedAnalysis,
    pub frames: FrameIndex,
    pub classes: BTreeMap<String, ClassDefinition>,
    pub diagnostics: Vec<String>,
    native_scene_contexts: Option<Arc<native::SceneContexts>>,
    untimed_errors: Vec<UntimedErrorEvent>,
    actions: history::ActionLog,
    frame_ends: Vec<usize>,
    checkpoints: BTreeMap<usize, Arc<history::Checkpoint>>,
    initial: Arc<SceneState>,
    memory_usage: HistoryMemoryUsage,
    disk: Option<live::DiskRecording>,
    stream_errors: Vec<(usize, ErrorEvent)>,
    streamed_frame_end: Option<usize>,
}
impl Recording {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if capture::Format::from_magic(bytes)?.is_some() {
            return Err(error(
                "project captures require a file path for bounded random access",
            ));
        }
        if bytes.len() > 512 << 20 {
            return Err(error(
                "recording exceeds the current 512 MiB indexing limit",
            ));
        }
        if bytes.starts_with(&native::MAGIC.to_le_bytes()) {
            return native::from_bytes_with_progress(bytes, |_, _| true);
        }
        let mut live = LiveRecording::default();
        for chunk in bytes.chunks(64 * 1024) {
            live.feed(chunk)?;
        }
        live.finish()
    }
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_path_with_progress(path, |_, _| true)
    }
    pub fn from_path_with_progress(
        path: impl AsRef<Path>,
        mut progress: impl FnMut(u64, u64) -> bool,
    ) -> Result<Self, Error> {
        let path = path.as_ref();
        let mut file = fs::File::open(path)?;
        let total = file.metadata()?.len();
        let mut prefix = [0; 8];
        if total >= 8 {
            file.read_exact(&mut prefix)?;
            use std::io::Seek;
            file.rewind()?;
            if capture::Format::from_magic(&prefix)?.is_some() {
                drop(file);
                return capture::open_with_progress(
                    path,
                    &capture::CaptureLimits::default(),
                    |state| progress(state.completed, state.total),
                );
            }
        }
        if total > 512 << 20 {
            return Err(error(
                "recording exceeds the current 512 MiB indexing limit",
            ));
        }
        let mut live = LiveRecording::default();
        let mut bytes = [0u8; 64 * 1024];
        let mut loaded = 0;
        let mut native_bytes = None;
        loop {
            if !progress(loaded, total) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "recording load cancelled",
                )
                .into());
            }
            let n = file.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            if loaded + n as u64 > 512 << 20 {
                return Err(error(
                    "recording grew beyond the current 512 MiB indexing limit",
                ));
            }
            if loaded == 0 && bytes[..n].starts_with(&native::MAGIC.to_le_bytes()) {
                native_bytes = Some(Vec::new());
            }
            if let Some(native) = &mut native_bytes {
                native.extend_from_slice(&bytes[..n]);
            } else {
                live.feed(&bytes[..n])?;
            }
            loaded += n as u64;
        }
        if let Some(bytes) = native_bytes {
            return native::from_bytes_with_progress(&bytes, |_, _| progress(total, total));
        }
        live.finish()
    }
    pub fn frame_count(&self) -> usize {
        self.streamed_frame_end
            .unwrap_or_else(|| self.frames.last().map_or(0, |frame| frame.index + 1))
    }
    pub fn capture_format(&self) -> Option<capture::Format> {
        self.disk
            .as_ref()
            .and_then(live::DiskRecording::capture_format)
    }

    pub fn first_frame(&self) -> usize {
        self.frames.first().map_or(0, |frame| frame.index)
    }
    pub fn frame_info(&self, index: usize) -> Option<&FrameInfo> {
        self.frames
            .binary_search_by_key(&index, |frame| frame.index)
            .ok()
            .map(|position| &self.frames[position])
    }
    pub fn is_frame_retained(&self, index: usize) -> bool {
        self.frame_info(index).is_some()
            && self
                .disk
                .as_ref()
                .is_none_or(|disk| disk.is_frame_retained(index))
    }
    pub fn is_disk_backed(&self) -> bool {
        self.disk.is_some()
    }
    fn disk_scene_generation(&self, frame: usize) -> Option<u32> {
        self.disk
            .as_ref()
            .and_then(|disk| disk.scene_generation(frame))
    }
    pub fn disk_usage(&self) -> Option<live::HistoryStats> {
        self.disk.as_ref().map(|disk| disk.stats.clone())
    }
    pub fn analysis_at(&self, index: usize) -> Result<analysis::SharedAnalysis, Error> {
        self.analysis_at_with_cancel(index, || false)
    }
    pub fn analysis_at_with_cancel(
        &self,
        index: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<analysis::SharedAnalysis, Error> {
        check_replay_cancelled(&mut cancelled)?;
        match &self.disk {
            Some(disk) => disk.analysis_at_with_cancel(index, &mut cancelled),
            None => {
                let analysis = self.analysis.clone();
                check_replay_cancelled(&mut cancelled)?;
                Ok(analysis)
            }
        }
    }
    pub fn state_at(&self, index: usize) -> Result<SceneState, Error> {
        self.state_at_with_cancel(index, || false)
    }
    pub fn state_at_with_cancel(
        &self,
        index: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<SceneState, Error> {
        check_replay_cancelled(&mut cancelled)?;
        if let Some(disk) = &self.disk {
            return disk.state_at_with_cancel(index, &mut cancelled);
        }
        let end = *self.frame_ends.get(index).ok_or_else(|| {
            error(format!(
                "frame {index} out of range ({} frames)",
                self.frame_count()
            ))
        })?;
        let (start, mut state) =
            if let Some((&frame, state)) = self.checkpoints.range(..=index).next_back() {
                (self.frame_ends[frame], state.restore())
            } else {
                (self.frame_ends[0], self.initial.as_ref().clone())
            };
        for action in self.actions.get(start..end).unwrap() {
            check_replay_cancelled(&mut cancelled)?;
            action.apply(&mut state);
        }
        check_replay_cancelled(&mut cancelled)?;
        Ok(state)
    }
    pub fn initial_state(&self) -> &SceneState {
        &self.initial
    }
    pub fn event_count(&self) -> usize {
        self.actions.len()
    }
    pub fn history_memory_usage(&self) -> HistoryMemoryUsage {
        self.memory_usage
    }
    pub fn error_events(&self) -> impl Iterator<Item = (Option<usize>, &ErrorEvent)> {
        self.actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                let Action::Error(event) = action else {
                    return None;
                };
                let frame = self.frame_ends.partition_point(|end| *end <= index);
                Some(((frame < self.frame_count()).then_some(frame), event))
            })
            .chain(
                self.stream_errors
                    .iter()
                    .map(|(frame, event)| (Some(*frame), event)),
            )
    }
    pub fn untimed_error_events(&self) -> &[UntimedErrorEvent] {
        &self.untimed_errors
    }
    pub fn verify_replay(&self) -> Result<(), Error> {
        if let Some(disk) = &self.disk {
            return disk.verify_replay();
        }
        let mut state = SceneState::default();
        let mut start = 0;
        for (frame, &end) in self.frame_ends.iter().enumerate() {
            for a in self.actions.get(start..end).unwrap() {
                a.apply(&mut state);
            }
            if state != self.state_at(frame)? {
                return Err(error(format!(
                    "sequential and checkpoint state differ at frame {frame}"
                )));
            }
            start = end;
        }
        Ok(())
    }
    /// Returns elapsed seconds relative to the first visible frame.
    /// Retained captures keep SDK timestamps and frame indices, but each visible
    /// interval has its own elapsed-time origin.
    pub fn seconds_at(&self, index: usize) -> Option<f64> {
        let frame = self.frame_info(index)?;
        let first = self.frames.first()?;
        Some(
            frame.timestamp.saturating_sub(first.timestamp) as f64
                * self.header.timestamp_numerator as f64
                / self.header.timestamp_denominator as f64
                / 1_000_000_000.0,
        )
    }
}

#[derive(Clone)]
struct PartialProperty {
    id: u64,
    property: u32,
    datatype: Name,
    data: Vec<u8>,
    items: u32,
}
#[derive(Clone)]
struct Message {
    args: Arc<[MessageArg]>,
    size: usize,
    compiled: Option<Arc<CompiledMessage>>,
}
struct CompiledMessage {
    class: String,
    args: Box<[CompiledMessageArg]>,
    bytes: usize,
}
struct CompiledMessageArg {
    key: String,
    datatype: String,
    start: usize,
    end: usize,
    array: bool,
}
const MESSAGE_CACHE_LIMIT: usize = 8 << 20;
pub struct RecordingBuilder {
    recording: Recording,
    strings: BTreeMap<u32, String>,
    messages: BTreeMap<Name, Message>,
    groups: BTreeMap<u64, Name>,
    partial: BTreeMap<u64, PartialProperty>,
    partial_bytes: usize,
    state: SceneState,
    generations: BTreeMap<u64, u32>,
    object_classes: BTreeMap<u64, String>,
    render_decoders: BTreeMap<u64, debug_draw::RenderDecoder>,
    retained_bytes: usize,
    checkpoint_bytes: usize,
    skipped_redundant_actions: usize,
    memory_limit: usize,
    frame_scenes: BTreeSet<u64>,
    streaming: Option<Box<live::Streaming>>,
    validation: Option<clip::ValidationState>,
    metrics: Metrics,
    message_cache_bytes: usize,
}
impl RecordingBuilder {
    pub fn new(header: Header) -> Self {
        Self {
            recording: Recording {
                source: RecordingSource::SdkStream,
                header,
                analysis: analysis::SharedAnalysis::default(),
                frames: FrameIndex::default(),
                classes: BTreeMap::new(),
                diagnostics: Vec::new(),
                native_scene_contexts: None,
                untimed_errors: Vec::new(),
                actions: history::ActionLog::default(),
                frame_ends: Vec::new(),
                checkpoints: BTreeMap::new(),
                initial: Arc::new(SceneState::default()),
                memory_usage: HistoryMemoryUsage::default(),
                disk: None,
                stream_errors: Vec::new(),
                streamed_frame_end: None,
            },
            strings: BTreeMap::new(),
            messages: BTreeMap::new(),
            groups: BTreeMap::new(),
            partial: BTreeMap::new(),
            partial_bytes: 0,
            state: SceneState::default(),
            generations: BTreeMap::new(),
            object_classes: BTreeMap::new(),
            render_decoders: BTreeMap::new(),
            retained_bytes: 0,
            checkpoint_bytes: 0,
            skipped_redundant_actions: 0,
            memory_limit: 512 << 20,
            frame_scenes: BTreeSet::new(),
            streaming: None,
            validation: None,
            metrics: Metrics::default(),
            message_cache_bytes: 0,
        }
    }
    fn set_metrics(&mut self, metrics: Metrics) {
        if let Some(live) = &mut self.streaming {
            live.history.set_metrics(metrics.clone());
        }
        self.metrics = metrics;
    }
    pub fn with_memory_limit(header: Header, bytes: usize) -> Self {
        let mut builder = Self::new(header);
        builder.memory_limit = bytes;
        builder
    }
    fn reserve_history(&mut self, bytes: usize) -> Result<(), Error> {
        if self.retained_bytes.saturating_add(bytes).saturating_add(
            self.validation
                .as_ref()
                .map_or(0, |state| state.state_bytes),
        ) > self.memory_limit
        {
            self.clear_message_cache();
        }
        let required = self
            .retained_bytes
            .checked_add(bytes)
            .ok_or_else(|| error("history memory accounting overflow"))?;
        if required
            > self.memory_limit.saturating_sub(
                self.validation
                    .as_ref()
                    .map_or(0, |state| state.state_bytes),
            )
        {
            return Err(error(format!(
                "capture exceeds the configured {} MiB history memory budget",
                self.memory_limit >> 20
            )));
        }
        while self.checkpoint_bytes > self.memory_limit - required {
            history::evict_checkpoint(&mut self.recording.checkpoints);
            self.checkpoint_bytes = history::checkpoint_bytes(&self.recording.checkpoints);
        }
        self.retained_bytes = required;
        Ok(())
    }
    fn clear_message_cache(&mut self) {
        if self.message_cache_bytes != 0 {
            for message in self.messages.values_mut() {
                message.compiled = None;
            }
            self.retained_bytes -= self.message_cache_bytes;
            self.message_cache_bytes = 0;
        }
    }
    fn release_message_cache_for_action(&mut self, action: &Action) {
        if self.message_cache_bytes == 0 {
            return;
        }
        let state_bytes = match (&self.streaming, &self.validation) {
            (Some(live), _) => live.state_bytes,
            (_, Some(validation)) => validation.state_bytes,
            _ => return,
        };
        // Optional metadata must not reserve space needed by state. Compound values
        // yield the cache without a second resource scan or a speculative mutation.
        let growth = match action {
            Action::Set(_, _, Value::Array(_)) | Action::Debug(..) | Action::Push(..) => None,
            Action::Create(object) if !object.properties.is_empty() => None,
            _ => Some(action.retained_bytes().saturating_add(256)),
        };
        if growth.is_none_or(|bytes| {
            self.retained_bytes
                .saturating_add(state_bytes)
                .saturating_add(bytes)
                > self.memory_limit
        }) {
            self.clear_message_cache();
        }
    }
    fn memory_usage(&self) -> HistoryMemoryUsage {
        HistoryMemoryUsage {
            estimated_history_bytes: self.retained_bytes
                + self.streaming.as_ref().map_or(0, |live| live.state_bytes),
            estimated_checkpoint_bytes: self.checkpoint_bytes
                + self
                    .streaming
                    .as_ref()
                    .map_or(0, |live| live.history.cache_bytes()),
            limit_bytes: self.memory_limit,
            checkpoint_count: self.recording.checkpoints.len(),
            retained_actions: self.recording.actions.len(),
            skipped_redundant_actions: self.skipped_redundant_actions,
        }
    }
    fn checkpoint(&mut self, index: usize) {
        let budget = (self.memory_limit / 4)
            .max(history::state_structure_bytes(&self.state).saturating_mul(2))
            .min(self.memory_limit.saturating_sub(self.retained_bytes));
        // Cache eviction changes seek cost, never the retained capture or frame numbering.
        if budget == 0 {
            return;
        }
        let previous = self
            .recording
            .checkpoints
            .last_key_value()
            .map(|(_, state)| state.as_ref());
        let checkpoint = history::Checkpoint::new(&self.state, previous);
        self.recording
            .checkpoints
            .insert(index, Arc::new(checkpoint));
        self.checkpoint_bytes = history::checkpoint_bytes(&self.recording.checkpoints);
        while self.checkpoint_bytes > budget {
            history::evict_checkpoint(&mut self.recording.checkpoints);
            self.checkpoint_bytes = history::checkpoint_bytes(&self.recording.checkpoints);
        }
    }
    fn string(&self, handle: u32) -> Result<String, Error> {
        if handle == 0 {
            return Ok(String::new());
        }
        self.strings
            .get(&handle)
            .cloned()
            .ok_or_else(|| error(format!("undefined string handle {handle}")))
    }
    fn name(&self, name: Name) -> Result<String, Error> {
        let ns = self.string(name.namespace)?;
        let n = self.string(name.name)?;
        Ok(if ns.is_empty() {
            n
        } else {
            format!("{ns}::{n}")
        })
    }
    fn action(&mut self, action: Action) -> Result<(), Error> {
        let _timer = self.metrics.measure(Stage::Action);
        if action.is_redundant(&self.state) {
            self.skipped_redundant_actions += 1;
            return Ok(());
        }
        self.apply_action(action)
    }
    fn set_value(&mut self, id: u64, key: &str, value: Value) -> Result<(), Error> {
        let _timer = self.metrics.measure(Stage::Action);
        if Action::set_is_redundant(&self.state, id, key, &value) {
            self.skipped_redundant_actions += 1;
            return Ok(());
        }
        self.apply_action(Action::Set(id, key.to_owned(), value))
    }
    fn apply_action(&mut self, action: Action) -> Result<(), Error> {
        self.release_message_cache_for_action(&action);
        if let Some(validation) = &mut self.validation {
            return validation.apply(
                action,
                &mut self.state,
                self.memory_limit.saturating_sub(self.retained_bytes),
            );
        }
        if let Some(streaming) = &mut self.streaming {
            return streaming.action(
                action,
                &mut self.state,
                self.memory_limit.saturating_sub(self.retained_bytes),
            );
        }
        if self.recording.actions.len() >= 20_000_000 {
            return Err(error("recording exceeds 20 million state changes"));
        }
        let copy_bytes = match &action {
            Action::Push(id, key, _) | Action::Remove(id, key, _) => self
                .state
                .objects
                .get(id)
                .and_then(|o| o.properties.get(key))
                .map_or(0, |value| match value {
                    Value::Array(values) if Arc::strong_count(values) > 1 => value.retained_bytes(),
                    _ => 0,
                }),
            Action::Debug(id, _) => self
                .state
                .debug_draw
                .get(id)
                .filter(|batch| Arc::strong_count(batch) > 1)
                .map_or(0, |batch| {
                    batch.capacity() * std::mem::size_of::<debug_draw::Primitive>()
                        + batch
                            .iter()
                            .map(|primitive| match primitive {
                                debug_draw::Primitive::Text { text, .. } => text.len(),
                                _ => 0,
                            })
                            .sum::<usize>()
                }),
            _ => 0,
        };
        let reference_copy_bytes = match &action {
            Action::Push(id, key, _) | Action::Remove(id, key, _) => self
                .state
                .reference_lifetimes
                .get(&(*id, key.clone()))
                .filter(|targets| Arc::strong_count(targets) > 1)
                .map(|targets| {
                    targets
                        .len()
                        .checked_mul(80)
                        .and_then(|bytes| bytes.checked_add(32))
                        .ok_or_else(|| error("reference copy accounting overflow"))
                })
                .transpose()?
                .unwrap_or(0),
            _ => 0,
        };
        let retained_bytes = action
            .retained_bytes()
            .checked_add(copy_bytes)
            .and_then(|bytes| bytes.checked_add(reference_copy_bytes))
            .ok_or_else(|| error("history memory accounting overflow"))?;
        self.reserve_history(retained_bytes)?;
        action.apply(&mut self.state);
        self.recording.actions.push(action);
        Ok(())
    }
    fn property(&self, id: u64, name: &str) -> Option<&PropertyDefinition> {
        let mut class = self.object_classes.get(&id)?.as_str();
        for _ in 0..128 {
            let c = self.recording.classes.get(class)?;
            if let Some(p) = c.properties.get(name) {
                return Some(p);
            }
            class = c.parent.as_deref()?;
        }
        None
    }
    fn set(
        &mut self,
        id: u64,
        property: u32,
        datatype: Name,
        items: u32,
        data: &[u8],
    ) -> Result<(), Error> {
        if !self.state.objects.contains_key(&id) {
            return Err(error(format!(
                "property update for missing instance 0x{id:X}"
            )));
        }
        let key = self.string(property)?;
        let typename = self.name(datatype)?;
        let array = key != "events"
            && self
                .property(id, &key)
                .map(|p| p.array)
                .unwrap_or(items != 1);
        self.set_resolved(id, &key, &typename, items, array, data)
    }
    fn set_resolved(
        &mut self,
        id: u64,
        key: &str,
        typename: &str,
        items: u32,
        array: bool,
        data: &[u8],
    ) -> Result<(), Error> {
        if key == "Contacts" && typename.ends_with("::PvdContact") {
            let contacts = contacts::decode(data, items, &self.recording.header)?;
            self.action(Action::Contacts(id, contacts.into()))?;
        }
        if key == "events" {
            let object_type = self.state.objects[&id].type_name.clone();
            if object_type.ends_with("::PvdUserRenderer") {
                let primitives = self
                    .render_decoders
                    .entry(id)
                    .or_default()
                    .decode(data, self.recording.header.endian)?;
                if !primitives.is_empty() {
                    self.action(Action::Debug(id, primitives.into()))?;
                }
                return self.set_value(id, key, Value::Bytes(Arc::from(data)));
            }
            if self
                .validation
                .as_ref()
                .is_some_and(|state| state.skip_analysis)
            {
                return self.set_value(id, key, Value::Bytes(Arc::from(data)));
            }
            let analysis_timer = self.metrics.measure(Stage::Analysis);
            let frame = self.validation.as_ref().map_or_else(
                || {
                    self.streaming
                        .as_ref()
                        .map_or(self.recording.frames.len(), |live| live.frames_seen)
                },
                |state| state.frames,
            );
            let data: Arc<[u8]> = Arc::from(data);
            if self
                .streaming
                .as_ref()
                .is_none_or(|live| live.analysis_failure.is_none())
            {
                if self.streaming.is_some() || self.validation.is_some() {
                    let _timer = self.metrics.measure(Stage::AnalysisMaintenance);
                    self.maintain_analysis(frame)?;
                }
                if self
                    .streaming
                    .as_ref()
                    .is_none_or(|live| live.analysis_failure.is_none())
                {
                    let _timer = self.metrics.measure(Stage::AnalysisIngest);
                    let input = live::AnalysisInput::Buffer {
                        id,
                        kind: object_type,
                        bytes: data.clone(),
                        little_endian: self.recording.header.endian == Endian::Little,
                        frame,
                    };
                    self.apply_analysis_input(input)?;
                }
            }
            drop(analysis_timer);
            return self.set_value(id, key, Value::Bytes(data));
        }
        let value = decode_value(
            typename,
            data,
            items,
            array,
            &self.recording.header,
            &self.strings,
        )?;
        self.set_value(id, key, value)
    }
    fn set_message(&mut self, id: u64, name: Name, data: &[u8]) -> Result<(), Error> {
        let _timer = self.metrics.measure(Stage::PropertyMessage);
        let object = self
            .state
            .objects
            .get(&id)
            .ok_or_else(|| error(format!("property update for missing instance 0x{id:X}")))?;
        let message = self
            .messages
            .get(&name)
            .ok_or_else(|| error("undefined property message"))?;
        if data.len() < message.size {
            return Err(error("property message shorter than declared size"));
        }
        let mut string_offset = message.size;
        let layout = match &message.compiled {
            Some(layout) if layout.class == object.type_name => Some(layout.clone()),
            _ => match self.compile_message(id, name) {
                Ok(layout) => layout,
                Err(_) => return self.set_message_uncached(id, name, data),
            },
        };
        let Some(layout) = layout else {
            return self.set_message_uncached(id, name, data);
        };
        for arg in &layout.args {
            let bytes = data
                .get(arg.start..arg.end)
                .ok_or_else(|| error("property exceeds message boundary"))?;
            if arg.datatype.ends_with("::String") {
                let tail = data
                    .get(string_offset..)
                    .ok_or_else(|| error("missing string message payload"))?;
                let n = tail
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or_else(|| error("unterminated message string"))?;
                self.set_value(
                    id,
                    &arg.key,
                    Value::String(String::from_utf8_lossy(&tail[..n]).into_owned()),
                )?;
                string_offset += n + 1;
            } else {
                self.set_resolved(id, &arg.key, &arg.datatype, 1, arg.array, bytes)?;
            }
        }
        Ok(())
    }
    fn compile_message(
        &mut self,
        id: u64,
        name: Name,
    ) -> Result<Option<Arc<CompiledMessage>>, Error> {
        let message = &self.messages[&name];
        let previous_bytes = message.compiled.as_ref().map_or(0, |layout| layout.bytes);
        let used = self.retained_bytes - previous_bytes
            + self.checkpoint_bytes
            + self.streaming.as_ref().map_or(0, |live| live.state_bytes)
            + self
                .validation
                .as_ref()
                .map_or(0, |state| state.state_bytes);
        let available = MESSAGE_CACHE_LIMIT
            .saturating_sub(self.message_cache_bytes - previous_bytes)
            .min(self.memory_limit.saturating_sub(used));
        let class = &self.state.objects[&id].type_name;
        let mut bytes = std::mem::size_of::<CompiledMessage>()
            + 32
            + class.len()
            + message.args.len() * std::mem::size_of::<CompiledMessageArg>();
        if bytes > available {
            return Ok(None);
        }
        let mut args = Vec::with_capacity(message.args.len());
        for arg in message.args.iter() {
            let key = self.string(arg.property)?;
            let datatype = self.name(arg.datatype)?;
            bytes += key.capacity() + datatype.capacity();
            if bytes > available {
                return Ok(None);
            }
            args.push(CompiledMessageArg {
                array: self
                    .property(id, &key)
                    .is_some_and(|property| property.array),
                key,
                datatype,
                start: arg.offset as usize,
                end: (arg.offset as usize)
                    .checked_add(arg.size as usize)
                    .ok_or_else(|| error("message offset overflow"))?,
            });
        }
        let layout = Arc::new(CompiledMessage {
            class: class.clone(),
            args: args.into_boxed_slice(),
            bytes,
        });
        self.messages.get_mut(&name).unwrap().compiled = Some(layout.clone());
        self.retained_bytes = self.retained_bytes - previous_bytes + bytes;
        self.message_cache_bytes = self.message_cache_bytes - previous_bytes + bytes;
        Ok(Some(layout))
    }
    fn set_message_uncached(&mut self, id: u64, name: Name, data: &[u8]) -> Result<(), Error> {
        let m = self
            .messages
            .get(&name)
            .ok_or_else(|| error("undefined property message"))?;
        if data.len() < m.size {
            return Err(error("property message shorter than declared size"));
        }
        let args = m.args.clone();
        let mut string_offset = m.size;
        for arg in args.iter() {
            let start = arg.offset as usize;
            let end = start
                .checked_add(arg.size as usize)
                .ok_or_else(|| error("message offset overflow"))?;
            let bytes = data
                .get(start..end)
                .ok_or_else(|| error("property exceeds message boundary"))?;
            let typename = self.name(arg.datatype)?;
            if typename.ends_with("::String") {
                let tail = data
                    .get(string_offset..)
                    .ok_or_else(|| error("missing string message payload"))?;
                let n = tail
                    .iter()
                    .position(|b| *b == 0)
                    .ok_or_else(|| error("unterminated message string"))?;
                let key = self.string(arg.property)?;
                self.set_value(
                    id,
                    &key,
                    Value::String(String::from_utf8_lossy(&tail[..n]).into_owned()),
                )?;
                string_offset += n + 1;
            } else {
                self.set(id, arg.property, arg.datatype, 1, bytes)?;
            }
        }
        Ok(())
    }
    pub fn push(&mut self, group: EventGroup) -> Result<(), Error> {
        let _timer = self.metrics.measure(Stage::BuilderPush);
        let stream = group.stream_id;
        for event in group.events {
            match event {
                Event::StringHandle { handle, value } => {
                    self.clear_message_cache();
                    if value.len() > 32 << 10 {
                        return Err(error("metadata string exceeds 32 KiB"));
                    }
                    if self.strings.len() > 1_000_000 {
                        return Err(error("too many string handles"));
                    }
                    if let Some(old) = self.strings.insert(handle, value.clone()) {
                        if old != value {
                            return Err(error("string handle redefined with another value"));
                        }
                    } else {
                        self.reserve_history(value.len() + 64)?;
                    }
                }
                Event::CreateClass { name } => {
                    self.clear_message_cache();
                    let n = self.name(name)?;
                    self.reserve_history(n.len() + 128)?;
                    self.recording.classes.entry(n).or_default();
                }
                Event::DeriveClass { parent, child } => {
                    self.clear_message_cache();
                    let p = self.name(parent)?;
                    let c = self.name(child)?;
                    let mut ancestor = Some(p.as_str());
                    let mut visited = BTreeSet::new();
                    while let Some(name) = ancestor {
                        if name == c || !visited.insert(name) {
                            return Err(error("class inheritance contains a cycle"));
                        }
                        if visited.len() >= 128 {
                            return Err(error("class inheritance exceeds 127 ancestors"));
                        }
                        ancestor = self
                            .recording
                            .classes
                            .get(name)
                            .and_then(|definition| definition.parent.as_deref());
                    }
                    self.reserve_history(p.len() + c.len() + 128)?;
                    self.recording.classes.entry(c).or_default().parent = Some(p);
                }
                Event::CreateProperty {
                    class,
                    name,
                    semantic,
                    datatype,
                    kind,
                    named_values,
                } => {
                    self.clear_message_cache();
                    if kind != 1 && kind != 2 {
                        return Err(error(format!("unknown property kind {kind}")));
                    }
                    let c = self.name(class)?;
                    let n = self.string(name)?;
                    let datatype = self.name(datatype)?;
                    let semantic = self.string(semantic)?;
                    let mut named_bytes = 0usize;
                    for (handle, _) in &named_values {
                        let length = if *handle == 0 {
                            0
                        } else {
                            self.strings
                                .get(handle)
                                .ok_or_else(|| error(format!("undefined string handle {handle}")))?
                                .len()
                        };
                        named_bytes = named_bytes
                            .checked_add(length + 32)
                            .ok_or_else(|| error("named property metadata size overflow"))?;
                    }
                    self.reserve_history(
                        c.len() + n.len() + datatype.len() + semantic.len() + named_bytes + 192,
                    )?;
                    let named_values = named_values
                        .into_iter()
                        .map(|(k, v)| Ok((self.string(k)?, v)))
                        .collect::<Result<Vec<_>, Error>>()?;
                    self.recording
                        .classes
                        .entry(c)
                        .or_default()
                        .properties
                        .insert(
                            n,
                            PropertyDefinition {
                                datatype,
                                semantic,
                                array: kind == 2,
                                named_values,
                            },
                        );
                }
                Event::CreatePropertyMessage {
                    class: _,
                    name,
                    args,
                    size,
                } => {
                    self.clear_message_cache();
                    self.reserve_history(args.len() * std::mem::size_of::<MessageArg>() + 128)?;
                    for a in &args {
                        if u64::from(a.offset) + u64::from(a.size) > u64::from(size) {
                            return Err(error("message property exceeds declared message size"));
                        }
                    }
                    self.messages.insert(
                        name,
                        Message {
                            args: args.into(),
                            size: size as usize,
                            compiled: None,
                        },
                    );
                }
                Event::CreateInstance { class, id } => {
                    if self.state.objects.contains_key(&id) {
                        return Err(error(format!(
                            "instance 0x{id:X} created while still alive"
                        )));
                    }
                    if self.state.objects.len() >= 1_000_000 {
                        return Err(error("too many live instances"));
                    }
                    let type_name = self.name(class)?;
                    self.object_classes.insert(id, type_name.clone());
                    let generation = *self
                        .generations
                        .entry(id)
                        .and_modify(|v| *v += 1)
                        .or_insert(0);
                    self.action(Action::Create(Object {
                        id,
                        generation,
                        type_name,
                        properties: BTreeMap::new(),
                    }))?;
                }
                Event::DestroyInstance { id } => {
                    self.action(Action::Destroy(id))?;
                    self.object_classes.remove(&id);
                    self.render_decoders.remove(&id);
                }
                Event::SetPropertyValue {
                    id,
                    property,
                    datatype,
                    items,
                    data,
                } => self.set(id, property, datatype, items, &data)?,
                Event::BeginSetPropertyValue {
                    id,
                    property,
                    datatype,
                } => {
                    if self.partial.len() >= 1024 {
                        return Err(error(
                            "capture exceeds 1024 concurrent property transactions",
                        ));
                    }
                    if self
                        .partial
                        .insert(
                            stream,
                            PartialProperty {
                                id,
                                property,
                                datatype,
                                data: Vec::new(),
                                items: 0,
                            },
                        )
                        .is_some()
                    {
                        return Err(error("nested property array update"));
                    }
                }
                Event::AppendPropertyValueData { data, items } => {
                    if data.len() > (64usize << 20).saturating_sub(self.partial_bytes) {
                        return Err(error(
                            "pending property data exceeds the aggregate 64 MiB limit",
                        ));
                    }
                    let p = self
                        .partial
                        .get_mut(&stream)
                        .ok_or_else(|| error("array append without begin"))?;
                    if p.data.len() + data.len() > 64 << 20 {
                        return Err(error("chunked property exceeds 64 MiB"));
                    }
                    p.data.extend_from_slice(&data);
                    self.partial_bytes += data.len();
                    p.items = p
                        .items
                        .checked_add(items)
                        .ok_or_else(|| error("array item overflow"))?;
                }
                Event::EndSetPropertyValue => {
                    let p = self
                        .partial
                        .remove(&stream)
                        .ok_or_else(|| error("array end without begin"))?;
                    self.partial_bytes -= p.data.len();
                    self.set(p.id, p.property, p.datatype, p.items, &p.data)?;
                }
                Event::SetPropertyMessage { id, message, data } => {
                    self.set_message(id, message, &data)?
                }
                Event::BeginPropertyMessageGroup { message } => {
                    if self.groups.len() >= 1024 {
                        return Err(error(
                            "capture exceeds 1024 concurrent property message groups",
                        ));
                    }
                    if self.groups.insert(stream, message).is_some() {
                        return Err(error("nested property message group"));
                    }
                }
                Event::SendPropertyMessageFromGroup { id, data } => {
                    let m = *self
                        .groups
                        .get(&stream)
                        .ok_or_else(|| error("property message without group"))?;
                    self.set_message(id, m, &data)?;
                }
                Event::EndPropertyMessageGroup => {
                    if self.groups.remove(&stream).is_none() {
                        return Err(error("property message group end without begin"));
                    }
                }
                Event::PushBackObjectRef {
                    id,
                    property,
                    target,
                } => {
                    let key = self.string(property)?;
                    self.action(Action::Push(id, key, target))?;
                }
                Event::RemoveObjectRef {
                    id,
                    property,
                    target,
                } => {
                    let key = self.string(property)?;
                    self.action(Action::Remove(id, key, target))?;
                }
                Event::EndSection {
                    id,
                    name,
                    timestamp,
                } => {
                    if self.string(name)? == "frame" {
                        if let Some(validation) = &mut self.validation {
                            validation.frames = validation
                                .frames
                                .checked_add(1)
                                .ok_or_else(|| error("scene frame count overflow"))?;
                            self.recording
                                .analysis
                                .maintain_live_history(validation.frames)?;
                            let renderer_ids = self.state.debug_draw.keys().copied().collect();
                            self.action(Action::ClearDebug(renderer_ids))?;
                            continue;
                        }
                        if self.frame_scenes.insert(id) && self.frame_scenes.len() == 2 {
                            self.recording.diagnostics.push("Multiple scenes use a merged stream-order timeline; debug draws reset at each scene frame boundary. Independent asynchronous scene alignment is not yet validated.".into());
                        }
                        if let Some(live) = &mut self.streaming {
                            let frame = live.frames_seen;
                            if frame.is_multiple_of(60) {
                                let _timer = self.metrics.measure(Stage::Analysis);
                                let _maintenance = self.metrics.measure(Stage::AnalysisMaintenance);
                                self.maintain_analysis(frame)?;
                            }
                            let live = self.streaming.as_mut().unwrap();
                            live.complete(timestamp, id, &self.state, &self.recording.analysis);
                            let renderer_ids = self.state.debug_draw.keys().copied().collect();
                            self.action(Action::ClearDebug(renderer_ids))?;
                            continue;
                        }
                        let index = self.recording.frames.len();
                        self.reserve_history(std::mem::size_of::<FrameInfo>() + 16)?;
                        if index == 0 {
                            self.reserve_history(history::state_structure_bytes(&self.state))?;
                            self.recording.initial = Arc::new(self.state.clone());
                        }
                        self.recording.frames.push(FrameInfo {
                            index,
                            timestamp,
                            scene_id: id,
                        });
                        self.recording.frame_ends.push(self.recording.actions.len());
                        if index != 0 && index.is_multiple_of(60) {
                            self.checkpoint(index);
                        }
                        let renderer_ids = self.state.debug_draw.keys().copied().collect();
                        self.action(Action::ClearDebug(renderer_ids))?;
                    }
                }
                Event::ErrorMessage {
                    code,
                    message,
                    file,
                    line,
                } => self.action(Action::Error(ErrorEvent {
                    code,
                    message,
                    file,
                    line,
                    timestamp: group.timestamp,
                }))?,
                Event::SetPickable { id, value } => {
                    self.action(Action::Set(id, "$pickable".into(), Value::Bool(value)))?
                }
                Event::SetIsTopLevel { id, value } => {
                    self.action(Action::Set(id, "$top_level".into(), Value::Bool(value)))?
                }
                Event::SetColor { id, rgba } => self.action(Action::Set(
                    id,
                    "$color".into(),
                    Value::Bytes(Arc::from(rgba)),
                ))?,
                Event::OriginShift { id, shift } => {
                    self.shift_origin(id, shift, &mut || true)?;
                }
                Event::AddProfileZone { id, name } => {
                    if self
                        .validation
                        .as_ref()
                        .is_some_and(|state| state.skip_analysis)
                    {
                        continue;
                    }
                    let _timer = self.metrics.measure(Stage::Analysis);
                    if self
                        .streaming
                        .as_ref()
                        .is_none_or(|live| live.analysis_failure.is_none())
                    {
                        self.apply_analysis_input(live::AnalysisInput::Zone(id, name))?;
                    }
                }
                Event::AddProfileZoneEvent {
                    id, name, event_id, ..
                } => {
                    if self
                        .validation
                        .as_ref()
                        .is_some_and(|state| state.skip_analysis)
                    {
                        continue;
                    }
                    let _timer = self.metrics.measure(Stage::Analysis);
                    if self
                        .streaming
                        .as_ref()
                        .is_none_or(|live| live.analysis_failure.is_none())
                    {
                        self.apply_analysis_input(live::AnalysisInput::Event(id, event_id, name))?;
                    }
                }
                Event::SetCamera {
                    name,
                    position,
                    up,
                    target,
                } => {
                    self.action(Action::Camera(
                        name,
                        SourceCamera {
                            position,
                            up,
                            target,
                        },
                    ))?;
                }
                Event::BeginSection { .. } | Event::StreamEnd { .. } => {}
            }
        }
        Ok(())
    }

    fn mutate_analysis<T>(
        &mut self,
        mut mutation: impl FnMut(&mut analysis::SharedAnalysis) -> Result<T, Error>,
    ) -> Result<T, Error> {
        match mutation(&mut self.recording.analysis) {
            Err(failure)
                if failure
                    .downcast_ref::<analysis::resident::ResidentBudgetExceeded>()
                    .is_some()
                    && self.streaming.is_some() =>
            {
                self.streaming
                    .as_mut()
                    .unwrap()
                    .history
                    .reclaim_analysis()?;
                mutation(&mut self.recording.analysis)
            }
            result => result,
        }
    }

    fn maintain_analysis(&mut self, frame: usize) -> Result<(), Error> {
        if self
            .streaming
            .as_ref()
            .is_some_and(|live| live.analysis_failure.is_some())
        {
            return Ok(());
        }
        let result = self.mutate_analysis(|analysis| analysis.maintain_live_history(frame));
        match result {
            Ok(true) => {
                if let Some(live) = &mut self.streaming {
                    live.analysis_input(live::AnalysisInput::Retain {
                        first_frame: self.recording.analysis.first_retained_frame(),
                        accounted_bytes: self.recording.analysis.accounted_bytes(),
                    });
                }
            }
            Ok(false) => {}
            Err(failure) => {
                self.analysis_result(Err(failure), live::AnalysisInput::Reset(frame))?
            }
        }
        Ok(())
    }

    fn apply_analysis_input(&mut self, input: live::AnalysisInput) -> Result<(), Error> {
        let result = self.mutate_analysis(|analysis| input.apply_shared(analysis, &mut || false));
        self.analysis_result(result, input)
    }

    fn analysis_result(
        &mut self,
        result: Result<(), Error>,
        input: live::AnalysisInput,
    ) -> Result<(), Error> {
        match (&mut self.streaming, result) {
            (Some(live), Ok(())) => live.analysis_input(input),
            (Some(live), Err(error)) => {
                let frame = live.frames_seen;
                live.analysis_failure = Some(error.to_string());
                self.recording.diagnostics.push(format!(
                    "Analysis decoding stopped at frame {frame}; scene decoding continues: {error}"
                ));
                if let Some(reset) = live.history.failure_analysis(frame)? {
                    self.recording.analysis = reset;
                } else {
                    self.recording.analysis.reset_at(frame)?;
                }
                live.analysis_input(live::AnalysisInput::Reset(frame));
            }
            (None, result) => result?,
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Recording, Error> {
        if !self.partial.is_empty() || !self.groups.is_empty() {
            return Err(error("stream ended inside a property transaction"));
        }
        if self.streaming.is_some() {
            return self.snapshot();
        }
        self.recording.memory_usage = self.memory_usage();
        if self.recording.frames.is_empty() {
            self.recording.initial = Arc::new(self.state);
            self.recording.diagnostics.push("No completed scene frames; metadata/object state is available without a playback timeline.".into());
        }
        Ok(self.recording)
    }
    pub fn snapshot(&self) -> Result<Recording, Error> {
        let _timer = self.metrics.measure(Stage::SnapshotPublish);
        let mut r = self.recording.clone();
        r.memory_usage = self.memory_usage();
        if let Some(live) = &self.streaming {
            let disk = live.snapshot();
            r.frames = disk.frames();
            r.disk = Some(disk);
            r.stream_errors = live.errors.iter().cloned().collect();
            r.streamed_frame_end = Some(live.frames_seen);
            return Ok(r);
        }
        if r.frames.is_empty() {
            r.initial = Arc::new(self.state.clone());
        }
        Ok(r)
    }
}

#[derive(Default)]
pub struct LiveRecording {
    decoder: Decoder,
    builder: Option<RecordingBuilder>,
    failed: bool,
    received_bytes: u64,
    history: Option<live::HistoryWriter>,
    continuous: bool,
    published_frame: Option<usize>,
    metrics: Metrics,
}
impl LiveRecording {
    #[cfg(test)]
    pub(crate) fn with_paged_history(config: live::HistoryConfig) -> Result<Self, Error> {
        Self::with_history(config)
    }

    #[cfg(test)]
    pub(crate) fn with_legacy_history(config: live::HistoryConfig) -> Result<Self, Error> {
        Ok(Self {
            history: Some(live::HistoryWriter::new(config)?),
            continuous: true,
            ..Self::default()
        })
    }
    /// Starts aggregate diagnostics for subsequent work; repeated calls share the collector.
    /// The first enabling call selects its mode.
    pub fn enable_metrics(&mut self) -> Metrics {
        self.metrics.enable();
        if let Some(history) = &mut self.history {
            history.set_metrics(self.metrics.clone());
        }
        if let Some(builder) = &mut self.builder {
            builder.set_metrics(self.metrics.clone());
        }
        self.metrics.clone()
    }

    /// Observes coarse progress without clocks or timing stacks. The first
    /// diagnostics-enabling call selects the mode shared by all observers.
    pub fn enable_progress_metrics(&mut self) -> Metrics {
        self.metrics.enable_progress();
        if let Some(history) = &mut self.history {
            history.set_metrics(self.metrics.clone());
        }
        if let Some(builder) = &mut self.builder {
            builder.set_metrics(self.metrics.clone());
        }
        self.metrics.clone()
    }

    pub fn with_history(config: live::HistoryConfig) -> Result<Self, Error> {
        Ok(Self {
            history: Some(live::HistoryWriter::new_paged(config)?),
            continuous: true,
            ..Self::default()
        })
    }
    pub fn take_update(&mut self) -> Option<(usize, live::SceneUpdate)> {
        let live = self.builder.as_mut()?.streaming.as_mut()?;
        let frame = live.frame.as_ref()?.index;
        if self.published_frame == Some(frame) {
            return None;
        }
        self.published_frame = Some(frame);
        Some((frame, std::mem::take(&mut live.pending)))
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        if self.failed {
            return Err(error("live recording failed; start another connection"));
        }
        let result = self.feed_inner(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn feed_inner(&mut self, bytes: &[u8]) -> Result<usize, Error> {
        self.received_bytes = self
            .received_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| error("capture size overflow"))?;
        if !self.continuous && self.received_bytes > 512 << 20 {
            return Err(error(
                "live capture exceeds the current 512 MiB raw stream limit; start another capture",
            ));
        }
        let groups = {
            let _timer = self.metrics.measure(Stage::ProtocolParse);
            self.decoder.feed(bytes)?
        };
        if self.builder.is_none()
            && let Some(header) = self.decoder.header()
        {
            let mut builder = RecordingBuilder::new(header.clone());
            if let Some(history) = self.history.take() {
                if let Some(analysis) = history.initial_analysis() {
                    builder.recording.analysis = analysis;
                }
                builder.streaming = Some(Box::new(live::Streaming::new(history)));
            }
            builder.set_metrics(self.metrics.clone());
            self.builder = Some(builder);
        }
        if let Some(builder) = self.builder.as_mut() {
            for group in groups {
                builder.push(group)?;
            }
            Ok(builder
                .streaming
                .as_ref()
                .map_or(builder.recording.frame_count(), |live| live.frames_seen))
        } else {
            Ok(0)
        }
    }
    pub fn frame_count(&self) -> usize {
        self.builder.as_ref().map_or(0, |b| {
            b.streaming
                .as_ref()
                .map_or(b.recording.frame_count(), |live| live.frames_seen)
        })
    }
    pub fn history_memory_usage(&self) -> Option<HistoryMemoryUsage> {
        self.builder.as_ref().map(RecordingBuilder::memory_usage)
    }
    pub fn snapshot(&self) -> Result<Recording, Error> {
        self.builder
            .as_ref()
            .ok_or_else(|| error("waiting for stream initialization"))?
            .snapshot()
    }
    pub fn finish(self) -> Result<Recording, Error> {
        if self.failed {
            return Err(error("live recording failed"));
        }
        self.decoder.finish()?;
        self.builder
            .ok_or_else(|| error("missing stream initialization"))?
            .finish()
    }
}

fn decode_value(
    typename: &str,
    data: &[u8],
    items: u32,
    array: bool,
    header: &Header,
    strings: &BTreeMap<u32, String>,
) -> Result<Value, Error> {
    if let Some(value) = queries::decode_property(typename, data, items, header)? {
        return Ok(value);
    }
    if let Some(value) = cloth::decode_property(typename, data, items, array, header)? {
        return Ok(value);
    }
    if items > 1_000_000 {
        return Err(error("property exceeds one million items"));
    }
    let name = typename.rsplit("::").next().unwrap_or(typename);
    let ptr = if header.flags & 1 != 0 { 8 } else { 4 };
    let size = match name {
        "PvdBool" | "PvdU8" | "PvdI8" => 1,
        "PvdU16" | "PvdI16" => 2,
        "PvdU32"
        | "PvdI32"
        | "PvdF32"
        | "StringHandle"
        | "PvdColor"
        | "PvdHullPolygonData"
        | "PxHeightFieldSample" => 4,
        "PvdU64" | "PvdI64" | "PvdF64" | "ObjectRef" => 8,
        "VoidPtr" | "String" => ptr,
        "PxVec2" => 8,
        "PxVec3" => 12,
        "PxVec4" | "PxQuat" | "U32Array4" => 16,
        "PxBounds3" => 24,
        "PxTransform" => 28,
        "PxMat33" => 36,
        "PxMat44" => 64,
        _ => return Ok(Value::Bytes(Arc::from(data))),
    };
    if name == "String" {
        if data.len() < ptr {
            return Err(error("string property is missing pointer storage"));
        }
        let text = &data[ptr..];
        let n = text
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| error("unterminated string property"))?;
        return Ok(Value::String(
            String::from_utf8_lossy(&text[..n]).into_owned(),
        ));
    }
    let expected = (items as usize)
        .checked_mul(size)
        .ok_or_else(|| error("property size overflow"))?;
    if data.len() != expected {
        return Err(error(format!(
            "{typename}: {items} items need {expected} bytes, got {}",
            data.len()
        )));
    }
    let uint = |b: &[u8]| -> u64 {
        let mut bytes = [0u8; 8];
        match header.endian {
            Endian::Little => {
                bytes[..b.len()].copy_from_slice(b);
                u64::from_le_bytes(bytes)
            }
            Endian::Big => {
                bytes[8 - b.len()..].copy_from_slice(b);
                u64::from_be_bytes(bytes)
            }
        }
    };
    let float = |b: &[u8]| f32::from_bits(uint(b) as u32);
    let decode_one = |b: &[u8]| match name {
        "PvdHullPolygonData" => {
            Value::Array(vec![Value::U64(uint(&b[..2])), Value::U64(uint(&b[2..]))].into())
        }
        "PxHeightFieldSample" => Value::Array(
            vec![
                Value::I64(i64::from(uint(&b[..2]) as i16)),
                Value::U64(u64::from(b[2])),
                Value::U64(u64::from(b[3])),
            ]
            .into(),
        ),
        "PvdBool" => Value::Bool(b[0] != 0),
        "PvdI8" => Value::I64(i64::from(b[0] as i8)),
        "PvdI16" => Value::I64(i64::from(uint(b) as i16)),
        "PvdI32" => Value::I64(i64::from(uint(b) as i32)),
        "PvdI64" => Value::I64(uint(b) as i64),
        "PvdU8" | "PvdU16" | "PvdU32" | "PvdU64" => Value::U64(uint(b)),
        "PvdF32" => Value::F32(float(b)),
        "PvdF64" => Value::F64(f64::from_bits(uint(b))),
        "StringHandle" => Value::String(
            strings
                .get(&(uint(b) as u32))
                .cloned()
                .unwrap_or_else(|| format!("<string handle {}>", uint(b))),
        ),
        "ObjectRef" | "VoidPtr" => Value::Ref(uint(b)),
        "PxVec3" => Value::Vec3([float(&b[0..4]), float(&b[4..8]), float(&b[8..12])]),
        "PxQuat" => Value::Quat([
            float(&b[0..4]),
            float(&b[4..8]),
            float(&b[8..12]),
            float(&b[12..16]),
        ]),
        "PxTransform" => Value::Transform {
            rotation: [
                float(&b[0..4]),
                float(&b[4..8]),
                float(&b[8..12]),
                float(&b[12..16]),
            ],
            position: [float(&b[16..20]), float(&b[20..24]), float(&b[24..28])],
        },
        "PxVec2" | "PxVec4" | "PxMat33" | "PxMat44" | "PxBounds3" => Value::Array(
            b.chunks_exact(4)
                .map(|v| Value::F32(float(v)))
                .collect::<Vec<_>>()
                .into(),
        ),
        "U32Array4" => Value::Array(
            b.chunks_exact(4)
                .map(|v| Value::U64(uint(v)))
                .collect::<Vec<_>>()
                .into(),
        ),
        _ => Value::Bytes(Arc::from(b)),
    };
    if !array && items == 1 {
        Ok(decode_one(data))
    } else {
        Ok(Value::Array(
            data.chunks_exact(size)
                .map(decode_one)
                .collect::<Vec<_>>()
                .into(),
        ))
    }
}

impl SceneState {
    fn expected_generation(&self, target: u64) -> u32 {
        self.objects
            .get(&target)
            .map(|o| o.generation)
            .unwrap_or_else(|| {
                self.generations
                    .get(&target)
                    .map_or(0, |g| g.saturating_add(1))
            })
    }
    pub fn resolve_reference(&self, owner: u64, property: &str, target: u64) -> Option<&Object> {
        let object = self.objects.get(&target)?;
        let bindings = self
            .reference_lifetimes
            .get(&(owner, property.to_owned()))?;
        bindings
            .get(&target)
            .is_some_and(|generation| *generation == object.generation)
            .then_some(object)
    }
    pub fn render_scene(&self) -> RenderScene {
        self.render_scene_with_limits(100_000, 128 * 1024 * 1024)
    }
    pub fn render_scene_cached(&self, cache: &mut MeshCache) -> RenderScene {
        self.render_scene_with_cache(100_000, 128 * 1024 * 1024, cache)
    }
    /// The caller maintains the combined display budget when merging actor subsets.
    pub fn render_actors_cached(
        &self,
        actors: impl IntoIterator<Item = u64>,
        cache: &mut MeshCache,
    ) -> RenderScene {
        self.render_actor_objects(
            actors.into_iter().filter_map(|id| self.objects.get(&id)),
            100_000,
            128 * 1024 * 1024,
            cache,
            Vec::new(),
        )
    }
    pub fn articulation_for_link(&self, id: u64) -> Option<&Object> {
        let mut link = self.objects.get(&id)?;
        // PhysX 3.4 articulations contain at most 64 links; malformed parent cycles stay bounded.
        for _ in 0..64 {
            if !link.type_name.ends_with("::PxArticulationLink") {
                return None;
            }
            let Value::Ref(parent) = link.properties.get("Parent")? else {
                return None;
            };
            let parent = self.resolve_reference(link.id, "Parent", *parent)?;
            if parent.type_name.ends_with("::PxArticulation") {
                return Some(parent);
            }
            link = parent;
        }
        None
    }
    fn render_scene_with_limits(&self, max_shapes: usize, max_mesh_bytes: usize) -> RenderScene {
        self.render_scene_with_cache(max_shapes, max_mesh_bytes, &mut MeshCache::default())
    }
    fn render_scene_with_cache(
        &self,
        max_shapes: usize,
        max_mesh_bytes: usize,
        cache: &mut MeshCache,
    ) -> RenderScene {
        let mut diagnostics = Vec::new();
        if self
            .objects
            .values()
            .any(|object| object.type_name.ends_with("::PxCloth"))
        {
            diagnostics.push("PxCloth records contain particles and Fabric constraint edges, but no surface triangle topology or solver previousParticles. Use the cloth particle/constraint view; surface mesh display is unavailable.".into());
        }
        self.render_actor_objects(
            self.objects.values(),
            max_shapes,
            max_mesh_bytes,
            cache,
            diagnostics,
        )
    }
    fn render_actor_objects<'a>(
        &self,
        actors: impl IntoIterator<Item = &'a Object>,
        max_shapes: usize,
        max_mesh_bytes: usize,
        cache: &mut MeshCache,
        mut diagnostics: Vec<String>,
    ) -> RenderScene {
        cache.retain_resources(self);
        let mut result = Vec::new();
        let mut mesh_bytes = 0usize;
        let mut mesh_cache = BTreeMap::<mesh::MeshKey, Option<Geometry>>::new();
        let mut skipped = 0usize;
        for actor in actors.into_iter().filter(|o| {
            o.type_name.ends_with("::PxRigidDynamic")
                || o.type_name.ends_with("::PxRigidStatic")
                || o.type_name.ends_with("::PxArticulationLink")
        }) {
            let (position, rotation) = transform(actor.properties.get("GlobalPose"));
            let dynamic = !actor.type_name.ends_with("::PxRigidStatic");
            let sleep_owner = if actor.type_name.ends_with("::PxArticulationLink") {
                self.articulation_for_link(actor.id)
            } else {
                Some(actor)
            };
            let sleeping = matches!(
                sleep_owner.and_then(|owner| owner.properties.get("IsSleeping")),
                Some(Value::Bool(true))
            );
            let shapes = refs(actor.properties.get("Shapes"));
            for shape_id in shapes {
                if result.len() >= max_shapes {
                    diagnostics.push(format!("Scene display stopped at the {max_shapes} shape limit; recorded object data remains available."));
                    return RenderScene {
                        objects: result,
                        diagnostics,
                    };
                }
                let Some(shape) = self.resolve_reference(actor.id, "Shapes", shape_id) else {
                    continue;
                };
                let (local_position, local_rotation) = transform(shape.properties.get("LocalPose"));
                let geo_id = refs(shape.properties.get("Geometry")).first().copied();
                let Some(geo) =
                    geo_id.and_then(|id| self.resolve_reference(shape.id, "Geometry", id))
                else {
                    continue;
                };
                let geometry = if geo.type_name.ends_with("::PxBoxGeometry") {
                    vec3(geo.properties.get("HalfExtents"))
                        .map(|half_extents| Geometry::Box { half_extents })
                } else if geo.type_name.ends_with("::PxSphereGeometry") {
                    scalar(geo.properties.get("Radius")).map(|radius| Geometry::Sphere { radius })
                } else if geo.type_name.ends_with("::PxCapsuleGeometry") {
                    scalar(geo.properties.get("Radius"))
                        .zip(scalar(geo.properties.get("HalfHeight")))
                        .map(|(radius, half_height)| Geometry::Capsule {
                            radius,
                            half_height,
                        })
                } else if geo.type_name.ends_with("::PxPlaneGeometry") {
                    Some(Geometry::Plane)
                } else if let Some(key) = self.mesh_key(geo) {
                    if let Some(cached) = mesh_cache.get(&key) {
                        cached.clone()
                    } else {
                        let decoded = cache.decode(self, geo);
                        if let Some(Geometry::TriangleMesh { vertices, indices }) = &decoded {
                            let size = vertices.len() * std::mem::size_of::<[f32; 3]>()
                                + indices.len() * std::mem::size_of::<u32>();
                            if size > max_mesh_bytes.saturating_sub(mesh_bytes) {
                                diagnostics.push(format!("Scene display stopped at its {} MiB unique mesh allocation limit; recorded mesh data remains available.", max_mesh_bytes >> 20));
                                return RenderScene {
                                    objects: result,
                                    diagnostics,
                                };
                            }
                            mesh_bytes += size;
                        }
                        mesh_cache.insert(key, decoded.clone());
                        decoded
                    }
                } else {
                    None
                };
                if let Some(geometry) = geometry {
                    let offset = rotate(rotation, local_position);
                    result.push(RenderObject {
                        id: actor.id,
                        shape_id,
                        position: [
                            position[0] + offset[0],
                            position[1] + offset[1],
                            position[2] + offset[2],
                        ],
                        rotation: quat_mul(rotation, local_rotation),
                        geometry,
                        dynamic,
                        sleeping,
                    });
                } else {
                    skipped += 1;
                }
            }
        }
        if skipped > 0 {
            diagnostics.push(format!("{skipped} shapes have unsupported, malformed or oversized geometry. Per-mesh display limits are 1,000,000 vertices and 3,000,000 indices."));
        }
        RenderScene {
            objects: result,
            diagnostics,
        }
    }
}
fn refs(value: Option<&Value>) -> Vec<u64> {
    match value {
        Some(Value::Ref(id)) => vec![*id],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| {
                if let Value::Ref(id) = v {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}
fn scalar(v: Option<&Value>) -> Option<f32> {
    match v {
        Some(Value::F32(n)) => Some(*n),
        _ => None,
    }
}
fn vec3(v: Option<&Value>) -> Option<[f32; 3]> {
    match v {
        Some(Value::Vec3(n)) => Some(*n),
        _ => None,
    }
}
fn transform(v: Option<&Value>) -> ([f32; 3], [f32; 4]) {
    match v {
        Some(Value::Transform { position, rotation }) => (*position, *rotation),
        _ => ([0.0; 3], [0.0, 0.0, 0.0, 1.0]),
    }
}
fn quat_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let r = quat_mul(
        quat_mul(q, [v[0], v[1], v[2], 0.0]),
        [-q[0], -q[1], -q[2], q[3]],
    );
    [r[0], r[1], r[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    fn header() -> Header {
        Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: Endian::Little,
        }
    }
    fn group(events: Vec<Event>) -> EventGroup {
        EventGroup {
            stream_id: 1,
            timestamp: 0,
            offset: 0,
            events,
        }
    }
    #[test]
    fn errors_follow_event_order_and_preserve_unframed_tail() {
        let mut builder = RecordingBuilder::new(header());
        let report = |code| Event::ErrorMessage {
            code,
            message: format!("error {code}"),
            file: "fixture.cpp".into(),
            line: code,
        };
        builder
            .push(group(vec![
                Event::StringHandle {
                    handle: 1,
                    value: "frame".into(),
                },
                report(1),
                Event::EndSection {
                    id: 9,
                    name: 1,
                    timestamp: 500,
                },
                Event::EndSection {
                    id: 9,
                    name: 1,
                    timestamp: 500,
                },
                report(2),
                Event::EndSection {
                    id: 9,
                    name: 1,
                    timestamp: 100,
                },
                report(3),
            ]))
            .unwrap();
        let recording = builder.finish().unwrap();
        assert_eq!(
            recording
                .error_events()
                .map(|(frame, error)| (frame, error.code))
                .collect::<Vec<_>>(),
            [(Some(0), 1), (Some(2), 2), (None, 3)]
        );
        for (frame, event) in recording.error_events() {
            if let Some(frame) = frame {
                assert!(recording.state_at(frame).unwrap().errors.contains(event));
            }
        }
        assert_eq!(recording.state_at(1).unwrap().errors.len(), 1);
        assert_eq!(recording.state_at(2).unwrap().errors.len(), 2);
    }
    #[test]
    fn replay_id_reuse_and_backward_seek() {
        let mut b = RecordingBuilder::new(header());
        let name = Name {
            namespace: 0,
            name: 1,
        };
        b.push(group(vec![
            Event::StringHandle {
                handle: 1,
                value: "Actor".into(),
            },
            Event::StringHandle {
                handle: 2,
                value: "frame".into(),
            },
            Event::StringHandle {
                handle: 3,
                value: "mass".into(),
            },
            Event::StringHandle {
                handle: 4,
                value: "PvdF32".into(),
            },
            Event::CreateClass { name },
        ]))
        .unwrap();
        for i in 0..130 {
            let mut events = Vec::new();
            if i == 0 || i == 70 {
                events.push(Event::CreateInstance {
                    class: name,
                    id: 99,
                });
            }
            if i == 69 {
                events.push(Event::DestroyInstance { id: 99 });
            } else {
                events.push(Event::SetPropertyValue {
                    id: 99,
                    property: 3,
                    datatype: Name {
                        namespace: 0,
                        name: 4,
                    },
                    items: 1,
                    data: Arc::from((i as f32).to_le_bytes()),
                });
            }
            events.push(Event::EndSection {
                id: 1,
                name: 2,
                timestamp: i,
            });
            b.push(group(events)).unwrap();
        }
        let r = b.finish().unwrap();
        let mut sequential = SceneState::default();
        let mut cursor = 0;
        for i in 0..130 {
            for a in r.actions.get(cursor..r.frame_ends[i]).unwrap() {
                a.apply(&mut sequential);
            }
            cursor = r.frame_ends[i];
            assert_eq!(sequential, r.state_at(i).unwrap());
        }
        assert!(!r.state_at(69).unwrap().objects.contains_key(&99));
        assert_eq!(r.state_at(70).unwrap().objects[&99].generation, 1);
        assert_eq!(
            r.state_at(12).unwrap().objects[&99].properties["mass"],
            Value::F32(12.0)
        );
    }
    #[test]
    fn incomplete_property_transaction_is_rejected() {
        let mut b = RecordingBuilder::new(header());
        b.push(group(vec![Event::BeginSetPropertyValue {
            id: 1,
            property: 1,
            datatype: Name {
                namespace: 0,
                name: 1,
            },
        }]))
        .unwrap();
        assert!(b.finish().is_err());
    }

    #[test]
    fn interleaved_unfinished_transactions_have_an_aggregate_limit() {
        let mut builder = RecordingBuilder::new(header());
        for stream in 0..1024 {
            let mut event = group(vec![Event::BeginSetPropertyValue {
                id: 1,
                property: 1,
                datatype: Name {
                    namespace: 0,
                    name: 1,
                },
            }]);
            event.stream_id = stream;
            builder.push(event).unwrap();
        }
        let mut event = group(vec![Event::BeginSetPropertyValue {
            id: 1,
            property: 1,
            datatype: Name {
                namespace: 0,
                name: 1,
            },
        }]);
        event.stream_id = 1024;
        assert!(
            builder
                .push(event)
                .unwrap_err()
                .to_string()
                .contains("1024 concurrent")
        );
        assert_eq!(builder.partial.len(), 1024);
        builder.partial_bytes = 64 << 20;
        let result = builder.push(group(vec![Event::AppendPropertyValueData {
            items: 1,
            data: Arc::from([1u8]),
        }]));
        assert!(result.unwrap_err().to_string().contains("aggregate 64 MiB"));
        assert!(builder.partial[&1].data.is_empty());
    }
    #[test]
    fn references_do_not_keep_destroyed_objects_alive() {
        let mut s = SceneState::default();
        let o = Object {
            id: 1,
            generation: 0,
            type_name: "Actor".into(),
            properties: BTreeMap::new(),
        };
        Action::Create(o).apply(&mut s);
        Action::Push(1, "Shapes".into(), 2).apply(&mut s);
        Action::Push(1, "Shapes".into(), 2).apply(&mut s);
        assert_eq!(
            s.objects[&1].properties["Shapes"],
            Value::Array(vec![Value::Ref(2)].into())
        );
        Action::Remove(1, "Shapes".into(), 2).apply(&mut s);
        assert_eq!(
            s.objects[&1].properties["Shapes"],
            Value::Array(Arc::new(Vec::new()))
        );
    }
    #[test]
    fn stale_reference_does_not_bind_to_reused_pointer() {
        let mut state = SceneState::default();
        for id in [1, 2] {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: "Object".into(),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
        }
        Action::Set(1, "target".into(), Value::Ref(2)).apply(&mut state);
        Action::Set(
            1,
            "nested".into(),
            Value::Array(vec![Value::Array(vec![Value::Ref(2)].into())].into()),
        )
        .apply(&mut state);
        assert_eq!(
            state.resolve_reference(1, "target", 2).unwrap().generation,
            0
        );
        Action::Destroy(2).apply(&mut state);
        Action::Create(Object {
            id: 2,
            generation: 1,
            type_name: "Replacement".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
        assert!(state.resolve_reference(1, "target", 2).is_none());
        assert!(state.resolve_reference(1, "nested", 2).is_none());
        Action::Set(1, "target".into(), Value::Ref(2)).apply(&mut state);
        assert_eq!(
            state.resolve_reference(1, "target", 2).unwrap().generation,
            1
        );
    }
    #[test]
    fn destruction_removes_owned_bindings_and_preserves_incoming_generations() {
        let mut state = SceneState::default();
        for id in [1, 2, 3, u64::MAX] {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: "Object".into(),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
            Action::Set(id, "target".into(), Value::Ref(2)).apply(&mut state);
            Action::Set(
                id,
                "nested".into(),
                Value::Array(vec![Value::Ref(1), Value::Ref(3)].into()),
            )
            .apply(&mut state);
        }
        let bindings = state.reference_lifetimes.clone();
        Action::Destroy(2).apply(&mut state);
        assert_eq!(state.reference_lifetimes.len(), 6);
        for ((owner, key), targets) in &bindings {
            if *owner == 2 {
                assert!(
                    !state
                        .reference_lifetimes
                        .contains_key(&(*owner, key.clone()))
                );
            } else {
                assert_eq!(
                    state.reference_lifetimes.get(&(*owner, key.clone())),
                    Some(targets)
                );
            }
        }
        Action::Create(Object {
            id: 2,
            generation: 1,
            type_name: "Replacement".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
        assert!(state.resolve_reference(1, "target", 2).is_none());
        Action::Set(1, "target".into(), Value::Ref(2)).apply(&mut state);
        assert_eq!(
            state.resolve_reference(1, "target", 2).unwrap().generation,
            1
        );
        Action::Destroy(u64::MAX).apply(&mut state);
        assert!(
            state
                .reference_lifetimes
                .keys()
                .all(|(owner, _)| *owner != u64::MAX)
        );
        assert_eq!(state.reference_lifetimes.len(), 4);
    }

    #[test]
    fn articulation_owner_rejects_stale_cycles_and_chains_beyond_sdk_limit() {
        let mut state = SceneState::default();
        for id in 1..=66 {
            Action::Create(Object {
                id,
                generation: 0,
                type_name: if id == 66 {
                    "physx3::PxArticulation"
                } else {
                    "physx3::PxArticulationLink"
                }
                .into(),
                properties: BTreeMap::new(),
            })
            .apply(&mut state);
            if id != 66 {
                Action::Set(id, "Parent".into(), Value::Ref(id + 1)).apply(&mut state);
            }
        }
        assert_eq!(state.articulation_for_link(2).unwrap().id, 66);
        assert!(state.articulation_for_link(1).is_none());
        assert!(state.articulation_for_link(66).is_none());
        Action::Set(33, "Parent".into(), Value::Ref(2)).apply(&mut state);
        assert!(state.articulation_for_link(2).is_none());
        Action::Set(33, "Parent".into(), Value::Ref(34)).apply(&mut state);
        Action::Destroy(66).apply(&mut state);
        Action::Create(Object {
            id: 66,
            generation: 1,
            type_name: "physx3::PxArticulation".into(),
            properties: BTreeMap::new(),
        })
        .apply(&mut state);
        assert!(state.articulation_for_link(2).is_none());
        Action::Set(65, "Parent".into(), Value::Ref(66)).apply(&mut state);
        assert_eq!(state.articulation_for_link(2).unwrap().generation, 1);
        Action::Set(33, "Parent".into(), Value::U64(34)).apply(&mut state);
        assert!(state.articulation_for_link(2).is_none());
    }
    #[test]
    fn history_budget_stops_growth_before_adding_action() {
        let mut b = RecordingBuilder::with_memory_limit(header(), 1024);
        let result = b.action(Action::Set(
            1,
            "blob".into(),
            Value::Bytes(vec![0; 2048].into()),
        ));
        assert!(result.unwrap_err().to_string().contains("memory budget"));
        assert!(b.recording.actions.is_empty());
        assert_eq!(b.retained_bytes, 0);
    }

    #[test]
    fn unchanged_values_are_compacted_without_losing_reference_rebinding() {
        let mut builder = RecordingBuilder::new(header());
        for id in 1..=2 {
            builder
                .action(Action::Create(Object {
                    id,
                    generation: 0,
                    type_name: "Object".into(),
                    properties: BTreeMap::new(),
                }))
                .unwrap();
        }
        builder
            .action(Action::Set(1, "target".into(), Value::Ref(2)))
            .unwrap();
        builder
            .action(Action::Set(
                1,
                "value".into(),
                Value::F32(f32::from_bits(0x7fc00001)),
            ))
            .unwrap();
        let initial_actions = builder.recording.actions.len();
        for _ in 0..10_000 {
            builder
                .action(Action::Set(1, "target".into(), Value::Ref(2)))
                .unwrap();
            builder
                .action(Action::Set(
                    1,
                    "value".into(),
                    Value::F32(f32::from_bits(0x7fc00001)),
                ))
                .unwrap();
        }
        assert_eq!(builder.recording.actions.len(), initial_actions);
        assert_eq!(builder.skipped_redundant_actions, 20_000);
        builder.action(Action::Destroy(2)).unwrap();
        builder
            .action(Action::Create(Object {
                id: 2,
                generation: 1,
                type_name: "Object".into(),
                properties: BTreeMap::new(),
            }))
            .unwrap();
        assert!(builder.state.resolve_reference(1, "target", 2).is_none());
        builder
            .action(Action::Set(1, "target".into(), Value::Ref(2)))
            .unwrap();
        assert_eq!(
            builder
                .state
                .resolve_reference(1, "target", 2)
                .unwrap()
                .generation,
            1
        );
        builder
            .action(Action::Set(
                1,
                "value".into(),
                Value::F32(f32::from_bits(0x7fc00002)),
            ))
            .unwrap();
        builder
            .action(Action::Set(1, "value".into(), Value::F32(-0.0)))
            .unwrap();
        builder
            .action(Action::Set(1, "value".into(), Value::F32(0.0)))
            .unwrap();
        assert_eq!(builder.recording.actions.len(), initial_actions + 6);
    }

    #[test]
    fn evicted_checkpoints_and_shared_snapshots_preserve_every_frame() {
        let mut builder = RecordingBuilder::with_memory_limit(header(), 384 * 1024);
        builder.strings.insert(1, "frame".into());
        for id in 1..=80 {
            builder
                .action(Action::Create(Object {
                    id,
                    generation: 0,
                    type_name: "Object".into(),
                    properties: BTreeMap::new(),
                }))
                .unwrap();
            builder
                .action(Action::Set(
                    id,
                    "constant".into(),
                    Value::String("captured property".into()),
                ))
                .unwrap();
            builder
                .action(Action::Set(id, "target".into(), Value::Ref(80)))
                .unwrap();
        }
        let mut snapshot = None;
        for frame in 0..600 {
            if frame == 119 {
                builder.action(Action::Destroy(80)).unwrap();
            }
            if frame == 120 {
                builder
                    .action(Action::Create(Object {
                        id: 80,
                        generation: 1,
                        type_name: "Object".into(),
                        properties: BTreeMap::new(),
                    }))
                    .unwrap();
            }
            if frame == 121 {
                builder
                    .action(Action::Set(1, "target".into(), Value::Ref(80)))
                    .unwrap();
            }
            builder
                .action(Action::Set(1, "position".into(), Value::U64(frame)))
                .unwrap();
            builder
                .push(group(vec![Event::EndSection {
                    id: 10,
                    name: 1,
                    timestamp: frame,
                }]))
                .unwrap();
            if frame == 120 {
                snapshot = Some(builder.snapshot().unwrap());
            }
        }
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.frame_count(), 121);
        assert!(
            snapshot
                .state_at(120)
                .unwrap()
                .resolve_reference(1, "target", 80)
                .is_none()
        );
        assert_eq!(
            snapshot.state_at(120).unwrap().objects[&1].properties["position"],
            Value::U64(120)
        );
        let recording = builder.finish().unwrap();
        assert_eq!(
            recording
                .state_at(121)
                .unwrap()
                .resolve_reference(1, "target", 80)
                .unwrap()
                .generation,
            1
        );
        assert!(Arc::ptr_eq(&recording.initial, &snapshot.initial));
        let usage = recording.history_memory_usage();
        assert!(
            usage.checkpoint_count < 9,
            "cache must have evicted a checkpoint: {usage:?}"
        );
        assert!(
            usage.estimated_history_bytes + usage.estimated_checkpoint_bytes <= usage.limit_bytes
        );
        for frame in [599, 0, 359, 60, 358, 120, 598] {
            assert_eq!(
                recording.state_at(frame).unwrap().objects[&1].properties["position"],
                Value::U64(frame as u64)
            );
        }
        recording.verify_replay().unwrap();
    }

    #[test]
    fn repeated_enum_names_are_budgeted_before_expansion() {
        let mut builder = RecordingBuilder::with_memory_limit(header(), 64 * 1024);
        builder
            .push(group(vec![
                Event::StringHandle {
                    handle: 1,
                    value: "Object".into(),
                },
                Event::StringHandle {
                    handle: 2,
                    value: "flags".into(),
                },
                Event::StringHandle {
                    handle: 3,
                    value: "x".repeat(32 * 1024),
                },
            ]))
            .unwrap();
        let result = builder.push(group(vec![Event::CreateProperty {
            class: Name {
                namespace: 0,
                name: 1,
            },
            name: 2,
            semantic: 0,
            datatype: Name {
                namespace: 0,
                name: 1,
            },
            kind: 1,
            named_values: vec![(3, 1); 100_000],
        }]));
        assert!(result.unwrap_err().to_string().contains("memory budget"));
        assert!(builder.recording.classes.is_empty());
    }

    #[test]
    fn cyclic_inheritance_is_rejected_without_changing_existing_metadata() {
        let mut builder = RecordingBuilder::new(header());
        let a = Name {
            namespace: 0,
            name: 1,
        };
        let b = Name {
            namespace: 0,
            name: 2,
        };
        builder
            .push(group(vec![
                Event::StringHandle {
                    handle: 1,
                    value: "A".into(),
                },
                Event::StringHandle {
                    handle: 2,
                    value: "B".into(),
                },
                Event::CreateClass { name: a },
                Event::CreateClass { name: b },
                Event::DeriveClass {
                    parent: a,
                    child: b,
                },
            ]))
            .unwrap();
        let error = builder
            .push(group(vec![Event::DeriveClass {
                parent: b,
                child: a,
            }]))
            .unwrap_err();
        assert!(error.to_string().contains("cycle"));
        assert!(builder.recording.classes["A"].parent.is_none());
        assert_eq!(builder.recording.classes["B"].parent.as_deref(), Some("A"));
    }

    #[test]
    fn large_reference_arrays_keep_snapshot_order_and_lifetimes() {
        let mut builder = RecordingBuilder::new(header());
        let class = Name {
            namespace: 0,
            name: 1,
        };
        builder
            .push(group(vec![
                Event::StringHandle {
                    handle: 1,
                    value: "Object".into(),
                },
                Event::StringHandle {
                    handle: 2,
                    value: "frame".into(),
                },
                Event::StringHandle {
                    handle: 3,
                    value: "Children".into(),
                },
                Event::CreateInstance { class, id: 1 },
                Event::CreateInstance { class, id: 1000 },
            ]))
            .unwrap();
        for frame in 0..=60 {
            let mut events = Vec::new();
            if frame < 2 {
                for target in 100 + frame * 10_000..100 + (frame + 1) * 10_000 {
                    events.push(Event::PushBackObjectRef {
                        id: 1,
                        property: 3,
                        target,
                    });
                }
            }
            if frame == 60 {
                events.extend([
                    Event::DestroyInstance { id: 1000 },
                    Event::CreateInstance { class, id: 1000 },
                    Event::RemoveObjectRef {
                        id: 1,
                        property: 3,
                        target: 1001,
                    },
                ]);
            }
            events.push(Event::EndSection {
                id: 1,
                name: 2,
                timestamp: frame,
            });
            builder.push(group(events)).unwrap();
        }
        let recording = builder.finish().unwrap();
        recording.verify_replay().unwrap();
        let first = recording.state_at(0).unwrap();
        let last = recording.state_at(60).unwrap();
        let Value::Array(first_refs) = &first.objects[&1].properties["Children"] else {
            panic!()
        };
        let Value::Array(last_refs) = &last.objects[&1].properties["Children"] else {
            panic!()
        };
        assert_eq!(first_refs.len(), 10_000);
        assert_eq!(last_refs.len(), 19_999);
        assert_eq!(first_refs.first(), Some(&Value::Ref(100)));
        assert_eq!(first_refs.last(), Some(&Value::Ref(10_099)));
        assert_eq!(last_refs.last(), Some(&Value::Ref(20_099)));
        assert!(!last_refs.contains(&Value::Ref(1001)));
        assert_eq!(
            first
                .resolve_reference(1, "Children", 1000)
                .unwrap()
                .generation,
            0
        );
        assert!(last.resolve_reference(1, "Children", 1000).is_none());
        assert_eq!(last.objects[&1000].generation, 1);
    }

    #[test]
    fn replay_preserves_nan_payloads_and_signed_zero_across_checkpoints() {
        let nan = f32::from_bits(0x7fc0_1234);
        let mut builder = RecordingBuilder::new(header());
        builder
            .push(group(vec![Event::StringHandle {
                handle: 1,
                value: "frame".into(),
            }]))
            .unwrap();
        builder
            .action(Action::Create(Object {
                id: 3,
                generation: 0,
                type_name: "CustomProbe".into(),
                properties: BTreeMap::new(),
            }))
            .unwrap();
        builder
            .action(Action::Set(
                3,
                "values".into(),
                Value::Array(
                    vec![
                        Value::F32(nan),
                        Value::F64(f64::from_bits(0x7ff8_0000_0000_1234)),
                        Value::Transform {
                            position: [nan, -0.0, 0.0],
                            rotation: [nan, 0.0, 0.0, 1.0],
                        },
                        Value::Vec3([0.0, nan, -0.0]),
                        Value::Quat([nan, 0.0, 0.0, 1.0]),
                    ]
                    .into(),
                ),
            ))
            .unwrap();
        let pose = debug_draw::Pose {
            position: [nan, -0.0, 0.0],
            rotation: [0.0, 0.0, nan, 1.0],
        };
        for frame in 0..125 {
            builder
                .action(Action::Debug(
                    9,
                    vec![debug_draw::Primitive::JointFrames {
                        instance: 3,
                        parent: pose,
                        child: pose,
                    }]
                    .into(),
                ))
                .unwrap();
            builder
                .action(Action::Contacts(
                    10,
                    vec![contacts::Contact {
                        position: [nan, 0.0, -0.0],
                        normal: [0.0, 1.0, 0.0],
                        shapes: [3, 4],
                        separation: nan,
                        normal_force: nan,
                        faces: [0, 0],
                        normal_force_available: false,
                    }]
                    .into(),
                ))
                .unwrap();
            builder
                .push(group(vec![Event::EndSection {
                    id: 10,
                    name: 1,
                    timestamp: frame,
                }]))
                .unwrap();
        }
        let recording = builder.finish().unwrap();
        recording.verify_replay().unwrap();
        for frame in [124, 59, 60, 0, 123] {
            let state = recording.state_at(frame).unwrap();
            assert_eq!(state, state.clone());
        }
        assert_ne!(Value::F32(nan), Value::F32(f32::from_bits(0x7fc0_1235)));
        assert_ne!(Value::F32(-0.0), Value::F32(0.0));
        assert_ne!(Value::F64(-0.0), Value::F64(0.0));
    }
}
