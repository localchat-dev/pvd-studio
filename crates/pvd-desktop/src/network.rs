use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::i18n::Message;
use pvd_core::{
    LiveRecording, Recording,
    live::{HistoryConfig, SceneUpdate},
    metrics::{Metrics, MetricsSnapshot},
};
use pvd_transport::{
    CaptureControl, CaptureServer, ListenConfig, ReceiverEvent, SpoolOptions, SpoolSnapshot,
    SpoolStats,
};

static CAPTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static LIVE_WORKERS: AtomicUsize = AtomicUsize::new(0);
const MAX_LIVE_WORKERS: usize = 32;

struct WorkerPermit;

impl WorkerPermit {
    fn reserve() -> io::Result<Self> {
        LIVE_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_LIVE_WORKERS).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| io::Error::new(
                io::ErrorKind::WouldBlock,
                "previous live workers are still releasing resources; retry when cleanup finishes",
            ))
    }
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        LIVE_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub struct NetworkOptions {
    pub history: HistoryConfig,
    pub receive_budget_bytes: u64,
    pub refresh_hz: u32,
    pub raw_limit_bytes: u64,
    pub live_profile: bool,
    #[cfg(test)]
    pub progress_only: bool,
}
impl Default for NetworkOptions {
    fn default() -> Self {
        Self {
            history: HistoryConfig::default(),
            receive_budget_bytes: 0,
            refresh_hz: 60,
            raw_limit_bytes: 0,
            live_profile: false,
            #[cfg(test)]
            progress_only: false,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LiveProfileSnapshot {
    pub session: u64,
    pub elapsed_seconds: f64,
    pub completed: bool,
    pub capture_failed: bool,
    pub decoder_input_bytes: u64,
    pub receive_session: Option<u64>,
    pub received_bytes: Option<u64>,
    pub queued_bytes: Option<u64>,
    pub retained_disk_bytes: Option<u64>,
    pub metrics: MetricsSnapshot,
}

struct LiveProfile {
    session: u64,
    started: Instant,
    elapsed_ns: AtomicU64,
    completed: AtomicBool,
    capture_failed: AtomicBool,
    decoder_input_bytes: AtomicU64,
    metrics: Metrics,
}

impl LiveProfile {
    fn new(session: u64, metrics: Metrics) -> Self {
        Self {
            session,
            started: Instant::now(),
            elapsed_ns: AtomicU64::new(0),
            completed: AtomicBool::new(false),
            capture_failed: AtomicBool::new(false),
            decoder_input_bytes: AtomicU64::new(0),
            metrics,
        }
    }

    fn finish(&self, failed: bool) {
        self.elapsed_ns.store(
            self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
        self.capture_failed.store(failed, Ordering::Relaxed);
        self.completed.store(true, Ordering::Release);
    }

    fn snapshot(&self, receive: Option<SpoolSnapshot>) -> LiveProfileSnapshot {
        let completed = self.completed.load(Ordering::Acquire);
        LiveProfileSnapshot {
            session: self.session,
            elapsed_seconds: if completed {
                self.elapsed_ns.load(Ordering::Relaxed) as f64 / 1_000_000_000.0
            } else {
                self.started.elapsed().as_secs_f64()
            },
            completed,
            capture_failed: self.capture_failed.load(Ordering::Relaxed),
            decoder_input_bytes: self.decoder_input_bytes.load(Ordering::Relaxed),
            receive_session: receive.as_ref().map(|status| status.session),
            received_bytes: receive
                .as_ref()
                .filter(|status| status.session == self.session)
                .map(|status| status.received_bytes),
            queued_bytes: receive.as_ref().map(|status| status.queued_bytes),
            retained_disk_bytes: receive.as_ref().map(|status| status.retained_disk_bytes),
            metrics: self.metrics.snapshot(),
        }
    }
}

pub struct LiveSnapshot {
    pub session: u64,
    pub recording: Recording,
    pub update: SceneUpdate,
    pub frame: usize,
    pub source_frame: Option<(
        pvd_core::frame_sources::FrameSourceKey,
        pvd_core::frame_sources::SourceFrame,
    )>,
    pub display_updated: bool,
}
#[cfg(test)]
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum PendingSnapshotProgress {
    Busy,
    Poisoned,
    Empty,
    Some {
        session: u64,
        frame: usize,
        latest_committed_in_snapshot: Option<usize>,
    },
}
pub enum LiveEvent {
    Status(Message),
    Connected(u64),
    CaptureStarted {
        session: u64,
        path: PathBuf,
    },
    Empty {
        session: u64,
    },
    Complete {
        session: u64,
        received_bytes: u64,
        result: Box<Result<Recording, String>>,
        capture: Option<PathBuf>,
    },
    Error(Message),
    Stopped,
}

struct RawCapture {
    writer: Option<BufWriter<File>>,
    path: PathBuf,
    complete_path: PathBuf,
    written: u64,
    limit: u64,
    reserve: u64,
}
impl RawCapture {
    fn create(directory: &Path, session: u64, options: &NetworkOptions) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let basename = format!(
            "capture-{stamp}-{}-{session}-{sequence}.pvdstream",
            std::process::id()
        );
        let complete_path = directory.join(&basename);
        let path = directory.join(format!("{basename}.partial"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            writer: Some(BufWriter::with_capacity(64 * 1024, file)),
            path,
            complete_path,
            written: 0,
            limit: options.raw_limit_bytes,
            reserve: options.history.min_free_disk_bytes,
        })
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        if self.writer.is_none() {
            return Ok(());
        }
        let result = self.write_inner(bytes);
        if result.is_err()
            && let Some(mut writer) = self.writer.take()
        {
            let _ = writer.flush();
        }
        result
    }
    fn write_inner(&mut self, bytes: &[u8]) -> Result<(), String> {
        let next = self
            .written
            .checked_add(bytes.len() as u64)
            .ok_or("raw capture size overflow")?;
        if self.limit != 0 && next > self.limit {
            return Err(format!(
                "configured raw recording capacity reached ({} bytes)",
                self.limit
            ));
        }
        let free =
            pvd_core::live::available_disk_space(self.path.parent().unwrap_or(Path::new(".")))
                .map_err(|e| e.to_string())?;
        let buffered = self
            .writer
            .as_ref()
            .map_or(0, |writer| writer.buffer().len()) as u64;
        let required = self
            .reserve
            .saturating_add(buffered)
            .saturating_add(bytes.len() as u64);
        if free.is_some_and(|free| free < required) {
            return Err(format!(
                "disk free space is below the {} MiB reserve",
                self.reserve >> 20
            ));
        }
        self.writer
            .as_mut()
            .expect("active raw writer")
            .write_all(bytes)
            .map_err(|e| e.to_string())?;
        self.written = next;
        Ok(())
    }
    fn finish(mut self, complete: bool) -> (PathBuf, Option<String>) {
        let Some(mut writer) = self.writer.take() else {
            return (self.path, None);
        };
        let result = writer.flush().and_then(|()| writer.get_ref().sync_all());
        drop(writer);
        if let Err(error) = result {
            return (self.path, Some(error.to_string()));
        }
        if complete {
            match fs::rename(&self.path, &self.complete_path) {
                Ok(()) => (self.complete_path, None),
                Err(error) => (self.path, Some(error.to_string())),
            }
        } else {
            (self.path, None)
        }
    }
}

#[derive(Clone, Copy, Default)]
struct FrameSelection {
    session: Option<u64>,
    source: Option<pvd_core::frame_sources::FrameSourceKey>,
    revision: u64,
}

struct LiveClient {
    session: u64,
    peer: String,
    live: Option<LiveRecording>,
    failure: Option<String>,
    raw: Option<RawCapture>,
    raw_attempted: bool,
    received_bytes: u64,
    published_frames: usize,
    selection_revision: u64,
    last_publish: Instant,
    profile: Option<Arc<LiveProfile>>,
}
struct WorkerOutput {
    sender: mpsc::SyncSender<LiveEvent>,
    cancel: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    context: egui::Context,
    snapshot: Arc<Mutex<Option<LiveSnapshot>>>,
    frame_source: Arc<Mutex<FrameSelection>>,
}
impl WorkerOutput {
    fn begin_frame_session(&self, session: u64) {
        *self.frame_source.lock().unwrap() = FrameSelection {
            session: Some(session),
            ..Default::default()
        };
    }

    fn emit(&self, mut event: LiveEvent) -> bool {
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return false;
            }
            if self.stop.load(Ordering::Relaxed) && matches!(event, LiveEvent::Status(_)) {
                return true;
            }
            match self.sender.try_send(event) {
                Ok(()) => {
                    self.context.request_repaint();
                    return true;
                }
                Err(mpsc::TrySendError::Full(value)) => {
                    event = value;
                    std::thread::park_timeout(Duration::from_millis(8));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return false,
            }
        }
    }
    fn publish(&self, client: &mut LiveClient) {
        let Some(live) = client.live.as_mut() else {
            return;
        };
        if live.frame_count() == 0 || live.frame_count() == client.published_frames {
            return;
        }
        let selection = self.frame_source.lock().unwrap();
        if selection.session != Some(client.session) {
            return;
        }
        let display_current = selection.revision == client.selection_revision
            && selection
                .source
                .is_none_or(|source| live.selected_frame_source() == Some(source));
        let Ok(recording) = live.snapshot() else {
            return;
        };
        let source_frame = live.display_frame_source();
        let display = display_current.then(|| live.take_update()).flatten();
        let display_updated = display.is_some();
        let (frame, update) = display.unwrap_or_else(|| {
            (
                source_frame.map_or(0, |(_, frame)| frame.index),
                SceneUpdate::default(),
            )
        });
        client.published_frames = live.frame_count();
        client.last_publish = Instant::now();
        let mut slot = self.snapshot.lock().unwrap();
        if let Some(previous) = slot
            .as_mut()
            .filter(|previous| previous.session == client.session)
        {
            if display_updated {
                previous.update.merge(update);
                previous.frame = frame;
                previous.source_frame = source_frame;
                previous.display_updated = true;
            }
            previous.recording = recording;
        } else {
            *slot = Some(LiveSnapshot {
                session: client.session,
                recording,
                update,
                frame,
                source_frame,
                display_updated,
            });
        }
        drop(slot);
        self.context.request_repaint();
    }
    fn sync_frame_source(&self, client: &mut LiveClient) {
        if let Some(live) = client.live.as_mut() {
            let selection = *self.frame_source.lock().unwrap();
            if selection.session == Some(client.session)
                && client.selection_revision != selection.revision
            {
                live.set_frame_source(selection.source);
                live.refresh_frame_source();
                client.selection_revision = selection.revision;
            }
        }
    }
    fn raw_error(&self, detail: impl std::fmt::Display) {
        self.emit(LiveEvent::Error(Message::new(
            format!("Raw recording stopped; live decoding continues: {detail}"),
            format!("原始录制保存已停止，实时解码继续：{detail}"),
        )));
    }
    fn complete(&self, mut client: LiveClient, clean_disconnect: bool) {
        if client.received_bytes == 0 && client.failure.is_none() {
            if let Some(profile) = &client.profile {
                profile.finish(false);
            }
            self.emit(LiveEvent::Empty {
                session: client.session,
            });
            return;
        }
        self.publish(&mut client);
        let result = match client.failure {
            Some(error) => Err(error),
            None => client
                .live
                .ok_or_else(|| "live decoder unavailable".to_string())
                .and_then(|live| live.finish().map_err(|e| e.to_string())),
        };
        let capture = client.raw.take().map(|raw| {
            let (path, error) = raw.finish(clean_disconnect && result.is_ok());
            if let Some(error) = error {
                self.raw_error(error);
            }
            path
        });
        if let Some(profile) = &client.profile {
            profile.finish(result.is_err());
        }
        self.emit(LiveEvent::Complete {
            session: client.session,
            received_bytes: client.received_bytes,
            result: Box::new(result),
            capture,
        });
    }
}

pub struct NetworkSession {
    pub receiver: Mutex<mpsc::Receiver<LiveEvent>>,
    snapshot: Arc<Mutex<Option<LiveSnapshot>>>,
    frame_source: Arc<Mutex<FrameSelection>>,
    cancel: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    address: std::net::SocketAddr,
    receive_stats: Option<Arc<SpoolStats>>,
    receive_control: Option<CaptureControl>,
    profile: Arc<Mutex<Option<Arc<LiveProfile>>>>,
}
impl NetworkSession {
    #[cfg(test)]
    pub(crate) fn test_session(snapshot: LiveSnapshot) -> (Self, mpsc::SyncSender<LiveEvent>) {
        let (sender, receiver) = mpsc::sync_channel(4);
        let session = snapshot.session;
        (
            Self {
                receiver: Mutex::new(receiver),
                snapshot: Arc::new(Mutex::new(Some(snapshot))),
                frame_source: Arc::new(Mutex::new(FrameSelection {
                    session: Some(session),
                    ..Default::default()
                })),
                cancel: Arc::new(AtomicBool::new(false)),
                stop: Arc::new(AtomicBool::new(false)),
                worker: None,
                address: "127.0.0.1:0".parse().unwrap(),
                receive_stats: None,
                receive_control: None,
                profile: Arc::default(),
            },
            sender,
        )
    }
    #[cfg(test)]
    pub fn start(config: ListenConfig, context: egui::Context) -> io::Result<Self> {
        Self::start_with_options(config, NetworkOptions::default(), context)
    }
    #[cfg(test)]
    pub fn start_with_options(
        config: ListenConfig,
        options: NetworkOptions,
        context: egui::Context,
    ) -> io::Result<Self> {
        Self::start_tracked(
            config,
            options,
            context,
            &crate::background_jobs::BackgroundJobs::default(),
        )
    }
    pub fn start_tracked(
        mut config: ListenConfig,
        options: NetworkOptions,
        context: egui::Context,
        jobs: &crate::background_jobs::BackgroundJobs,
    ) -> io::Result<Self> {
        #[cfg(test)]
        if options.progress_only && options.live_profile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "progress-only diagnostics cannot enable live timings",
            ));
        }
        let permit = WorkerPermit::reserve()?;
        let capture_directory = config.capture_directory.take();
        if config.max_session_bytes == ListenConfig::default().max_session_bytes {
            config.max_session_bytes = u64::MAX;
        }
        let receive_directory = options.history.directory.join("receive");
        let space_directory = receive_directory.clone();
        let reserve = options.history.min_free_disk_bytes;
        let wake_context = context.clone();
        let server = CaptureServer::bind_spooled(
            config,
            SpoolOptions {
                directory: receive_directory,
                max_backlog_bytes: options.receive_budget_bytes,
                space_check: Some(Arc::new(move |bytes| {
                    let free = pvd_core::live::available_disk_space(&space_directory)
                        .map_err(io::Error::other)?
                        .ok_or_else(|| {
                            io::Error::other("receive disk free space is unavailable")
                        })?;
                    if free < reserve.saturating_add(bytes) {
                        return Err(io::Error::other(format!(
                            "receive spool disk free space is below the {} MiB reserve; connection closed",
                            reserve >> 20
                        )));
                    }
                    Ok(())
                })),
                ..Default::default()
            },
            move || wake_context.request_repaint(),
        )?;
        let receive_stats = server.spool_stats();
        let receive_control = Some(server.control());
        let address = server.address();
        let (sender, receiver) = mpsc::sync_channel(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(Mutex::new(None));
        let frame_source = Arc::new(Mutex::new(FrameSelection::default()));
        let profile = Arc::new(Mutex::new(None));
        let worker_profile = profile.clone();
        let output = WorkerOutput {
            sender,
            cancel: cancel.clone(),
            stop: stop.clone(),
            context,
            snapshot: snapshot.clone(),
            frame_source: frame_source.clone(),
        };
        let shutdown = cancel.clone();
        let stop_receiver = receive_control.clone();
        let worker = jobs.spawn_with_cancel(
            "pvd-live-indexer",
            move || {
                shutdown.store(true, Ordering::Release);
                if let Some(control) = &stop_receiver {
                    control.request_stop();
                }
            },
            move || {
                let _permit = permit;
                let mut server = server;
                let output = output;
                let interval =
                    Duration::from_secs_f64(1.0 / f64::from(options.refresh_hz.clamp(1, 120)));
                let mut active = None::<LiveClient>;
                let mut stopping = false;
                while !output.cancel.load(Ordering::Relaxed) {
                    if output.stop.load(Ordering::Relaxed) && !stopping {
                        stopping = true;
                        server.stop();
                    }
                    if let Some(client) = active.as_mut()
                        && client.last_publish.elapsed() >= interval
                    {
                        output.publish(client);
                    }
                    let event = match server.receiver.recv_timeout(Duration::from_millis(8)) {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    match event {
                        ReceiverEvent::Listening(address) => {
                            output.emit(LiveEvent::Status(Message::new(
                                format!("Listening · {address}"),
                                format!("监听中 · {address}"),
                            )));
                        }
                        ReceiverEvent::Connected { session, peer } => {
                            let (mut live, failure) =
                                match LiveRecording::with_history(options.history.clone()) {
                                    Ok(live) => (Some(live), None),
                                    Err(error) => (None, Some(error.to_string())),
                                };
                            let profile = options.live_profile.then(|| {
                                Arc::new(LiveProfile::new(
                                    session,
                                    live.as_mut()
                                        .map(LiveRecording::enable_metrics)
                                        .unwrap_or_default(),
                                ))
                            });
                            #[cfg(test)]
                            let profile = if options.progress_only {
                                Some(Arc::new(LiveProfile::new(
                                    session,
                                    live.as_mut()
                                        .map(LiveRecording::enable_progress_metrics)
                                        .unwrap_or_default(),
                                )))
                            } else {
                                profile
                            };
                            if options.live_profile {
                                *worker_profile.lock().unwrap() = profile.clone();
                            }
                            #[cfg(test)]
                            if options.progress_only {
                                *worker_profile.lock().unwrap() = profile.clone();
                            }
                            output.begin_frame_session(session);
                            output.emit(LiveEvent::Connected(session));
                            if let Some(error) = &failure {
                                output.emit(LiveEvent::Error(Message::new(
                                    format!("Live decoder initialization failed: {error}"),
                                    format!("实时解码器初始化失败：{error}"),
                                )));
                            }
                            output.emit(LiveEvent::Status(if failure.is_some() {
                                Message::new(
                                    format!("Receiving raw data only · decoding stopped · {peer}"),
                                    format!("仅接收原始数据 · 解码已停止 · {peer}"),
                                )
                            } else {
                                Message::new(
                                    format!("Connected · waiting for PVD data · {peer}"),
                                    format!("已连接 · 等待 PVD 数据 · {peer}"),
                                )
                            }));
                            active = Some(LiveClient {
                                session,
                                peer: peer.to_string(),
                                live,
                                failure,
                                raw: None,
                                raw_attempted: false,
                                received_bytes: 0,
                                published_frames: 0,
                                selection_revision: 0,
                                last_publish: Instant::now() - interval,
                                profile,
                            });
                        }
                        ReceiverEvent::Data { session, bytes } => {
                            let Some(client) =
                                active.as_mut().filter(|client| client.session == session)
                            else {
                                continue;
                            };
                            if !client.raw_attempted && !bytes.is_empty() {
                                client.raw_attempted = true;
                                if client.failure.is_none() {
                                    output.emit(LiveEvent::Status(Message::new(
                                        format!("Receiving · {}", client.peer),
                                        format!("接收中 · {}", client.peer),
                                    )));
                                }
                                client.raw = capture_directory.as_ref().and_then(|directory| {
                                    match RawCapture::create(directory, session, &options) {
                                        Ok(raw) => {
                                            output.emit(LiveEvent::CaptureStarted {
                                                session,
                                                path: raw.path.clone(),
                                            });
                                            Some(raw)
                                        }
                                        Err(error) => {
                                            output.raw_error(error);
                                            None
                                        }
                                    }
                                });
                            }
                            client.received_bytes =
                                client.received_bytes.saturating_add(bytes.len() as u64);
                            if let Some(profile) = &client.profile
                                && client.live.is_some()
                            {
                                profile
                                    .decoder_input_bytes
                                    .store(client.received_bytes, Ordering::Relaxed);
                            }
                            if let Some(raw) = client.raw.as_mut()
                                && let Err(error) = raw.write(&bytes)
                            {
                                output.raw_error(error);
                            }
                            output.sync_frame_source(client);
                            if let Some(live) = client.live.as_mut()
                                && let Err(error) = live.feed(&bytes)
                            {
                                client.failure = Some(error.to_string());
                                if let Some(profile) = &client.profile {
                                    profile.capture_failed.store(true, Ordering::Relaxed);
                                }
                                output.publish(client);
                                client.live = None;
                                output.emit(LiveEvent::Status(Message::new(
                                    format!(
                                        "Receiving raw data only · decoding stopped · {}",
                                        client.peer
                                    ),
                                    format!("仅接收原始数据 · 解码已停止 · {}", client.peer),
                                )));
                                output.emit(LiveEvent::Error(Message::new(
                                    format!("Live decoding stopped: {error}"),
                                    format!("实时解码已停止：{error}"),
                                )));
                            }
                        }
                        ReceiverEvent::CaptureStarted { .. } => {}
                        ReceiverEvent::Disconnected { session, bytes, .. } => {
                            if active
                                .as_ref()
                                .is_some_and(|client| client.session == session)
                            {
                                let mut client = active.take().unwrap();
                                if client.received_bytes != bytes && client.failure.is_none() {
                                    client.failure = Some(format!(
                                        "TCP byte count mismatch: transport received {bytes}, decoder received {}",
                                        client.received_bytes
                                    ));
                                }
                                output.complete(client, true);
                            }
                            if !stopping {
                                output.emit(LiveEvent::Status(Message::new(
                                    format!("Listening · {address} · client disconnected"),
                                    format!("监听中 · {address} · 客户端已断开"),
                                )));
                            }
                        }
                        ReceiverEvent::Failed { session, message } => {
                            output.emit(LiveEvent::Error(Message::new(
                                format!("Capture failed: {message}"),
                                format!("捕获失败：{message}"),
                            )));
                            if active
                                .as_ref()
                                .is_some_and(|client| session.is_none_or(|id| client.session == id))
                            {
                                let mut client = active.take().unwrap();
                                if client.failure.is_none() {
                                    client.failure = Some(message);
                                }
                                output.complete(client, false);
                                if !stopping {
                                    output.emit(LiveEvent::Status(Message::new(
                                        format!("Listening · {address} · previous client failed"),
                                        format!("监听中 · {address} · 上一个客户端连接失败"),
                                    )));
                                }
                            }
                        }
                    }
                }
                if let Some(client) = active.take()
                    && !output.cancel.load(Ordering::Relaxed)
                {
                    output.complete(client, false);
                }
                drop(server);
                output.emit(LiveEvent::Stopped);
            },
        )?;
        Ok(Self {
            receiver: Mutex::new(receiver),
            snapshot,
            frame_source,
            cancel,
            stop,
            worker: Some(worker),
            address,
            receive_stats,
            receive_control,
            profile,
        })
    }
    pub fn set_frame_source(
        &self,
        session: u64,
        source: Option<pvd_core::frame_sources::FrameSourceKey>,
    ) -> bool {
        let mut selected = self.frame_source.lock().unwrap();
        if selected.session != Some(session) {
            return false;
        }
        if selected.source != source {
            selected.source = source;
            selected.revision = selected.revision.wrapping_add(1);
            let mut snapshot = self.snapshot.lock().unwrap();
            if snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.session == session)
            {
                snapshot.take();
            }
        }
        true
    }
    #[cfg(test)]
    pub fn take_snapshot(&self) -> Option<LiveSnapshot> {
        self.snapshot.lock().unwrap().take()
    }
    #[cfg(test)]
    pub(crate) fn pending_snapshot_progress(&self) -> PendingSnapshotProgress {
        use std::sync::TryLockError;
        match self.snapshot.try_lock() {
            Ok(slot) => match slot.as_ref() {
                Some(snapshot) => PendingSnapshotProgress::Some {
                    session: snapshot.session,
                    frame: snapshot.frame,
                    latest_committed_in_snapshot: snapshot
                        .recording
                        .frames
                        .last()
                        .map(|info| info.index),
                },
                None => PendingSnapshotProgress::Empty,
            },
            Err(TryLockError::WouldBlock) => PendingSnapshotProgress::Busy,
            Err(TryLockError::Poisoned(_)) => PendingSnapshotProgress::Poisoned,
        }
    }
    pub fn take_snapshot_for(&self, session: u64) -> Option<LiveSnapshot> {
        let mut slot = self.snapshot.lock().unwrap();
        if slot
            .as_ref()
            .is_some_and(|snapshot| snapshot.session == session)
        {
            slot.take()
        } else {
            None
        }
    }
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(control) = &self.receive_control {
            control.request_stop();
        }
    }
    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }
    pub fn receive_status(&self) -> Option<SpoolSnapshot> {
        self.receive_stats.as_ref().map(|stats| stats.snapshot())
    }
    pub fn profile_snapshot(&self) -> Option<LiveProfileSnapshot> {
        let profile = self.profile.lock().unwrap().clone()?;
        Some(profile.snapshot(self.receive_status()))
    }
}
impl Drop for NetworkSession {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(control) = &self.receive_control {
            control.request_stop();
        }
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            // Disk writes may outlive the view. The worker owns its resources and shutdown permit.
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    mod frame_sources;
    use super::*;
    use std::{io::Write, net::TcpStream};

