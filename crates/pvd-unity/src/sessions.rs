use crate::geometry::{FocusRequest, FrameCache, ResourceCache};
use pvd_core::{
    LatestSceneFrame, LatestSceneReader, LiveRecording, Recording, SceneState,
    live::{HistoryConfig, ReplayCursor, SceneUpdate},
    rooms::RoomDirectory,
};
use pvd_transport::{
    CaptureControl, CaptureServer, ListenConfig, ReceiverEvent, SpoolOptions, SpoolStats,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs, io,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::RecvTimeoutError,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod latest;
#[cfg(test)]
mod tests;

const PUBLISH_INTERVAL: Duration = Duration::from_millis(50);
const MAX_DISPLAY_CATCH_UP_FRAMES: usize = 4;

struct Entry {
    leases: Arc<()>,
    snapshot: Option<Arc<Recording>>,
    directory: PathBuf,
    bytes: u64,
}

#[derive(Default)]
struct Catalog {
    entries: BTreeMap<u64, Entry>,
    live: Option<LiveView>,
    sequence: u64,
    active: u64,
    error: String,
}

impl Catalog {
    fn next_id(&mut self) -> Result<u64, String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("recording ID overflow")?;
        Ok(self.sequence)
    }
}

struct LiveView {
    id: u64,
    reader: Arc<LatestSceneReader>,
    connected: Arc<AtomicBool>,
    frames: usize,
    rooms: RoomDirectory,
    cache: Arc<FrameWork>,
    resources: Arc<ResourceCache>,
}

struct Listener {
    address: String,
    control: CaptureControl,
    abort: Arc<AtomicBool>,
    spool: Option<Arc<SpoolStats>>,
    worker: JoinHandle<()>,
}

#[derive(Default)]
pub struct Sessions {
    listener: Option<Listener>,
    catalog: Arc<Mutex<Catalog>>,
    viewed: Option<u64>,
    requests: Arc<AtomicU64>,
    frame_cache: Arc<FrameWork>,
    resources: Arc<ResourceCache>,
}

#[derive(Default)]
struct FrameWork {
    workspace: Mutex<FrameWorkspace>,
}

#[derive(Default)]
struct FrameWorkspace {
    geometry: FrameCache,
    cursor: ReplayCursor,
    lease: Option<Arc<()>>,
    scene: SceneState,
    session: Option<u64>,
    restored: Option<(Weak<Recording>, usize)>,
    latest_index: Option<usize>,
}

pub struct FrameJob {
    id: u64,
    sequence: u64,
    source: FrameSource,
    requests: Arc<AtomicU64>,
    cache: Arc<FrameWork>,
    resources: Arc<ResourceCache>,
    prepared: Option<ResourceCache>,
}

enum FrameSource {
    History(Snapshot),
    Live {
        reader: Weak<LatestSceneReader>,
        connected: Arc<AtomicBool>,
    },
}

pub struct Snapshot {
    pub recording: Arc<Recording>,
    // A background frame read or export can outlive the selected view.
    _lease: Arc<()>,
}

pub enum FrameFormat {
    Inline,
    Cached(u64),
    Resources(u64, Option<FocusRequest>),
    View(u64, Option<FocusRequest>),
    Queries(u64, Option<FocusRequest>, bool),
}