    fn test_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pvd-network-{name}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            CAPTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    struct TestEnvironment {
        jobs: crate::background_jobs::BackgroundJobs,
        history: PathBuf,
    }

    impl TestEnvironment {
        fn new() -> Self {
            Self {
                jobs: crate::background_jobs::BackgroundJobs::default(),
                history: test_directory("history"),
            }
        }

        fn start(
            &self,
            config: ListenConfig,
            context: egui::Context,
        ) -> io::Result<NetworkSession> {
            self.start_with_options(config, NetworkOptions::default(), context)
        }

        fn start_with_options(
            &self,
            config: ListenConfig,
            mut options: NetworkOptions,
            context: egui::Context,
        ) -> io::Result<NetworkSession> {
            options.history.directory = self.history.clone();
            NetworkSession::start_tracked(config, options, context, &self.jobs)
        }
    }

    impl Drop for TestEnvironment {
        fn drop(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.jobs.cancel_all_and_check_finished() {
                assert!(
                    Instant::now() < deadline,
                    "test network jobs did not finish"
                );
                std::thread::park_timeout(Duration::from_millis(1));
            }
            assert_eq!(self.jobs.active_count(), 0);
            let mut directories = vec![self.history.clone()];
            let mut files = Vec::new();
            let mut position = 0;
            while position < directories.len() {
                let path = &directories[position];
                if path.exists() {
                    for entry in fs::read_dir(path).unwrap() {
                        let entry = entry.unwrap();
                        if entry.file_type().unwrap().is_dir() {
                            directories.push(entry.path());
                        } else {
                            files.push(entry.path());
                        }
                    }
                }
                position += 1;
            }
            assert!(
                files.is_empty(),
                "test history cache files remain: {files:?}"
            );
            for path in directories.into_iter().rev() {
                if path.exists() {
                    fs::remove_dir(path).unwrap();
                }
            }
        }
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn raw_space_reserve_is_checked_again_before_the_next_write() {
        let directory = test_directory("space-recheck");
        let mut raw = RawCapture::create(&directory, 1, &NetworkOptions::default()).unwrap();
        raw.write(b"accepted-prefix").unwrap();
        raw.reserve = u64::MAX;
        assert!(
            raw.write(b"must-not-be-written")
                .unwrap_err()
                .contains("reserve")
        );
        assert!(raw.writer.is_none());
        let (path, error) = raw.finish(true);
        assert!(error.is_none());
        assert_eq!(path.extension().unwrap(), "partial");
        assert_eq!(fs::read(&path).unwrap(), b"accepted-prefix");
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    fn frame_group(timestamp: u64) -> Vec<u8> {
        let mut events = vec![1];
        events.extend(6u32.to_le_bytes());
        events.extend(b"frame\0");
        events.extend(1u32.to_le_bytes());
        events.push(19);
        events.extend(7u64.to_le_bytes());
        events.extend(1u32.to_le_bytes());
        events.extend(timestamp.to_le_bytes());
        let mut group = Vec::new();
        group.extend((events.len() as u32).to_le_bytes());
        group.extend(2u32.to_le_bytes());
        group.extend(3u64.to_le_bytes());
        group.extend(timestamp.to_le_bytes());
        group.extend(events);
        group
    }

    #[test]
    fn raw_quota_does_not_stop_live_frames_and_held_snapshots_coalesce() {
        let environment = TestEnvironment::new();
        let directory = test_directory("quota");
        let session = environment
            .start_with_options(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(directory.clone()),
                    ..Default::default()
                },
                NetworkOptions {
                    raw_limit_bytes: 32,
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let address = session.address();
        let producer = std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client.write_all(&empty_stream_header()).unwrap();
            let mut total = empty_stream_header().len();
            for timestamp in 0..32 {
                let group = frame_group(timestamp);
                total += group.len();
                client.write_all(&group).unwrap();
                std::thread::sleep(Duration::from_millis(10));
            }
            total
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut warning = false;
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Error(message) => {
                    assert!(
                        message
                            .text(crate::i18n::Language::English)
                            .contains("Raw recording stopped; live decoding continues")
                    );
                    warning = true;
                }
                LiveEvent::Complete {
                    received_bytes,
                    result,
                    capture,
                    ..
                } => {
                    assert!(warning);
                    assert_eq!(result.unwrap().frame_count(), 32);
                    assert_eq!(received_bytes, producer.join().unwrap() as u64);
                    assert!(session.take_snapshot_for(999).is_none());
                    let snapshot = session.take_snapshot_for(1).unwrap();
                    assert_eq!(snapshot.frame, 31);
                    assert!(snapshot.update.replace_all);
                    assert!(session.take_snapshot().is_none());
                    let capture = capture.unwrap();
                    assert_eq!(capture.extension().unwrap(), "partial");
                    assert!(fs::metadata(&capture).unwrap().len() <= 32);
                    fs::remove_file(capture).unwrap();
                    break;
                }
                _ => {}
            }
        }
        drop(session);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn live_refresh_publishes_without_a_half_second_gate() {
        let environment = TestEnvironment::new();
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let address = session.address();
        let producer = std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client.write_all(&empty_stream_header()).unwrap();
            for timestamp in 0..24 {
                client.write_all(&frame_group(timestamp)).unwrap();
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut snapshots = 0;
        let mut last = None;
        loop {
            assert!(Instant::now() < deadline);
            if let Some(snapshot) = session.take_snapshot() {
                snapshots += 1;
                last = Some(snapshot.frame);
            }
            match session
                .receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(8))
            {
                Ok(LiveEvent::Complete { result, .. }) => {
                    assert_eq!(result.unwrap().frame_count(), 24);
                    if let Some(snapshot) = session.take_snapshot() {
                        snapshots += 1;
                        last = Some(snapshot.frame);
                    }
                    assert_eq!(last, Some(23));
                    assert!(
                        snapshots >= 4,
                        "only {snapshots} display updates across 480 ms of input"
                    );
                    break;
                }
                Ok(LiveEvent::Error(error)) => {
                    panic!("{}", error.text(crate::i18n::Language::English))
                }
                _ => {}
            }
        }
        producer.join().unwrap();
    }

    #[test]
    fn unavailable_raw_output_keeps_decoding_and_exposes_the_save_error() {
        let environment = TestEnvironment::new();
        let directory = test_directory("unavailable");
        fs::create_dir_all(&directory).unwrap();
        let not_directory = directory.join("file");
        fs::write(&not_directory, b"occupied").unwrap();
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(not_directory.clone()),
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let mut bytes = empty_stream_header();
        bytes.extend(frame_group(5));
        TcpStream::connect(session.address())
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_error = false;
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Error(error) => {
                    assert!(
                        error
                            .text(crate::i18n::Language::English)
                            .contains("live decoding continues")
                    );
                    saw_error = true;
                }
                LiveEvent::Complete {
                    result,
                    capture,
                    received_bytes,
                    ..
                } => {
                    assert!(saw_error);
                    assert!(capture.is_none());
                    assert_eq!(received_bytes, bytes.len() as u64);
                    assert_eq!(result.unwrap().frame_count(), 1);
                    assert_eq!(session.take_snapshot().unwrap().frame, 0);
                    break;
                }
                _ => {}
            }
        }
        drop(session);
        fs::remove_file(not_directory).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    fn next_event(session: &NetworkSession, deadline: Instant) -> LiveEvent {
        loop {
            assert!(Instant::now() < deadline, "network event did not arrive");
            match session
                .receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(100))
            {
                Ok(event) => return event,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => panic!("network event channel closed: {error}"),
            }
        }
    }

    fn empty_stream_header() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(10u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes
    }

    #[test]
    fn empty_tcp_session_has_no_capture_or_decode_failure() {
        for live_profile in [false, true] {
            let environment = TestEnvironment::new();
            let directory = test_directory("empty");
            let session = environment
                .start_with_options(
                    ListenConfig {
                        address: "127.0.0.1:0".parse().unwrap(),
                        capture_directory: Some(directory.clone()),
                        ..Default::default()
                    },
                    NetworkOptions {
                        live_profile,
                        ..Default::default()
                    },
                    egui::Context::default(),
                )
                .unwrap();
            drop(TcpStream::connect(session.address()).unwrap());
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut connected = false;
            let mut waiting = false;
            loop {
                match next_event(&session, deadline) {
                    LiveEvent::Connected(id) => {
                        assert_eq!(id, 1);
                        connected = true;
                    }
                    LiveEvent::Status(message) => {
                        waiting |= message
                            .text(crate::i18n::Language::English)
                            .contains("waiting for PVD data");
                    }
                    LiveEvent::Empty { session: id } => {
                        assert_eq!(id, 1);
                        break;
                    }
                    LiveEvent::Error(message) => panic!(
                        "empty connection failed: {}",
                        message.text(crate::i18n::Language::English)
                    ),
                    LiveEvent::Complete { .. } | LiveEvent::CaptureStarted { .. } => {
                        panic!("empty connection must not produce a capture")
                    }
                    LiveEvent::Stopped => panic!("empty connection stopped the listener"),
                }
            }
            assert!(connected && waiting);
            assert!(session.take_snapshot().is_none());
            assert_eq!(session.receive_status().unwrap().received_bytes, 0);
            assert!(!directory.exists());
            if let Some(profile) = session.profile_snapshot() {
                assert!(live_profile);
                assert!(profile.completed);
                assert!(!profile.capture_failed);
                assert_eq!(profile.decoder_input_bytes, 0);
                assert_eq!(profile.received_bytes, Some(0));
            } else {
                assert!(!live_profile);
            }
        }
    }