impl FrameWorkspace {
    fn seek_disk(
        &mut self,
        recording: &Recording,
        index: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<SceneUpdate, pvd_core::Error> {
        let start = self
            .restored
            .as_ref()
            .map(|(_, frame)| *frame)
            .filter(|&frame| {
                (2..=MAX_DISPLAY_CATCH_UP_FRAMES).contains(&index.saturating_sub(frame))
                    && recording.is_frame_retained(frame)
            })
            .map_or(index, |frame| frame + 1);
        let mut update = self.cursor.seek_scene(recording, start, &cancelled)?.update;
        // Live display can skip producer frames between editor updates. Fold short
        // runs of adjacent changes before applying them to the displayed scene.
        if start < index {
            for frame in (start + 1)..=index {
                update.merge(self.cursor.seek_scene(recording, frame, &cancelled)?.update);
            }
        }
        Ok(update)
    }

    fn seek(
        &mut self,
        id: u64,
        snapshot: &Snapshot,
        index: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), String> {
        if self.session != Some(id) {
            self.session = Some(id);
            self.cursor.clear();
            self.scene = SceneState::default();
            self.restored = None;
        }
        // The cursor may keep a history reader open between frame jobs.
        self.lease = Some(snapshot._lease.clone());
        let recording = &snapshot.recording;
        let snapshot = Arc::downgrade(recording);
        if self.restored.as_ref().is_some_and(|(previous, frame)| {
            *frame == index && previous.ptr_eq(&snapshot) && recording.is_frame_retained(index)
        }) {
            self.geometry.sync_spatial(id, &self.scene, Some(&[]));
            return Ok(());
        }

        let replay = if recording.is_disk_backed() {
            match self.seek_disk(recording, index, &cancelled) {
                Ok(update) => Some(update),
                Err(error) => {
                    self.cursor.clear();
                    if cancelled() || !recording.is_frame_retained(index) {
                        return Err(detail(error));
                    }
                    // An older cursor can lose its rewind source when history is sealed.
                    None
                }
            }
        } else {
            self.cursor.clear();
            None
        };
        let changes = if let Some(update) = replay {
            let changes = (!update.replace_all).then(|| {
                update
                    .changed_objects()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>()
            });
            self.geometry.retain_rooms(&update, &self.scene);
            update.apply_owned(&mut self.scene);
            changes
        } else {
            self.scene = recording
                .state_at_with_cancel(index, cancelled)
                .map_err(detail)?;
            None
        };
        self.geometry
            .sync_spatial(id, &self.scene, changes.as_deref());
        self.restored = Some((snapshot, index));
        Ok(())
    }

    fn seek_latest(&mut self, id: u64, frame: &LatestSceneFrame) {
        self.geometry.sync_latest(id, frame, self.latest_index);
        self.latest_index = Some(frame.index);
    }
}

impl FrameJob {
    pub fn read(&mut self, index: u64, room: u64, format: FrameFormat) -> Result<Vec<u8>, String> {
        self.prepared = None;
        let cancelled = || {
            self.requests.load(Ordering::Acquire) != self.sequence
                || matches!(&self.source, FrameSource::Live { connected, .. }
                    if !connected.load(Ordering::Acquire))
        };
        let check_current = || {
            if cancelled() {
                Err("frame request superseded".to_string())
            } else {
                Ok(())
            }
        };
        check_current()?;
        let mut work = self
            .cache
            .workspace
            .lock()
            .map_err(|_| "frame cache poisoned")?;
        check_current()?;
        let mut encode = |state: &SceneState, rooms: &RoomDirectory, geometry: &mut FrameCache| {
            check_current()?;
            let scope = (self.id, room);
            let data = match format {
                FrameFormat::Inline => crate::geometry::encode(state, rooms, room)?,
                FrameFormat::Cached(token) => geometry.encode(state, rooms, scope, token)?,
                FrameFormat::Resources(base, focus)
                | FrameFormat::View(base, focus)
                | FrameFormat::Queries(base, focus, _) => {
                    let region = focus.map(|focus| focus.region(state)).transpose()?;
                    let (scene, memberships) =
                        geometry.render(state, rooms, scope, base, region)?;
                    let (mut data, next) =
                        self.resources
                            .prepare(state, scene, &memberships, scope, base)?;
                    match format {
                        FrameFormat::View(..) => {
                            crate::geometry::append_rays(&mut data, state, rooms, room, region);
                        }
                        FrameFormat::Queries(_, _, include_rays) => {
                            crate::geometry::append_queries(
                                &mut data,
                                state,
                                rooms,
                                room,
                                region,
                                include_rays,
                            );
                        }
                        _ => {}
                    }
                    self.prepared = Some(next);
                    data
                }
            };
            check_current()?;
            Ok(data)
        };
        let result = (|| match &self.source {
            FrameSource::History(snapshot) => {
                let index = if index == u64::MAX {
                    snapshot
                        .recording
                        .frames
                        .last()
                        .ok_or("recording has no available frames")?
                        .index
                } else {
                    usize::try_from(index).map_err(|_| "frame index overflow")?
                };
                work.seek(self.id, snapshot, index, cancelled)?;
                let FrameWorkspace {
                    scene, geometry, ..
                } = &mut *work;
                encode(scene, &snapshot.recording.rooms, geometry)
            }
            FrameSource::Live { reader, .. } => {
                let reader = reader.upgrade().ok_or("live session disconnected")?;
                // Unwind only after releasing the shared producer slot.
                let encoded = reader.with_latest(|frame| {
                    catch_unwind(AssertUnwindSafe(|| {
                        check_current()?;
                        let frame = frame.ok_or("live session has no completed frame")?;
                        if index != u64::MAX && index != frame.index as u64 {
                            return Err("live session only has its latest completed frame".into());
                        }
                        work.seek_latest(self.id, frame);
                        encode(&frame.scene, &frame.rooms, &mut work.geometry)
                    }))
                });
                match encoded {
                    Ok(result) => result,
                    Err(panic) => resume_unwind(panic),
                }
            }
        })();
        if result.is_err() {
            work.cursor.clear();
            work.lease = None;
        }
        result
    }
}

fn detail(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn history_bytes(directory: &Path) -> io::Result<u64> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut bytes = 0u64;
    for entry in entries {
        let file = entry?;
        let metadata = fs::symlink_metadata(file.path())?;
        if metadata.is_file()
            && (file.file_name() == ".pvd-studio-owner"
                || file
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "pvdhistory"))
        {
            bytes = bytes.saturating_add(metadata.len());
        }
    }
    Ok(bytes)
}
impl Sessions {
    pub fn start(
        &mut self,
        root: &Path,
        address: &str,
        record_history: bool,
    ) -> Result<(), String> {
        if self
            .listener
            .as_ref()
            .is_some_and(|l| !l.worker.is_finished())
        {
            return Err("listener already running".into());
        }
        self.stop()?;
        let config = ListenConfig {
            address: address.parse().map_err(detail)?,
            max_session_bytes: u64::MAX,
            ..Default::default()
        };
        let (server, history) = if record_history {
            fs::create_dir_all(root).map_err(detail)?;
            let root = fs::canonicalize(root).map_err(detail)?;
            let history = root.join("history");
            fs::create_dir_all(&history).map_err(detail)?;
            if fs::canonicalize(&history).map_err(detail)? != history {
                return Err("history directory resolves outside the selected cache path".into());
            }
            pvd_core::live::cleanup_stale_history(&history).map_err(detail)?;
            let server = CaptureServer::bind_spooled(
                config,
                SpoolOptions {
                    directory: root.join("receive"),
                    ..Default::default()
                },
                || {},
            )
            .map_err(detail)?;
            (server, Some(history))
        } else {
            (CaptureServer::bind(config, || {}).map_err(detail)?, None)
        };
        let address = server.address().to_string();
        let control = server.control();
        let spool = server.spool_stats();
        let catalog = self.catalog.clone();
        catalog.lock().unwrap().error.clear();
        let abort = Arc::new(AtomicBool::new(false));
        let worker_abort = abort.clone();
        let worker = thread::Builder::new()
            .name("pvd-unity-ingest".into())
            .spawn(move || match history {
                Some(history) => ingest(server, &history, &catalog, &worker_abort),
                None => latest::ingest(server, &catalog, &worker_abort),
            })
            .map_err(detail)?;
        self.listener = Some(Listener {
            address,
            control,
            abort,
            spool,
            worker,
        });
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(listener) = self.listener.take() {
            if listener.spool.is_some() {
                listener.control.request_finish();
            } else {
                listener.abort.store(true, Ordering::Release);
                listener.control.request_stop();
            }
            listener
                .worker
                .join()
                .map_err(|_| "capture worker panicked")?;
        }
        Ok(())
    }

    pub fn status(&self) -> Value {
        let (active_id, error, summaries) = {
            let catalog = self.catalog.lock().unwrap();
            let mut summaries = catalog
                .entries
                .iter()
                .map(|(&id, entry)| {
                    (
                        id,
                        entry.bytes,
                        Arc::strong_count(&entry.leases) > 1,
                        false,
                        entry
                            .snapshot
                            .as_ref()
                            .map(|r| (r.frames.len(), r.rooms.clone())),
                    )
                })
                .collect::<Vec<_>>();
            if let Some(view) = &catalog.live {
                summaries.push((
                    view.id,
                    0,
                    false,
                    true,
                    Some((view.frames, view.rooms.clone())),
                ));
            }
            (catalog.active, catalog.error.clone(), summaries)
        };
        let mut total = self
            .listener
            .as_ref()
            .and_then(|l| l.spool.as_ref())
            .map_or(0, |spool| spool.snapshot().retained_disk_bytes);
        let mut clearable = 0u64;
        let sessions: Vec<_> = summaries
            .into_iter()
            .map(|(id, bytes, leased, live_only, snapshot)| {
                total = total.saturating_add(bytes);
                let active = active_id == id;
                if !active && !leased && self.viewed != Some(id) {
                    clearable = clearable.saturating_add(bytes);
                }
                let (frames, directory) = snapshot.unwrap_or_default();
                let rooms: Vec<_> = directory
                    .iter()
                    .filter(|info| !live_only || info.destroyed_at_frame.is_none())
                    .map(|info| json!({"number":info.number}))
                    .collect();
                let mut value = json!({"id":id, "frames":frames, "active":active, "rooms":rooms});
                if live_only {
                    value["liveOnly"] = json!(true);
                }
                value
            })
            .collect();
        json!({
            "listening":self.listener.as_ref().is_some_and(|l| !l.worker.is_finished()),
            "address":self.listener.as_ref().map_or("", |l| l.address.as_str()),
            "error":error, "active":active_id,
            "totalBytes":total, "clearableBytes":clearable, "sessions":sessions
        })
    }