    #[test]
    fn stopping_an_empty_tcp_session_does_not_report_truncation() {
        let environment = TestEnvironment::new();
        let directory = test_directory("empty-stop");
        let session = environment
            .start_with_options(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(directory.clone()),
                    ..Default::default()
                },
                NetworkOptions {
                    live_profile: true,
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let client = TcpStream::connect(session.address()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let LiveEvent::Status(message) = next_event(&session, deadline)
                && message
                    .text(crate::i18n::Language::English)
                    .contains("waiting for PVD data")
            {
                break;
            }
        }
        session.request_stop();
        let mut empty = false;
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Empty { session: id } => {
                    assert_eq!(id, 1);
                    empty = true;
                }
                LiveEvent::Stopped => break,
                LiveEvent::Error(_) | LiveEvent::Complete { .. } => {
                    panic!("stopping before the first byte must not fail decoding")
                }
                LiveEvent::CaptureStarted { .. } => panic!("no bytes were recorded"),
                _ => {}
            }
        }
        assert!(empty);
        assert!(!directory.exists());
        assert!(!session.profile_snapshot().unwrap().capture_failed);
        drop(client);
    }

    #[test]
    fn empty_truncated_and_valid_tcp_sessions_remain_distinct() {
        let environment = TestEnvironment::new();
        let directory = test_directory("empty-reconnect");
        let session = environment
            .start_with_options(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(directory.clone()),
                    ..Default::default()
                },
                NetworkOptions {
                    live_profile: true,
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let header = empty_stream_header();
        let mut frame = header.clone();
        frame.extend(frame_group(1));
        let cases = [
            Vec::new(),
            header[..1].to_vec(),
            header[..header.len() - 1].to_vec(),
            header.clone(),
            frame,
        ];
        assert!(Recording::from_bytes(&[]).is_err());
        for (index, bytes) in cases.iter().enumerate() {
            let id = index as u64 + 1;
            let mut client = TcpStream::connect(session.address()).unwrap();
            client.write_all(bytes).unwrap();
            drop(client);
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match next_event(&session, deadline) {
                    LiveEvent::Empty { session: observed } => {
                        assert_eq!(observed, id);
                        assert!(bytes.is_empty());
                        assert!(!directory.exists());
                        break;
                    }
                    LiveEvent::Complete {
                        session: observed,
                        received_bytes,
                        result,
                        capture,
                    } => {
                        assert_eq!(observed, id);
                        assert_eq!(received_bytes, bytes.len() as u64);
                        assert!(!bytes.is_empty());
                        let path = capture.unwrap();
                        assert_eq!(fs::read(&path).unwrap(), *bytes);
                        if bytes.len() < header.len() {
                            let Err(error) = *result else {
                                panic!("nonempty truncated headers must fail")
                            };
                            assert!(error.contains("truncated stream initialization"));
                            assert_eq!(path.extension().unwrap(), "partial");
                        } else {
                            let recording = result.unwrap();
                            assert_eq!(recording.frame_count(), usize::from(index == 4));
                            assert_eq!(path.extension().unwrap(), "pvdstream");
                        }
                        fs::remove_file(path).unwrap();
                        session.take_snapshot_for(id);
                        break;
                    }
                    LiveEvent::CaptureStarted {
                        session: observed, ..
                    } => {
                        assert_eq!(observed, id);
                        assert!(!bytes.is_empty());
                    }
                    LiveEvent::Error(message) => panic!(
                        "unexpected connection failure: {}",
                        message.text(crate::i18n::Language::English)
                    ),
                    LiveEvent::Stopped => panic!("listener must accept the next connection"),
                    _ => {}
                }
            }
            let profile = session.profile_snapshot().unwrap();
            assert_eq!(profile.session, id);
            assert!(profile.completed);
            assert_eq!(profile.capture_failed, (1..=2).contains(&index));
            assert_eq!(profile.decoder_input_bytes, bytes.len() as u64);
            assert_eq!(profile.received_bytes, Some(bytes.len() as u64));
        }
        drop(session);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn live_profile_defaults_off_even_after_a_completed_connection() {
        let environment = TestEnvironment::new();
        assert!(!NetworkOptions::default().live_profile);
        assert!(!NetworkOptions::default().progress_only);
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        assert_eq!(session.receive_status().unwrap().limit_bytes, 0);
        assert!(session.profile_snapshot().is_none());
        TcpStream::connect(session.address())
            .unwrap()
            .write_all(&empty_stream_header())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let LiveEvent::Complete { result, .. } = next_event(&session, deadline) {
                assert!(result.is_ok());
                break;
            }
        }
        assert!(session.profile_snapshot().is_none());
    }

    #[test]
    fn pending_snapshot_progress_is_non_consuming_and_never_waits_for_the_slot() {
        let mut bytes = empty_stream_header();
        bytes.extend(frame_group(1));
        let recording = Recording::from_bytes(&bytes).unwrap();
        let (session, _sender) = NetworkSession::test_session(LiveSnapshot {
            source_frame: None,
            display_updated: true,
            session: 7,
            frame: 9,
            recording,
            update: SceneUpdate::default(),
        });
        let expected = PendingSnapshotProgress::Some {
            session: 7,
            frame: 9,
            latest_committed_in_snapshot: Some(0),
        };
        assert_eq!(session.pending_snapshot_progress(), expected);
        let held = session.snapshot.lock().unwrap();
        assert_eq!(
            session.pending_snapshot_progress(),
            PendingSnapshotProgress::Busy
        );
        drop(held);
        assert_eq!(session.pending_snapshot_progress(), expected);
        assert_eq!(session.take_snapshot().unwrap().frame, 9);
        assert_eq!(
            session.pending_snapshot_progress(),
            PendingSnapshotProgress::Empty
        );
    }

    #[test]
    fn progress_only_reports_commit_and_completed_counts_without_stage_timings() {
        let environment = TestEnvironment::new();
        let session = environment
            .start_with_options(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                NetworkOptions {
                    progress_only: true,
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let mut bytes = empty_stream_header();
        bytes.extend(frame_group(1));
        TcpStream::connect(session.address())
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let LiveEvent::Complete { result, .. } = next_event(&session, deadline) {
                assert_eq!(result.unwrap().frame_count(), 1);
                break;
            }
        }
        let snapshot = session.profile_snapshot().unwrap();
        assert!(snapshot.completed);
        assert!(snapshot.metrics.enabled);
        assert!(!snapshot.metrics.timings_enabled);
        assert_eq!(snapshot.decoder_input_bytes, bytes.len() as u64);
        assert_eq!(snapshot.metrics.last_committed_frame, Some(0));
        assert!(
            snapshot
                .metrics
                .stages
                .iter()
                .any(|stage| stage.completed > 0)
        );
        assert!(snapshot.metrics.stages.iter().all(|stage| {
            stage.calls == stage.completed
                && stage.started == stage.completed
                && stage.in_flight == 0
                && stage.inclusive_ns == 0
                && stage.self_ns == 0
                && stage.max_ns == 0
        }));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn configured_receive_disk_reserve_stops_before_spooling_data() {
        let environment = TestEnvironment::new();
        let session = environment
            .start_with_options(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                NetworkOptions {
                    history: HistoryConfig {
                        min_free_disk_bytes: u64::MAX,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let mut client = TcpStream::connect(session.address()).unwrap();
        client.write_all(&empty_stream_header()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let LiveEvent::Complete { result, .. } = next_event(&session, deadline) {
                assert!(result.unwrap_err().contains("reserve"));
                break;
            }
        }
        let status = session.receive_status().unwrap();
        assert!(!status.connected);
        assert_eq!(status.queued_bytes, 0);
        assert!(status.last_failure.unwrap().contains("reserve"));
    }

    #[test]
    fn live_profile_resets_for_new_connections_and_retains_completion() {
        let environment = TestEnvironment::new();
        let session = environment
            .start_with_options(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                NetworkOptions {
                    live_profile: true,
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        assert!(session.profile_snapshot().is_none());
        for id in 1..=2 {
            let mut client = TcpStream::connect(session.address()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let LiveEvent::Connected(connected) = next_event(&session, deadline) {
                    assert_eq!(connected, id);
                    break;
                }
            }
            let beginning = session.profile_snapshot().unwrap();
            assert_eq!(beginning.session, id);
            assert!(!beginning.completed);
            assert!(beginning.metrics.enabled);
            assert_eq!(beginning.decoder_input_bytes, 0);
            assert!(
                beginning
                    .metrics
                    .stages
                    .iter()
                    .all(|stage| stage.calls == 0)
            );
            let mut bytes = empty_stream_header();
            bytes.extend(frame_group(id));
            client.write_all(&bytes).unwrap();
            drop(client);
            loop {
                if let LiveEvent::Complete {
                    session: completed,
                    result,
                    ..
                } = next_event(&session, deadline)
                {
                    assert_eq!(completed, id);
                    assert_eq!(result.unwrap().frame_count(), 1);
                    break;
                }
            }
            let finished = session.profile_snapshot().unwrap();
            assert_eq!(finished.session, id);
            assert!(finished.completed);
            assert!(!finished.capture_failed);
            assert_eq!(finished.decoder_input_bytes, bytes.len() as u64);
            assert_eq!(finished.receive_session, Some(id));
            assert_eq!(finished.received_bytes, Some(bytes.len() as u64));
            assert!(finished.metrics.stages.iter().any(|stage| stage.calls != 0));
            assert_eq!(
                session.profile_snapshot().unwrap().elapsed_seconds,
                finished.elapsed_seconds
            );
            let json = serde_json::to_value(&finished).unwrap();
            assert_eq!(json["session"], id);
            assert!(json["metrics"]["stages"].as_array().unwrap().len() > 1);
            assert!(json.as_object().unwrap().keys().all(|key| {
                [
                    "session",
                    "elapsed_seconds",
                    "completed",
                    "capture_failed",
                    "decoder_input_bytes",
                    "receive_session",
                    "received_bytes",
                    "queued_bytes",
                    "retained_disk_bytes",
                    "metrics",
                ]
                .contains(&key.as_str())
            }));
        }
        session.request_stop();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if matches!(next_event(&session, deadline), LiveEvent::Stopped) {
                break;
            }
        }
        let stopped = session.profile_snapshot().unwrap();
        assert_eq!(stopped.session, 2);
        assert!(stopped.completed);
    }

    #[test]
    fn live_profile_never_attributes_new_socket_bytes_to_an_older_decoder() {
        let profile = LiveProfile::new(41, Metrics::default());
        let snapshot = profile.snapshot(Some(SpoolSnapshot {
            session: 42,
            received_bytes: 1234,
            queued_bytes: 123,
            retained_disk_bytes: 456,
            limit_bytes: 1024,
            connected: true,
            last_failure: None,
            failure_session: None,
        }));
        assert_eq!(snapshot.session, 41);
        assert_eq!(snapshot.receive_session, Some(42));
        assert_eq!(snapshot.received_bytes, None);
        assert_eq!(snapshot.queued_bytes, Some(123));
        assert_eq!(snapshot.retained_disk_bytes, Some(456));
    }

    fn completed_frame_then_semantic_failure() -> Vec<u8> {
        let mut events = vec![1];
        events.extend(6u32.to_le_bytes());
        events.extend(b"frame\0");
        events.extend(1u32.to_le_bytes());
        events.push(19);
        events.extend(7u64.to_le_bytes());
        events.extend(1u32.to_le_bytes());
        events.extend(10u64.to_le_bytes());
        events.push(6);
        events.extend(42u32.to_le_bytes());
        events.extend(43u32.to_le_bytes());
        events.extend(99u64.to_le_bytes());
        let mut bytes = empty_stream_header();
        bytes.extend((events.len() as u32).to_le_bytes());
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(3u64.to_le_bytes());
        bytes.extend(4u64.to_le_bytes());
        bytes.extend(events);
        bytes
    }

    #[test]
    fn live_semantic_failure_exposes_raw_only_state_retains_bytes_and_recovers() {
        let environment = TestEnvironment::new();
        let directory = std::env::temp_dir().join(format!(
            "pvd-live-failure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    capture_directory: Some(directory.clone()),
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let mut client = TcpStream::connect(session.address()).unwrap();
        let peer = client.local_addr().unwrap().to_string();
        let mut expected = completed_frame_then_semantic_failure();
        client.write_all(&expected).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_raw_only = false;
        let mut failure = None;
        let mut saw_completed_frame = false;
        while !saw_raw_only || failure.is_none() || !saw_completed_frame {
            if let Some(snapshot) = session.take_snapshot() {
                assert_eq!(snapshot.session, 1);
                assert_eq!(snapshot.recording.frame_count(), 1);
                assert!(snapshot.recording.state_at(0).unwrap().objects.is_empty());
                snapshot.recording.verify_replay().unwrap();
                saw_completed_frame = true;
                if saw_raw_only && failure.is_some() {
                    break;
                }
            }
            match next_event(&session, deadline) {
                LiveEvent::Status(message)
                    if message
                        .text(crate::i18n::Language::English)
                        .contains("decoding stopped") =>
                {
                    assert_eq!(
                        message.text(crate::i18n::Language::English),
                        format!("Receiving raw data only · decoding stopped · {peer}")
                    );
                    assert_eq!(
                        message.text(crate::i18n::Language::Chinese),
                        format!("仅接收原始数据 · 解码已停止 · {peer}")
                    );
                    saw_raw_only = true;
                }
                LiveEvent::Error(message) => failure = Some(message),
                LiveEvent::Complete { .. } | LiveEvent::Stopped => {
                    panic!("decoding failure must not close the raw capture")
                }
                _ => {}
            }
        }
        let tail = [0xff, 0, 0x17, 0x99];
        client.write_all(&tail).unwrap();
        expected.extend(tail);
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Complete {
                    session,
                    received_bytes,
                    result,
                    capture,
                } => {
                    assert_eq!(session, 1);
                    assert_eq!(received_bytes, expected.len() as u64);
                    let error = result.expect_err("failed decode must remain failed");
                    assert!(
                        failure
                            .unwrap()
                            .text(crate::i18n::Language::English)
                            .contains(&error)
                    );
                    let capture = capture.unwrap();
                    assert_eq!(std::fs::read(&capture).unwrap(), expected);
                    std::fs::remove_file(capture).unwrap();
                    break;
                }
                LiveEvent::Status(message) => assert!(
                    !message
                        .text(crate::i18n::Language::English)
                        .starts_with("Receiving ·")
                ),
                LiveEvent::Error(_) | LiveEvent::Stopped => {
                    panic!("raw capture should continue without another decode attempt")
                }
                _ => {}
            }
        }
        let header = empty_stream_header();
        TcpStream::connect(session.address())
            .unwrap()
            .write_all(&header)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_receiving = false;
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Status(message)
                    if message
                        .text(crate::i18n::Language::English)
                        .starts_with("Receiving ·") =>
                {
                    saw_receiving = true;
                }
                LiveEvent::Complete {
                    session,
                    received_bytes,
                    result,
                    capture,
                } => {
                    assert_eq!(session, 2);
                    assert!(saw_receiving);
                    assert_eq!(received_bytes, header.len() as u64);
                    assert_eq!(result.unwrap().frame_count(), 0);
                    std::fs::remove_file(capture.unwrap()).unwrap();
                    break;
                }
                LiveEvent::Error(_) | LiveEvent::Stopped => {
                    panic!("new client must recover normal capture")
                }
                _ => {}
            }
        }
        drop(session);
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn tcp_capture_limit_failure_restores_listening_and_accepts_a_new_client() {
        let environment = TestEnvironment::new();
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    max_session_bytes: 64,
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let mut oversized = empty_stream_header();
        oversized.resize(65, 0);
        TcpStream::connect(session.address())
            .unwrap()
            .write_all(&oversized)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_error = false;
        let mut saw_complete = false;
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Error(message) => {
                    assert!(
                        message
                            .text(crate::i18n::Language::English)
                            .contains("exceeds capture limit")
                    );
                    saw_error = true;
                }
                LiveEvent::Complete {
                    session: id,
                    result,
                    ..
                } => {
                    assert_eq!(id, 1);
                    assert!(result.unwrap_err().contains("exceeds capture limit"));
                    saw_complete = true;
                }
                LiveEvent::Status(message) if saw_complete => {
                    assert!(saw_error);
                    assert!(
                        message
                            .text(crate::i18n::Language::English)
                            .starts_with("Listening ·")
                    );
                    assert!(
                        message
                            .text(crate::i18n::Language::English)
                            .contains(&session.address().to_string())
                    );
                    assert!(
                        message
                            .text(crate::i18n::Language::Chinese)
                            .starts_with("监听中 ·")
                    );
                    break;
                }
                LiveEvent::Stopped => panic!("capture failure must not close the listener"),
                _ => {}
            }
        }
        let header = empty_stream_header();
        TcpStream::connect(session.address())
            .unwrap()
            .write_all(&header)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Complete {
                    session: id,
                    received_bytes,
                    result,
                    ..
                } => {
                    assert_eq!(id, 2);
                    assert_eq!(received_bytes, header.len() as u64);
                    assert_eq!(result.unwrap().frame_count(), 0);
                    break;
                }
                LiveEvent::Error(message) => panic!(
                    "second capture failed: {}",
                    message.text(crate::i18n::Language::English)
                ),
                LiveEvent::Stopped => panic!("listener stopped before second capture completed"),
                _ => {}
            }
        }
    }

    #[test]
    fn stopping_an_active_client_does_not_emit_a_listening_status() {
        let environment = TestEnvironment::new();
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let mut client = TcpStream::connect(session.address()).unwrap();
        let header = empty_stream_header();
        client.write_all(&header).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let LiveEvent::Status(message) = next_event(&session, deadline)
                && message
                    .text(crate::i18n::Language::English)
                    .starts_with("Receiving ·")
            {
                break;
            }
        }
        session.request_stop();
        drop(client);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Status(message) => assert!(
                    !message
                        .text(crate::i18n::Language::English)
                        .starts_with("Listening ·")
                ),
                LiveEvent::Stopped => break,
                _ => {}
            }
        }
    }

    #[test]
    fn dropping_an_idle_listener_or_client_cancels_without_a_ui_tick() {
        let environment = TestEnvironment::new();
        for connect in [false, true] {
            let session = environment
                .start(
                    ListenConfig {
                        address: "127.0.0.1:0".parse().unwrap(),
                        ..Default::default()
                    },
                    egui::Context::default(),
                )
                .unwrap();
            let address = session.address();
            let _client = connect.then(|| TcpStream::connect(address).unwrap());
            let started = Instant::now();
            drop(session);
            assert!(started.elapsed() < Duration::from_secs(2));
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if std::net::TcpListener::bind(address).is_ok() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "cancelled listener retained its socket"
                );
                std::thread::park_timeout(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn closing_a_session_does_not_wait_for_a_blocked_worker() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let (release, blocked) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let worker = std::thread::spawn(move || {
            let _ = blocked.recv_timeout(Duration::from_secs(3));
            assert!(worker_cancel.load(Ordering::Relaxed));
            drop(sender);
            done.send(()).unwrap();
        });
        let session = NetworkSession {
            receiver: Mutex::new(receiver),
            snapshot: Arc::new(Mutex::new(None)),
            frame_source: Arc::default(),
            cancel,
            stop: Arc::new(AtomicBool::new(false)),
            worker: Some(worker),
            address: "127.0.0.1:0".parse().unwrap(),
            receive_stats: None,
            receive_control: None,
            profile: Arc::default(),
        };
        let started = Instant::now();
        drop(session);
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(elapsed < Duration::from_secs(1), "close took {elapsed:?}");
    }

    #[test]
    fn semantic_failure_stays_failed_after_disconnect() {
        let environment = TestEnvironment::new();
        let mut bytes = Vec::new();
        bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(10u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(17u32.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(3u64.to_le_bytes());
        bytes.extend(4u64.to_le_bytes());
        bytes.push(6);
        bytes.extend(42u32.to_le_bytes());
        bytes.extend(43u32.to_le_bytes());
        bytes.extend(99u64.to_le_bytes());
        let mut decoder = pvd_protocol::Decoder::default();
        assert_eq!(decoder.feed(&bytes).unwrap().len(), 1);
        assert!(Recording::from_bytes(&bytes).is_err());
        let session = environment
            .start(
                ListenConfig {
                    address: "127.0.0.1:0".parse().unwrap(),
                    ..Default::default()
                },
                egui::Context::default(),
            )
            .unwrap();
        let expected = bytes.clone();
        let address = session.address();
        let producer = std::thread::spawn(move || {
            TcpStream::connect(address)
                .unwrap()
                .write_all(&bytes)
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut first_error = None;
        loop {
            assert!(Instant::now() < deadline, "failed session did not complete");
            match session
                .receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(100))
            {
                Ok(LiveEvent::Error(error)) => first_error = Some(error),
                Ok(LiveEvent::Complete {
                    received_bytes,
                    result,
                    ..
                }) => {
                    assert_eq!(received_bytes, expected.len() as u64);
                    let error = match *result {
                        Err(error) => error,
                        Ok(_) => panic!("semantic failure must remain a failed capture"),
                    };
                    let first_error = first_error.unwrap();
                    assert!(
                        first_error
                            .text(crate::i18n::Language::English)
                            .contains(&error)
                    );
                    assert!(
                        first_error
                            .text(crate::i18n::Language::Chinese)
                            .contains(&error)
                    );
                    break;
                }
                _ => (),
            }
        }
        producer.join().unwrap();
    }

    #[test]
    #[ignore = "Requires the 1000-frame official PhysX 3.4 CCT large-scene capture"]
    fn real_large_capture_keeps_live_snapshots_and_raw_capture_complete() {
        use std::io::Read;
        let path = std::env::var_os("PVD_LARGE_CAPTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4-cct-large.pvdstream")
            });
        let length = fs::metadata(&path).unwrap().len();
        let directory = test_directory("large");
        let session = NetworkSession::start(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                capture_directory: Some(directory.clone()),
                ..Default::default()
            },
            egui::Context::default(),
        )
        .unwrap();
        let address = session.address();
        let producer_path = path.clone();
        let producer = std::thread::spawn(move || {
            let mut file = File::open(producer_path).unwrap();
            let mut client = TcpStream::connect(address).unwrap();
            assert_eq!(std::io::copy(&mut file, &mut client).unwrap(), length);
        });
        let started = Instant::now();
        let deadline = started + Duration::from_secs(180);
        let mut snapshots = 0;
        let mut displayed = pvd_core::SceneState::default();
        let mut last_frame = None;
        loop {
            assert!(Instant::now() < deadline, "large capture timed out");
            if let Some(snapshot) = session.take_snapshot() {
                assert_eq!(snapshot.session, 1);
                assert!(last_frame.is_none_or(|frame| snapshot.frame >= frame));
                snapshot.update.apply(&mut displayed);
                last_frame = Some(snapshot.frame);
                snapshots += 1;
            }
            match session
                .receiver
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_millis(20))
            {
                Ok(LiveEvent::Complete {
                    session: id,
                    received_bytes,
                    result,
                    capture,
                }) => {
                    if let Some(snapshot) = session.take_snapshot_for(id) {
                        snapshot.update.apply(&mut displayed);
                        last_frame = Some(snapshot.frame);
                        snapshots += 1;
                    }
                    assert_eq!(id, 1);
                    let recording = result.unwrap();
                    assert_eq!(recording.frame_count(), 1000);
                    assert!(snapshots > 0);
                    assert_eq!(last_frame, Some(999));
                    assert_eq!(recording.state_at(999).unwrap(), displayed);
                    assert_eq!(received_bytes, length);
                    let capture = capture.unwrap();
                    let mut original = File::open(&path).unwrap();
                    let mut captured = File::open(&capture).unwrap();
                    let mut a = [0; 64 * 1024];
                    let mut b = [0; 64 * 1024];
                    loop {
                        let count = original.read(&mut a).unwrap();
                        captured.read_exact(&mut b[..count]).unwrap();
                        assert_eq!(a[..count], b[..count]);
                        if count == 0 {
                            break;
                        }
                    }
                    assert_eq!(captured.read(&mut b).unwrap(), 0);
                    eprintln!(
                        "large TCP capture: {snapshots} snapshots, {} frames, {received_bytes} bytes, {:?} history, {:.2}s",
                        recording.frame_count(),
                        recording.history_memory_usage(),
                        started.elapsed().as_secs_f64()
                    );
                    fs::remove_file(capture).unwrap();
                    break;
                }
                Ok(LiveEvent::Error(message)) => panic!(
                    "large capture failed: {}",
                    message.text(crate::i18n::Language::English)
                ),
                Ok(LiveEvent::Stopped) => panic!("listener stopped before capture completed"),
                _ => {}
            }
        }
        producer.join().unwrap();
        drop(session);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 output"]
    #[cfg(target_os = "windows")]
    fn real_capture_storage_quotas_keep_latest_scene_after_raw_and_history_stop() {
        let path = std::env::var_os("PVD_SDK34_CAPTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/physx-3.4.pvdstream")
            });
        let bytes = fs::read(path).unwrap();
        let expected = Recording::from_bytes(&bytes).unwrap();
        let last = expected.frame_count() - 1;
        let directory = test_directory("storage-quotas");
        let options = NetworkOptions {
            history: HistoryConfig {
                disk_budget_bytes: 4096,
                ..Default::default()
            },
            raw_limit_bytes: 1,
            ..Default::default()
        };
        let session = NetworkSession::start_with_options(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                capture_directory: Some(directory.clone()),
                ..Default::default()
            },
            options,
            egui::Context::default(),
        )
        .unwrap();
        let address = session.address();
        let producer = std::thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client.write_all(&bytes).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut raw_warning = false;
        loop {
            match next_event(&session, deadline) {
                LiveEvent::Error(error) => {
                    assert!(
                        error
                            .text(crate::i18n::Language::English)
                            .contains("capacity")
                    );
                    raw_warning = true;
                }
                LiveEvent::Complete {
                    result, capture, ..
                } => {
                    assert!(raw_warning);
                    let recording = result.unwrap();
                    assert!(
                        recording
                            .disk_usage()
                            .unwrap()
                            .warning
                            .unwrap()
                            .contains("budget")
                    );
                    assert!(recording.state_at(last).is_err());
                    let snapshot = session.take_snapshot().unwrap();
                    assert_eq!(snapshot.frame, last);
                    assert!(snapshot.update.replace_all);
                    let mut displayed = pvd_core::SceneState::default();
                    snapshot.update.apply(&mut displayed);
                    assert_eq!(displayed, expected.state_at(last).unwrap());
                    let capture = capture.unwrap();
                    assert_eq!(capture.extension().unwrap(), "partial");
                    assert_eq!(fs::metadata(&capture).unwrap().len(), 0);
                    fs::remove_file(capture).unwrap();
                    break;
                }
                _ => {}
            }
        }
        producer.join().unwrap();
        drop(session);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 output"]
    fn real_capture_reconnect_and_truncation_keep_sessions_isolated() {
        let path = std::env::var_os("PVD_SDK34_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4.pvdstream")
            });
        let capture = std::fs::read(path).unwrap();
        let expected = Recording::from_bytes(&capture).unwrap().frame_count();
        let directory = std::env::temp_dir().join(format!(
            "pvd-desktop-capture-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let session = NetworkSession::start(
            ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                capture_directory: Some(directory.clone()),
                ..Default::default()
            },
            egui::Context::default(),
        )
        .unwrap();
        for (index, data) in [capture.clone(), capture[..20].to_vec(), capture.clone()]
            .into_iter()
            .enumerate()
        {
            let address = session.address();
            let expected_bytes = data.len();
            let expected_raw = data.clone();
            let producer = std::thread::spawn(move || {
                let mut client = TcpStream::connect(address).unwrap();
                client.write_all(&data).unwrap();
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "live session did not finish");
                if let Ok(LiveEvent::Complete {
                    session: id,
                    received_bytes,
                    result,
                    capture,
                }) = session
                    .receiver
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_millis(100))
                {
                    assert_eq!(id, index as u64 + 1);
                    assert_eq!(received_bytes, expected_bytes as u64);
                    let path = capture.expect("automatic capture should return its actual path");
                    assert_eq!(std::fs::read(&path).unwrap(), expected_raw);
                    assert_eq!(
                        path.extension().unwrap(),
                        if index == 1 { "partial" } else { "pvdstream" }
                    );
                    std::fs::remove_file(path).unwrap();
                    if index == 1 {
                        assert!(result.is_err());
                    } else {
                        let recording = result.unwrap();
                        assert_eq!(recording.frame_count(), expected);
                        recording.verify_replay().unwrap();
                    }
                    break;
                }
            }
            producer.join().unwrap();
        }
        drop(session);
        std::fs::remove_dir(directory).unwrap();
    }
}