    pub fn frame_count(&self, id: u64) -> Option<i64> {
        let catalog = self.catalog.try_lock().ok()?;
        if let Some(view) = catalog.live.as_ref().filter(|view| view.id == id) {
            return Some(view.frames as i64);
        }
        let entry = catalog.entries.get(&id)?;
        Some(entry.snapshot.as_ref().map_or(0, |r| r.frames.len() as i64))
    }

    pub fn snapshot(&self, id: u64) -> Result<Snapshot, String> {
        let catalog = self.catalog.lock().unwrap();
        if catalog.live.as_ref().is_some_and(|view| view.id == id) {
            return Err("a live-only session cannot be exported".into());
        }
        let entry = catalog
            .entries
            .get(&id)
            .ok_or("recording has no available frames")?;
        let recording = entry
            .snapshot
            .clone()
            .ok_or("recording has no available frames")?;
        Ok(Snapshot {
            recording,
            _lease: entry.leases.clone(),
        })
    }

    pub fn prepare_frame(&mut self, id: u64) -> Result<FrameJob, String> {
        let live = self
            .catalog
            .lock()
            .unwrap()
            .live
            .as_ref()
            .filter(|view| view.id == id)
            .map(|view| {
                (
                    FrameSource::Live {
                        reader: Arc::downgrade(&view.reader),
                        connected: view.connected.clone(),
                    },
                    view.cache.clone(),
                    view.resources.clone(),
                )
            });
        let (source, cache, resources) = match live {
            Some(live) => live,
            None => (
                FrameSource::History(self.snapshot(id)?),
                self.frame_cache.clone(),
                self.resources.clone(),
            ),
        };
        let sequence = self
            .requests
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or("frame request sequence overflow")?;
        self.requests.store(sequence, Ordering::Release);
        Ok(FrameJob {
            id,
            sequence,
            source,
            requests: self.requests.clone(),
            cache,
            resources,
            prepared: None,
        })
    }

    pub fn finish_frame(&mut self, job: &mut FrameJob, success: bool) -> bool {
        if !Arc::ptr_eq(&job.requests, &self.requests)
            || job.sequence != self.requests.load(Ordering::Acquire)
        {
            return false;
        }
        if matches!(&job.source, FrameSource::Live { .. }) {
            {
                let mut catalog = self.catalog.lock().unwrap();
                let Some(view) = catalog.live.as_mut().filter(|view| view.id == job.id) else {
                    return false;
                };
                if success && let Some(resources) = job.prepared.take() {
                    view.resources = Arc::new(resources);
                }
            }
            if success && self.viewed.take().is_some() {
                self.frame_cache = Arc::default();
                self.resources = Arc::default();
            }
        } else if success {
            if let Some(resources) = job.prepared.take() {
                self.resources = Arc::new(resources);
            }
            self.viewed = Some(job.id);
        }
        true
    }

    #[cfg(test)]
    fn frame(&mut self, id: u64, index: usize, room: u64) -> Result<Vec<u8>, String> {
        let mut job = self.prepare_frame(id)?;
        let result = job.read(index as u64, room, FrameFormat::Inline);
        self.finish_frame(&mut job, result.is_ok());
        result
    }

    pub fn release_view(&mut self) {
        self.viewed = None;
        self.requests.store(0, Ordering::Release);
        self.requests = Arc::default();
        self.frame_cache = Arc::default();
        self.resources = Arc::default();
        let old_view = self.catalog.lock().unwrap().live.as_mut().map(|view| {
            (
                std::mem::take(&mut view.cache),
                std::mem::take(&mut view.resources),
            )
        });
        drop(old_view);
    }

    pub fn clear(&mut self) -> Result<u64, String> {
        let entries = {
            let mut catalog = self.catalog.lock().unwrap();
            let active = catalog.active;
            catalog
                .entries
                .extract_if(.., |id, entry| {
                    *id != active
                        && self.viewed != Some(*id)
                        && Arc::strong_count(&entry.leases) == 1
                })
                .collect::<Vec<_>>()
        };
        let mut released = 0u64;
        let mut failure = None;
        for (id, mut entry) in entries {
            let cleanup = (|| -> io::Result<()> {
                let before = history_bytes(&entry.directory)?;
                drop(entry.snapshot.take());
                entry.bytes = history_bytes(&entry.directory)?;
                released = released.saturating_add(before.saturating_sub(entry.bytes));
                if entry.bytes != 0 {
                    let root = entry
                        .directory
                        .parent()
                        .ok_or_else(|| io::Error::other("missing history root"))?;
                    pvd_core::live::cleanup_stale_history(root)?;
                    let previous = entry.bytes;
                    entry.bytes = history_bytes(&entry.directory)?;
                    released = released.saturating_add(previous.saturating_sub(entry.bytes));
                    if entry.bytes != 0 {
                        return Err(io::Error::other(
                            "history files are still in use or unrecognized",
                        ));
                    }
                }
                Ok(())
            })();
            if let Err(error) = cleanup {
                failure = Some(format!("could not clear recording {id}: {error}"));
                self.catalog.lock().unwrap().entries.insert(id, entry);
            }
        }
        if let Some(warning) = failure {
            self.catalog.lock().unwrap().error = warning;
        }
        Ok(released)
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        self.requests.store(0, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            listener.abort.store(true, Ordering::Release);
            listener.control.request_stop();
            let _ = listener.worker.join();
        }
        // Dropping the catalog releases the core's owned history leases.
    }
}

struct Incoming {
    id: u64,
    live: Option<LiveRecording>,
    received_data: bool,
    published: usize,
    rooms_revision: u64,
    last_publish: Instant,
    directory: PathBuf,
}

impl Incoming {
    fn begin(
        root: &Path,
        catalog: &Mutex<Catalog>,
        abort: &Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let id = catalog.lock().unwrap().next_id()?;
        let mut live = LiveRecording::with_persistent_history(HistoryConfig {
            directory: root.to_path_buf(),
            ..Default::default()
        })
        .map_err(detail)?;
        live.set_cancellation(abort.clone());
        let directory = live
            .history_directory()
            .ok_or("missing history directory")?
            .to_path_buf();
        let mut state = catalog.lock().unwrap();
        state.entries.insert(
            id,
            Entry {
                leases: Arc::default(),
                snapshot: None,
                bytes: 0,
                directory: directory.clone(),
            },
        );
        state.active = id;
        state.error.clear();
        Ok(Self {
            id,
            live: Some(live),
            received_data: false,
            published: 0,
            rooms_revision: 0,
            last_publish: Instant::now(),
            directory,
        })
    }

    fn publish(&mut self, catalog: &Mutex<Catalog>, force: bool) {
        let Some(live) = &mut self.live else { return };
        let revision = live.rooms().map_or(0, |rooms| rooms.revision());
        if (self.published == live.frame_count() && self.rooms_revision == revision)
            || (!force && self.last_publish.elapsed() < PUBLISH_INTERVAL)
        {
            return;
        }
        self.last_publish = Instant::now();
        match live.snapshot() {
            Ok(recording) => {
                self.published = live.frame_count();
                self.rooms_revision = revision;
                let _ = live.take_update();
                publish_recording(catalog, self.id, recording);
            }
            Err(error) => catalog.lock().unwrap().error = error.to_string(),
        }
    }

    fn finish(mut self, catalog: &Mutex<Catalog>) {
        self.publish(catalog, true);
        if let Some(live) = self.live.take() {
            if self.received_data {
                match live.finish() {
                    Ok(recording) => publish_recording(catalog, self.id, recording),
                    Err(error) => catalog.lock().unwrap().error = error.to_string(),
                }
            } else {
                drop(live);
            }
        }
        let empty_snapshot = {
            let mut state = catalog.lock().unwrap();
            let entry = state.entries.get_mut(&self.id).unwrap();
            entry
                .snapshot
                .take_if(|recording| recording.frames.is_empty())
        };
        drop(empty_snapshot);
        let measured = history_bytes(&self.directory);
        let mut state = catalog.lock().unwrap();
        if state.entries[&self.id].snapshot.is_none()
            && measured.as_ref().is_ok_and(|bytes| *bytes == 0)
        {
            state.entries.remove(&self.id);
        } else {
            match measured {
                Ok(bytes) => state.entries.get_mut(&self.id).unwrap().bytes = bytes,
                Err(error) => state.error = error.to_string(),
            }
        }
        state.active = 0;
    }
}

fn publish_recording(catalog: &Mutex<Catalog>, id: u64, recording: Recording) {
    let usage = recording.disk_usage();
    let recording = Arc::new(recording);
    let previous = {
        let mut state = catalog.lock().unwrap();
        if let Some(warning) = usage.as_ref().and_then(|u| u.warning.as_ref()) {
            state.error.clone_from(warning);
        }
        let entry = state.entries.get_mut(&id).unwrap();
        entry.bytes = usage.map_or(0, |u| u.disk_bytes);
        entry.snapshot.replace(recording)
    };
    drop(previous);
}

fn ingest(server: CaptureServer, root: &Path, catalog: &Mutex<Catalog>, abort: &Arc<AtomicBool>) {
    let mut active: Option<Incoming> = None;
    while !abort.load(Ordering::Acquire) {
        match server.receiver.recv_timeout(PUBLISH_INTERVAL) {
            Ok(ReceiverEvent::Listening(_) | ReceiverEvent::CaptureStarted { .. }) => (),
            Ok(ReceiverEvent::Connected { .. }) => {
                if let Some(previous) = active.take() {
                    previous.finish(catalog);
                }
                match Incoming::begin(root, catalog, abort) {
                    Ok(connection) => active = Some(connection),
                    Err(error) => catalog.lock().unwrap().error = error,
                }
            }
            Ok(ReceiverEvent::Data { bytes, .. }) => {
                if let Some(connection) = &mut active {
                    connection.received_data = true;
                    if let Some(live) = &mut connection.live
                        && let Err(error) = live.feed(&bytes)
                    {
                        connection.publish(catalog, true);
                        connection.live = None;
                        catalog.lock().unwrap().error = error.to_string();
                    }
                }
            }
            Ok(ReceiverEvent::Disconnected { .. }) => {
                if let Some(connection) = active.take() {
                    connection.finish(catalog);
                }
            }
            Ok(ReceiverEvent::Failed { message, .. }) => {
                if let Some(connection) = active.take() {
                    connection.finish(catalog);
                }
                catalog.lock().unwrap().error = message;
            }
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(connection) = &mut active {
            connection.publish(catalog, false);
        }
    }
    if !abort.load(Ordering::Acquire)
        && let Some(connection) = active
    {
        connection.finish(catalog);
    }
}
