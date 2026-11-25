use super::*;
use std::{
    collections::VecDeque,
    net::Shutdown,
    sync::{Condvar, Mutex, atomic::AtomicU64},
};

const MAX_ITEMS: usize = 1024;
#[cfg(test)]
#[path = "spool/finish_tests.rs"]
mod finish_tests;
#[cfg(test)]
static DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub type SpaceCheck = Arc<dyn Fn(u64) -> io::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct SpoolOptions {
    pub directory: PathBuf,
    /// Zero delegates the capacity boundary to the required disk space check.
    pub max_backlog_bytes: u64,
    pub segment_bytes: u64,
    pub space_check: Option<SpaceCheck>,
}

impl Default for SpoolOptions {
    fn default() -> Self {
        Self {
            directory: std::env::temp_dir().join("pvd-studio-receive"),
            max_backlog_bytes: 256 << 20,
            segment_bytes: 16 << 20,
            space_check: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpoolFailure {
    pub session: Option<u64>,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct SpoolSnapshot {
    pub session: u64,
    pub received_bytes: u64,
    /// Committed disk bytes not yet delivered to the consumer's bounded event queue.
    pub queued_bytes: u64,
    /// Raw payload bytes, including consumed prefixes until their segment can be removed.
    /// Ownership metadata and filesystem allocation overhead are excluded.
    pub retained_disk_bytes: u64,
    /// Zero indicates that disk space, rather than a byte quota, bounds the spool.
    pub limit_bytes: u64,
    pub connected: bool,
    pub last_failure: Option<String>,
    pub failure_session: Option<u64>,
}

pub struct SpoolStats {
    connected: AtomicBool,
    session: AtomicU64,
    received: AtomicU64,
    queued: AtomicU64,
    disk: AtomicU64,
    limit: u64,
    failure: Mutex<Option<SpoolFailure>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl SpoolStats {
    pub fn snapshot(&self) -> SpoolSnapshot {
        let failure = self.failure.lock().unwrap().clone();
        SpoolSnapshot {
            session: self.session.load(Ordering::Acquire),
            received_bytes: self.received.load(Ordering::Acquire),
            queued_bytes: self.queued.load(Ordering::Acquire),
            retained_disk_bytes: self.disk.load(Ordering::Acquire),
            limit_bytes: self.limit,
            connected: self.connected.load(Ordering::Acquire),
            failure_session: failure.as_ref().and_then(|failure| failure.session),
            last_failure: failure.map(|failure| failure.message),
        }
    }
    fn fail(&self, session: Option<u64>, message: &str) {
        *self.failure.lock().unwrap() = Some(SpoolFailure {
            session,
            message: message.to_owned(),
        });
        (self.wake)();
    }
    pub(super) fn disconnected(&self) {
        self.connected.store(false, Ordering::Release);
        (self.wake)();
    }
}

struct Directory {
    storage: crate::spool_directory::SpoolDirectory,
    stats: Arc<SpoolStats>,
    space_check: Option<SpaceCheck>,
}

impl Directory {
    fn new(options: &SpoolOptions, stats: Arc<SpoolStats>) -> io::Result<Arc<Self>> {
        let storage = crate::spool_directory::SpoolDirectory::new(&options.directory)?;
        Ok(Arc::new(Self {
            storage,
            stats,
            space_check: options.space_check.clone(),
        }))
    }
    fn reserve(&self, bytes: u64) -> io::Result<()> {
        if let Some(check) = &self.space_check {
            check(bytes)?;
        }
        self.stats
            .disk
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|&total| self.stats.limit == 0 || total <= self.stats.limit)
            })
            .map(|_| ())
            .map_err(|_| {
                io::Error::other(if self.stats.limit == 0 {
                    "receive spool byte count overflow".to_owned()
                } else {
                    format!(
                        "receive backlog capacity reached: configured quota is {} bytes; connection closed to avoid blocking the SDK",
                        self.stats.limit
                    )
                })
            })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ProgressState {
    committed: u64,
    finished: bool,
}

#[derive(Default)]
struct SessionProgress {
    state: Mutex<ProgressState>,
    signal: Condvar,
}

impl SessionProgress {
    fn commit(&self, committed: u64) {
        self.state.lock().unwrap().committed = committed;
        self.signal.notify_all();
    }

    fn finish(&self) {
        self.state.lock().unwrap().finished = true;
        self.signal.notify_all();
    }

    fn wait_after(&self, position: u64, cancel: &AtomicBool) -> Option<ProgressState> {
        let state = self.state.lock().unwrap();
        let (state, timeout) = self
            .signal
            .wait_timeout_while(state, POLL, |state| {
                state.committed == position && !state.finished && !cancel.load(Ordering::Acquire)
            })
            .unwrap();
        (!timeout.timed_out()).then_some(*state)
    }
}

struct SessionSpool {
    owner: Arc<Directory>,
    segment_bytes: u64,
    created_segments: AtomicU64,
    first_retained_segment: AtomicU64,
    reserved_bytes: AtomicU64,
    progress: SessionProgress,
    session: u64,
}

impl SessionSpool {
    fn path(&self, index: u64) -> PathBuf {
        self.owner.storage.path.join(self.name(index))
    }

    fn name(&self, index: u64) -> String {
        format!("{:016}-{index:016}.bin", self.session)
    }

    fn remove(&self, index: u64) -> io::Result<()> {
        debug_assert_eq!(index, self.first_retained_segment.load(Ordering::Acquire));
        self.remove_file(index)?;
        self.first_retained_segment
            .store(index + 1, Ordering::Release);
        Ok(())
    }

    fn remove_file(&self, index: u64) -> io::Result<()> {
        self.owner.storage.remove_segment(&self.name(index))?;
        let reserved = self.reserved_bytes.load(Ordering::Acquire);
        let bytes = reserved
            .saturating_sub(index.saturating_mul(self.segment_bytes))
            .min(self.segment_bytes);
        self.owner.stats.disk.fetch_sub(bytes, Ordering::Release);
        Ok(())
    }
}

impl Drop for SessionSpool {
    fn drop(&mut self) {
        let first = self.first_retained_segment.load(Ordering::Acquire);
        let end = self.created_segments.load(Ordering::Acquire);
        for index in first..end {
            let _ = self.remove_file(index);
        }
    }
}

struct Writer {
    file: Option<File>,
    spool: Arc<SessionSpool>,
    written: u64,
}

impl Writer {
    fn new(
        owner: &Arc<Directory>,
        queue: &Queue,
        segment_bytes: u64,
        session: u64,
    ) -> io::Result<Self> {
        let spool = Arc::new(SessionSpool {
            owner: owner.clone(),
            segment_bytes,
            created_segments: AtomicU64::new(0),
            first_retained_segment: AtomicU64::new(0),
            reserved_bytes: AtomicU64::new(0),
            progress: SessionProgress::default(),
            session,
        });
        let writer = Self {
            file: None,
            spool,
            written: 0,
        };
        queue.push(Item::Session(writer.spool.clone()))?;
        Ok(writer)
    }
    fn append(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let index = self.written / self.spool.segment_bytes;
            if self.file.is_none() {
                self.file = Some(
                    self.spool
                        .owner
                        .storage
                        .create_segment(&self.spool.name(index))?,
                );
                self.spool
                    .created_segments
                    .store(index + 1, Ordering::Release);
            }
            let remaining = self.spool.segment_bytes - self.written % self.spool.segment_bytes;
            let count = remaining.min(bytes.len() as u64) as usize;
            let written = self
                .written
                .checked_add(count as u64)
                .ok_or_else(|| io::Error::other("receive spool byte count overflow"))?;
            self.spool.owner.reserve(count as u64)?;
            self.spool
                .reserved_bytes
                .fetch_add(count as u64, Ordering::Release);
            self.file.as_mut().unwrap().write_all(&bytes[..count])?;
            self.written = written;
            if self.written.is_multiple_of(self.spool.segment_bytes) {
                // A published full segment can immediately be deleted by the delivery thread.
                drop(self.file.take());
            }
            self.spool
                .owner
                .stats
                .queued
                .fetch_add(count as u64, Ordering::Release);
            self.spool.progress.commit(self.written);
            bytes = &bytes[count..];
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        drop(self.file.take());
        self.spool.progress.finish();
    }
}

enum Item {
    Event(ReceiverEvent),
    Session(Arc<SessionSpool>),
}

#[derive(Default)]
struct QueueState {
    items: VecDeque<Item>,
    done: bool,
    terminal: Option<ReceiverEvent>,
}

#[derive(Default)]
struct Queue {
    state: Mutex<QueueState>,
    signal: Arc<Condvar>,
    abort: AtomicBool,
    socket: Arc<Mutex<Option<TcpStream>>>,
}

impl Queue {
    fn push(&self, item: Item) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.items.len() >= MAX_ITEMS {
            return Err(io::Error::other(
                "receive spool event metadata capacity reached",
            ));
        }
        state.items.push_back(item);
        self.signal.notify_all();
        Ok(())
    }
    fn fail(&self, stats: &SpoolStats, session: Option<u64>, message: String) -> bool {
        stats.fail(session, &message);
        let event = ReceiverEvent::Failed { session, message };
        let mut state = self.state.lock().unwrap();
        if state.items.len() < MAX_ITEMS {
            state.items.push_back(Item::Event(event));
            self.signal.notify_all();
            true
        } else {
            state.terminal = Some(event);
            self.signal.notify_all();
            false
        }
    }
}

fn relay_session(spool: Arc<SessionSpool>, output: &Output) -> io::Result<bool> {
    let mut file = None::<File>;
    let mut position = 0u64;
    while !output.stopped() {
        let Some(ProgressState {
            committed,
            finished,
        }) = spool.progress.wait_after(position, &output.cancel)
        else {
            continue;
        };
        if position < committed {
            let index = position / spool.segment_bytes;
            if file.is_none() {
                file = Some(File::open(spool.path(index))?);
            }
            let remaining = spool.segment_bytes - position % spool.segment_bytes;
            let length = (committed - position).min(remaining).min(CHUNK_SIZE as u64) as usize;
            let mut bytes = vec![0; length];
            file.as_mut().unwrap().read_exact(&mut bytes)?;
            if !output.send(ReceiverEvent::Data {
                session: spool.session,
                bytes,
            }) {
                return Ok(false);
            }
            spool
                .owner
                .stats
                .queued
                .fetch_sub(length as u64, Ordering::Release);
            position += length as u64;
            if position.is_multiple_of(spool.segment_bytes) {
                drop(file.take());
                spool.remove(index)?;
            }
        } else if finished {
            drop(file.take());
            if !position.is_multiple_of(spool.segment_bytes) {
                spool.remove(position / spool.segment_bytes)?;
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn relay(queue: Arc<Queue>, output: Output, stats: Arc<SpoolStats>) {
    while !output.stopped() {
        let item = {
            let mut state = queue.state.lock().unwrap();
            if let Some(item) = state.items.pop_front() {
                Some(item)
            } else if state.done {
                state.terminal.take().map(Item::Event)
            } else {
                drop(queue.signal.wait_timeout(state, POLL).unwrap());
                continue;
            }
        };
        let session = match &item {
            Some(Item::Session(spool)) => Some(spool.session),
            _ => None,
        };
        let result = match item {
            Some(Item::Event(event)) => Ok(output.send(event)),
            Some(Item::Session(spool)) => relay_session(spool, &output),
            None => break,
        };
        match result {
            Ok(true) => {}
            Ok(false) => break,
            Err(error) => {
                let message = format!("receive spool read failed: {error}");
                queue.abort.store(true, Ordering::Release);
                if let Some(socket) = queue.socket.lock().unwrap().take() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
                stats.disconnected();
                stats.fail(session, &message);
                output.send(ReceiverEvent::Failed { session, message });
                break;
            }
        }
    }
    output.cancel.store(true, Ordering::Release);
}

struct ReceiveContext<'a> {
    config: &'a ListenConfig,
    options: &'a SpoolOptions,
    owner: &'a Arc<Directory>,
    queue: &'a Queue,
    cancel: &'a AtomicBool,
    finish: &'a AtomicBool,
    intake_quiesced: &'a AtomicBool,
}

impl ReceiveContext<'_> {
    fn receive(
        &self,
        stream: &mut Incoming<'_>,
        session: u64,
    ) -> io::Result<(u64, Option<PathBuf>)> {
        let Self {
            config,
            options,
            owner,
            queue,
            cancel,
            finish,
            intake_quiesced,
        } = *self;
        stream.configure()?;
        let mut capture = config
            .capture_directory
            .as_deref()
            .map(|dir| CaptureFile::create(dir, session))
            .transpose()?;
        if let Some(capture) = &capture {
            queue.push(Item::Event(ReceiverEvent::CaptureStarted {
                session,
                path: capture.partial.clone(),
            }))?;
        }
        let mut writer = Writer::new(owner, queue, options.segment_bytes, session)?;
        // Keep deletion ownership separate from the writer. On cancellation,
        // closing both writers must precede publishing quiescence, while the
        // potentially long segment deletion must follow it.
        let retiring_spool = writer.spool.clone();
        let mut buffer = vec![0; options.segment_bytes.min(CHUNK_SIZE as u64) as usize];
        let mut total = 0u64;
        let mut last_wake = std::time::Instant::now();
        let result = (|| {
            while !cancel.load(Ordering::Acquire) && !queue.abort.load(Ordering::Acquire) {
                let received = if finish.load(Ordering::Acquire) {
                    Ok(0)
                } else {
                    stream.read(&mut buffer, &|| {
                        cancel.load(Ordering::Acquire)
                            || finish.load(Ordering::Acquire)
                            || queue.abort.load(Ordering::Acquire)
                    })
                };
                let received = match received {
                    Err(_) if finish.load(Ordering::Acquire) => Ok(0),
                    value => value,
                };
                let length = match received {
                    Ok(0) => {
                        if !cancel.load(Ordering::Acquire)
                            && !finish.load(Ordering::Acquire)
                            && !queue.abort.load(Ordering::Acquire)
                        {
                            stream.complete();
                        }
                        return Ok((total, capture.take().map(CaptureFile::finish).transpose()?));
                    }
                    Ok(length) => length,
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                total = total
                    .checked_add(length as u64)
                    .ok_or_else(|| io::Error::other("receive byte count overflow"))?;
                owner.stats.received.store(total, Ordering::Release);
                if last_wake.elapsed() >= Duration::from_millis(100) {
                    (owner.stats.wake)();
                    last_wake = std::time::Instant::now();
                }
                if total > config.max_session_bytes {
                    return Err(io::Error::other(format!(
                        "session {session} exceeds capture limit of {} bytes; partial capture retained",
                        config.max_session_bytes
                    )));
                }
                stream.share(&buffer[..length]);
                if let Some(capture) = capture.as_mut() {
                    capture.writer.write_all(&buffer[..length])?;
                }
                writer.append(&buffer[..length])?;
            }
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "receive cancelled",
            ))
        })();
        drop(writer);
        // BufWriter may flush on drop, so even a cancelled partial raw capture
        // must be closed before an observer is allowed to leave cleanup behind.
        drop(capture);
        // Publish idle even on ordinary EOF: cancellation may arrive while the
        // following last-owner drop is deleting a large backlog. A new session
        // clears this flag under the socket gate before checking stop or writing.
        intake_quiesced.store(true, Ordering::Release);
        drop(retiring_spool);
        result
    }
}

impl CaptureServer {
    pub fn bind_spooled(
        config: ListenConfig,
        options: SpoolOptions,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::bind_spooled_with_sharing(config, options, None, wake)
    }

    pub fn bind_spooled_with_sharing(
        config: ListenConfig,
        options: SpoolOptions,
        sharing: Option<ShareOptions>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Self> {
        if config.max_session_bytes == 0
            || !(1..=1024).contains(&config.queue_capacity)
            || options.segment_bytes == 0
            || (options.max_backlog_bytes != 0 && options.segment_bytes > options.max_backlog_bytes)
            || (options.max_backlog_bytes == 0 && options.space_check.is_none())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid receive spool configuration: require a positive segment size, a compatible quota, a disk space check for zero quota, and queue capacity 1..=1024",
            ));
        }
        if let Some(directory) = &config.capture_directory {
            fs::create_dir_all(directory)?;
        }
        let wake: intake::Wake = Arc::new(wake);
        let stats = Arc::new(SpoolStats {
            connected: AtomicBool::new(false),
            session: AtomicU64::new(0),
            received: AtomicU64::new(0),
            queued: AtomicU64::new(0),
            disk: AtomicU64::new(0),
            limit: options.max_backlog_bytes,
            failure: Mutex::new(None),
            wake: wake.clone(),
        });
        let owner = Directory::new(&options, stats.clone())?;
        let mut listener = Listener::bind(config.address, sharing, wake.clone())?;
        let address = listener.address()?;
        let role = listener.role();
        let cancel = Arc::new(AtomicBool::new(false));
        let finish = Arc::new(AtomicBool::new(false));
        let intake_quiesced = Arc::new(AtomicBool::new(false));
        let intake_completion = IntakeCompletion(intake_quiesced.clone());
        let finish_intake = finish.clone();
        let output_wake = wake.clone();
        let (output, receiver) =
            Output::channel(config.queue_capacity, cancel.clone(), move || output_wake());
        let delivery_signal = output.signal.clone();
        let stop = cancel.clone();
        let queue = Arc::new(Queue::default());
        let socket = queue.socket.clone();
        queue.push(Item::Event(ReceiverEvent::Listening(address)))?;
        let relay_queue = queue.clone();
        let relay_stats = stats.clone();
        let relay_worker = thread::Builder::new()
            .name("pvd-spool-delivery".into())
            .spawn(move || relay(relay_queue, output, relay_stats))?;
        let receive_stats = stats.clone();
        let worker_result = thread::Builder::new()
            .name("pvd-spool-receiver".into())
            .spawn(move || {
                let intake_completion = intake_completion;
                let mut session = 0u64;
                let stopped = || {
                    stop.load(Ordering::Acquire)
                        || finish_intake.load(Ordering::Acquire)
                        || queue.abort.load(Ordering::Acquire)
                };
                while !stopped() {
                    match listener.accept(&stopped) {
                        Ok(mut stream) => {
                            let peer = stream.peer;
                            let mut active_socket = queue.socket.lock().unwrap();
                            // request_stop sets cancellation before taking this
                            // same gate. Once it returns, idle cannot race a new
                            // writer that has already passed the stop check.
                            intake_completion.0.store(false, Ordering::Release);
                            if stopped() {
                                stream.shutdown();
                                break;
                            }
                            session += 1;
                            receive_stats.connected.store(true, Ordering::Release);
                            *receive_stats.failure.lock().unwrap() = None;
                            receive_stats.session.store(session, Ordering::Release);
                            receive_stats.received.store(0, Ordering::Release);
                            (receive_stats.wake)();
                            let registered = stream.control_socket().map(|socket| {
                                *active_socket = socket;
                            });
                            drop(active_socket);
                            let result = registered
                                .and_then(|()| {
                                    queue.push(Item::Event(ReceiverEvent::Connected {
                                        session,
                                        peer,
                                    }))
                                })
                                .and_then(|()| {
                                    ReceiveContext {
                                        config: &config,
                                        options: &options,
                                        owner: &owner,
                                        queue: &queue,
                                        cancel: &stop,
                                        finish: &finish_intake,
                                        intake_quiesced: &intake_completion.0,
                                    }
                                    .receive(&mut stream, session)
                                });
                            stream.shutdown();
                            drop(stream);
                            queue.socket.lock().unwrap().take();
                            receive_stats.disconnected();
                            if stop.load(Ordering::Acquire) || queue.abort.load(Ordering::Acquire) {
                                break;
                            }
                            match result {
                                Ok((bytes, capture)) => {
                                    if let Err(error) =
                                        queue.push(Item::Event(ReceiverEvent::Disconnected {
                                            session,
                                            bytes,
                                            capture,
                                        }))
                                        && !queue.fail(
                                            &receive_stats,
                                            Some(session),
                                            error.to_string(),
                                        )
                                    {
                                        break;
                                    }
                                }
                                Err(error) => {
                                    if !queue.fail(&receive_stats, Some(session), error.to_string())
                                    {
                                        break;
                                    }
                                }
                            }
                        }
                        Err(_) if stopped() => break,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::park_timeout(POLL)
                        }
                        Err(error) => {
                            queue.fail(&receive_stats, None, error.to_string());
                            break;
                        }
                    }
                }
                // Draining a bounded consumer can take arbitrarily long; do not accept new clients meanwhile.
                drop(listener);
                drop(intake_completion);
                queue.state.lock().unwrap().done = true;
                queue.signal.notify_all();
                let _ = relay_worker.join();
                queue.state.lock().unwrap().items.clear();
                receive_stats.queued.store(0, Ordering::Release);
            });
        let worker = match worker_result {
            Ok(worker) => worker,
            Err(error) => {
                delivery_signal.stop(&cancel);
                return Err(error);
            }
        };
        Ok(Self {
            receiver,
            address,
            cancel,
            finish,
            intake_quiesced,
            worker: Some(worker),
            spool_stats: Some(stats),
            socket,
            delivery_signal,
            role,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "pvd-spool-test-{}-{}",
                std::process::id(),
                DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn options(&self, capacity: u64) -> SpoolOptions {
            SpoolOptions {
                directory: self.0.clone(),
                max_backlog_bytes: capacity,
                segment_bytes: 64 << 10,
                space_check: None,
            }
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if self.0.exists() {
                assert!(
                    fs::read_dir(&self.0).unwrap().next().is_none(),
                    "spool files survived shutdown"
                );
                fs::remove_dir(&self.0).unwrap();
            }
        }
    }
    fn config() -> ListenConfig {
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            queue_capacity: 1,
            max_session_bytes: u64::MAX,
            ..Default::default()
        }
    }
    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "spool condition timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
    fn event(server: &CaptureServer) -> ReceiverEvent {
        server
            .receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
    }

    #[test]
    fn progress_published_before_wait_is_not_reliant_on_a_future_notification() {
        let progress = SessionProgress::default();
        let cancel = AtomicBool::new(false);
        let observed = *progress.state.lock().unwrap();
        assert_eq!(observed.committed, 0);
        assert!(!observed.finished);

        progress.commit(19);
        let available = progress
            .wait_after(observed.committed, &cancel)
            .expect("committed bytes must bypass the condition-variable wait");
        assert_eq!(available.committed, 19);
        assert!(!available.finished);

        progress.finish();
        let complete = progress
            .wait_after(available.committed, &cancel)
            .expect("finished sessions must bypass the condition-variable wait");
        assert_eq!(complete.committed, 19);
        assert!(complete.finished);
    }

    #[test]
    fn progress_publication_is_serialized_with_the_wait_predicate() {
        for finish in [false, true] {
            let progress = Arc::new(SessionProgress::default());
            let state = progress.state.lock().unwrap();
            let (attempting, attempted) = mpsc::channel();
            let (completed, completion) = mpsc::channel();
            let publishing = progress.clone();
            let publisher = thread::spawn(move || {
                attempting.send(()).unwrap();
                if finish {
                    publishing.finish();
                } else {
                    publishing.commit(7);
                }
                completed.send(()).unwrap();
            });
            attempted.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(matches!(
                completion.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));

            let (state, timeout) = progress
                .signal
                .wait_timeout_while(state, Duration::from_secs(5), |state| {
                    state.committed == 0 && !state.finished
                })
                .unwrap();
            assert!(
                !timeout.timed_out(),
                "publication must wake a waiting reader"
            );
            assert_eq!(state.committed, if finish { 0 } else { 7 });
            assert_eq!(state.finished, finish);
            drop(state);
            publisher.join().unwrap();
            completion.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }

    #[test]
    fn cancelled_progress_wait_does_not_require_a_writer_notification() {
        let progress = SessionProgress::default();
        let cancel = AtomicBool::new(true);
        let state = progress
            .wait_after(0, &cancel)
            .expect("cancellation must bypass the condition-variable wait");
        assert_eq!(state.committed, 0);
        assert!(!state.finished);
    }

    #[test]
    fn zero_quota_requires_a_disk_space_check() {
        let directory = TestDirectory::new();
        let result = CaptureServer::bind_spooled(config(), directory.options(0), || {});
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
        assert!(!directory.0.exists());
    }

    #[test]
    fn stalled_delivery_spools_over_a_thousand_segments_and_preserves_session_order() {
        let directory = TestDirectory::new();
        let mut options = directory.options(0);
        options.segment_bytes = 257;
        options.space_check = Some(Arc::new(|_| Ok(())));
        let mut server = CaptureServer::bind_spooled(config(), options, || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let first: Vec<u8> = (0..(MAX_ITEMS + 37) * 257 + 113)
            .map(|n| (n % 251) as u8)
            .collect();
        let second = b"second session after a long unread stream".to_vec();
        for (index, payload) in [&first, &second].into_iter().enumerate() {
            let mut client = TcpStream::connect(server.address()).unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            client.write_all(payload).unwrap();
            client.shutdown(Shutdown::Write).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let status = stats.snapshot();
                if status.session == index as u64 + 1
                    && !status.connected
                    && status.received_bytes == payload.len() as u64
                {
                    assert!(status.last_failure.is_none(), "{status:?}");
                    break;
                }
                assert!(Instant::now() < deadline, "stalled intake: {status:?}");
                thread::sleep(Duration::from_millis(5));
            }
        }
        let session_directory = fs::read_dir(&directory.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(fs::read_dir(session_directory).unwrap().count() > MAX_ITEMS);
        let total = (first.len() + second.len()) as u64;
        assert_eq!(stats.snapshot().retained_disk_bytes, total);
        assert_eq!(stats.snapshot().queued_bytes, total);
        assert_eq!(stats.snapshot().limit_bytes, 0);

        assert!(matches!(event(&server), ReceiverEvent::Listening(_)));
        for (index, expected) in [first, second].into_iter().enumerate() {
            let session = index as u64 + 1;
            assert!(matches!(
                event(&server),
                ReceiverEvent::Connected { session: id, .. } if id == session
            ));
            let mut received = Vec::new();
            loop {
                match event(&server) {
                    ReceiverEvent::Data { session: id, bytes } => {
                        assert_eq!(id, session);
                        received.extend(bytes);
                    }
                    ReceiverEvent::Disconnected {
                        session: id, bytes, ..
                    } => {
                        assert_eq!(id, session);
                        assert_eq!(bytes, expected.len() as u64);
                        break;
                    }
                    other => panic!("unexpected event: {other:?}"),
                }
            }
            assert_eq!(received, expected);
        }
        wait_for(|| stats.snapshot().retained_disk_bytes == 0);
        assert_eq!(stats.snapshot().queued_bytes, 0);
        server.stop();
    }

    #[test]
    fn cancelling_a_large_segment_range_closes_socket_and_removes_all_owned_files() {
        let directory = TestDirectory::new();
        let mut options = directory.options(0);
        options.segment_bytes = 257;
        options.space_check = Some(Arc::new(|_| Ok(())));
        let mut server = CaptureServer::bind_spooled(config(), options, || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let mut client = TcpStream::connect(server.address()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let payload = vec![73; (MAX_ITEMS + 9) * 257 + 31];
        client.write_all(&payload).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while stats.snapshot().queued_bytes != payload.len() as u64 {
            assert!(
                Instant::now() < deadline,
                "intake did not commit the payload"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        server.control().request_stop();
        let mut byte = [0];
        assert!(!matches!(client.read(&mut byte), Ok(n) if n > 0));
        assert!(started.elapsed() < Duration::from_secs(1));
        server.stop();
        assert_eq!(stats.snapshot().retained_disk_bytes, 0);
        assert_eq!(stats.snapshot().queued_bytes, 0);
        assert!(!stats.snapshot().connected);
    }

    #[test]
    fn stalled_parser_does_not_block_socket_and_replayed_bytes_are_exact() {
        let directory = TestDirectory::new();
        let mut server =
            CaptureServer::bind_spooled(config(), directory.options(8 << 20), || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let payload: Vec<u8> = (0..4 << 20).map(|n| (n % 251) as u8).collect();
        let sent = payload.clone();
        let address = server.address();
        let producer = thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            for chunk in sent.chunks(97_003) {
                client.write_all(chunk).unwrap();
            }
            client.shutdown(Shutdown::Write).unwrap();
        });
        wait_for(|| producer.is_finished());
        producer.join().unwrap();
        wait_for(|| {
            stats.snapshot().received_bytes == payload.len() as u64 && !stats.snapshot().connected
        });
        assert!(stats.snapshot().queued_bytes > 0);
        assert!(stats.snapshot().retained_disk_bytes <= 8 << 20);
        let mut received = Vec::new();
        loop {
            match event(&server) {
                ReceiverEvent::Data { session, bytes } => {
                    assert_eq!(session, 1);
                    received.extend(bytes);
                }
                ReceiverEvent::Disconnected { session, bytes, .. } => {
                    assert_eq!(session, 1);
                    assert_eq!(bytes, payload.len() as u64);
                    break;
                }
                ReceiverEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(received, payload);
        wait_for(|| stats.snapshot().retained_disk_bytes == 0);
        assert_eq!(stats.snapshot().queued_bytes, 0);
        server.stop();
    }

    #[test]
    fn full_spool_closes_socket_before_parser_consumes_failure_and_reconnects_cleanly() {
        let directory = TestDirectory::new();
        let mut server =
            CaptureServer::bind_spooled(config(), directory.options(128 << 10), || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let address = server.address();
        let producer = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let _ = stream.write_all(&vec![23; 8 << 20]);
            let mut byte = [0];
            assert!(!matches!(stream.read(&mut byte), Ok(n) if n > 0));
        });
        wait_for(|| stats.snapshot().last_failure.is_some());
        let status = stats.snapshot();
        assert!(!status.connected);
        assert_eq!(status.failure_session, Some(1));
        assert!(status.last_failure.unwrap().contains("backlog capacity"));
        assert!(status.retained_disk_bytes <= status.limit_bytes);
        producer.join().unwrap();
        let mut prefix = Vec::new();
        loop {
            match event(&server) {
                ReceiverEvent::Data { session, bytes } => {
                    assert_eq!(session, 1);
                    prefix.extend(bytes);
                }
                ReceiverEvent::Failed { session, message } => {
                    assert_eq!(session, Some(1));
                    assert!(message.contains("backlog capacity"));
                    break;
                }
                ReceiverEvent::Disconnected { .. } => {
                    panic!("overload cannot become a clean disconnect")
                }
                _ => {}
            }
        }
        assert!(!prefix.is_empty() && prefix.len() <= 128 << 10);
        assert!(prefix.iter().all(|&byte| byte == 23));
        wait_for(|| stats.snapshot().retained_disk_bytes == 0);
        let mut client = TcpStream::connect(server.address()).unwrap();
        client.write_all(b"new session").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut received = Vec::new();
        loop {
            match event(&server) {
                ReceiverEvent::Data { session, bytes } => {
                    assert_eq!(session, 2);
                    received.extend(bytes);
                }
                ReceiverEvent::Disconnected { session, .. } => {
                    assert_eq!(session, 2);
                    break;
                }
                ReceiverEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(received, b"new session");
        assert!(stats.snapshot().last_failure.is_none());
        server.stop();
    }

    #[test]
    fn concurrent_segment_completion_and_reconnect_preserve_every_tail_byte() {
        let directory = TestDirectory::new();
        let mut options = directory.options(2 << 20);
        options.segment_bytes = 257;
        let mut server = CaptureServer::bind_spooled(
            ListenConfig {
                queue_capacity: 32,
                ..config()
            },
            options,
            || {},
        )
        .unwrap();
        for session in 1..=8 {
            let expected: Vec<u8> = (0..(257 * 63 + session as usize))
                .map(|n| ((n + session as usize) % 251) as u8)
                .collect();
            let payload = expected.clone();
            let address = server.address();
            let producer = thread::spawn(move || {
                let mut client = TcpStream::connect(address).unwrap();
                for chunk in payload.chunks(37) {
                    client.write_all(chunk).unwrap();
                    thread::yield_now();
                }
                client.shutdown(Shutdown::Write).unwrap();
            });
            let mut received = Vec::new();
            loop {
                match event(&server) {
                    ReceiverEvent::Data { session: id, bytes } => {
                        assert_eq!(id, session);
                        received.extend(bytes);
                    }
                    ReceiverEvent::Disconnected {
                        session: id, bytes, ..
                    } => {
                        assert_eq!(id, session);
                        assert_eq!(bytes, expected.len() as u64);
                        break;
                    }
                    ReceiverEvent::Failed { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            producer.join().unwrap();
            assert_eq!(received, expected);
        }
        server.stop();
    }

    #[test]
    fn configured_session_limit_preserves_the_existing_diagnostic() {
        let directory = TestDirectory::new();
        let mut server = CaptureServer::bind_spooled(
            ListenConfig {
                max_session_bytes: 1,
                ..config()
            },
            directory.options(1 << 20),
            || {},
        )
        .unwrap();
        let mut client = TcpStream::connect(server.address()).unwrap();
        client.write_all(&[3; 8192]).unwrap();
        loop {
            match event(&server) {
                ReceiverEvent::Failed { session, message } => {
                    assert_eq!(session, Some(1));
                    assert_eq!(
                        message,
                        "session 1 exceeds capture limit of 1 bytes; partial capture retained"
                    );
                    break;
                }
                ReceiverEvent::Disconnected { .. } => panic!("limit must fail the session"),
                _ => {}
            }
        }
        server.stop();
    }

    #[test]
    fn disk_space_failure_is_immediate_and_closes_the_connection() {
        let directory = TestDirectory::new();
        let mut options = directory.options(0);
        options.space_check = Some(Arc::new(|_| {
            Err(io::Error::other("injected disk reserve failure"))
        }));
        let mut server = CaptureServer::bind_spooled(config(), options, || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let mut client = TcpStream::connect(server.address()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        client.write_all(b"uncommitted").unwrap();
        wait_for(|| stats.snapshot().last_failure.is_some());
        assert!(!stats.snapshot().connected);
        assert!(
            stats
                .snapshot()
                .last_failure
                .unwrap()
                .contains("injected disk reserve failure")
        );
        assert_eq!(stats.snapshot().queued_bytes, 0);
        assert_eq!(stats.snapshot().retained_disk_bytes, 0);
        let mut byte = [0];
        assert!(!matches!(client.read(&mut byte), Ok(n) if n > 0));
        server.stop();
    }

    #[test]
    fn cancelling_an_unconsumed_spool_releases_threads_socket_and_owned_files() {
        let directory = TestDirectory::new();
        let mut server =
            CaptureServer::bind_spooled(config(), directory.options(2 << 20), || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let mut client = TcpStream::connect(server.address()).unwrap();
        client.write_all(&vec![13; 512 << 10]).unwrap();
        wait_for(|| stats.snapshot().queued_bytes > 0);
        let started = Instant::now();
        server.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!stats.snapshot().connected);
        assert_eq!(stats.snapshot().retained_disk_bytes, 0);
        assert_eq!(stats.snapshot().queued_bytes, 0);
    }

    #[test]
    fn queue_metadata_rejects_growth_beyond_its_fixed_bound() {
        let queue = Queue::default();
        for _ in 0..MAX_ITEMS {
            queue
                .push(Item::Event(ReceiverEvent::Listening(
                    "127.0.0.1:1".parse().unwrap(),
                )))
                .unwrap();
        }
        assert!(
            queue
                .push(Item::Event(ReceiverEvent::Listening(
                    "127.0.0.1:1".parse().unwrap()
                )))
                .is_err()
        );
        assert_eq!(queue.state.lock().unwrap().items.len(), MAX_ITEMS);
    }

    #[cfg(windows)]
    #[test]
    fn accepted_windows_socket_waits_for_its_read_timeout() {
        for spooled in [false, true] {
            let directory = TestDirectory::new();
            let server = if spooled {
                CaptureServer::bind_spooled(config(), directory.options(2 << 20), || {}).unwrap()
            } else {
                CaptureServer::bind(config(), || {}).unwrap()
            };
            let mut client = TcpStream::connect(server.address()).unwrap();
            client.write_all(b"a").unwrap();
            assert!(matches!(event(&server), ReceiverEvent::Listening(_)));
            assert!(matches!(event(&server), ReceiverEvent::Connected { .. }));
            assert!(matches!(event(&server), ReceiverEvent::Data { bytes, .. } if bytes == b"a"));

            let socket = server
                .socket
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .try_clone()
                .unwrap();
            assert_eq!(socket.read_timeout().unwrap(), Some(POLL));
            assert_eq!(
                socket.peek(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::TimedOut,
                "spooled={spooled}"
            );
            client.write_all(b"b").unwrap();
            assert!(matches!(event(&server), ReceiverEvent::Data { bytes, .. } if bytes == b"b"));
        }
    }

    #[test]
    fn independent_control_closes_socket_without_a_parser_tick() {
        for spooled in [false, true] {
            let directory = TestDirectory::new();
            let mut server = if spooled {
                CaptureServer::bind_spooled(config(), directory.options(2 << 20), || {}).unwrap()
            } else {
                CaptureServer::bind(config(), || {}).unwrap()
            };
            let control = server.control();
            let mut client = TcpStream::connect(server.address()).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            client.write_all(&[17; 4096]).unwrap();
            wait_for(|| server.socket.lock().unwrap().is_some());
            let started = Instant::now();
            control.request_stop();
            let mut byte = [0];
            assert!(!matches!(client.read(&mut byte), Ok(n) if n > 0));
            assert!(started.elapsed() < Duration::from_millis(500));
            server.stop();
            if let Some(stats) = server.spool_stats() {
                assert!(!stats.snapshot().connected);
                assert_eq!(stats.snapshot().retained_disk_bytes, 0);
            }
        }
    }

    #[test]
    fn unreadable_segment_reports_its_session_and_closes_socket_before_error_delivery() {
        let directory = TestDirectory::new();
        let notifications = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let received_wake = notifications.clone();
        let mut server =
            CaptureServer::bind_spooled(config(), directory.options(2 << 20), move || {
                received_wake.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        let stats = server.spool_stats().unwrap();
        let mut client = TcpStream::connect(server.address()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client.write_all(&[41; 128]).unwrap();
        wait_for(|| stats.snapshot().queued_bytes == 128);
        let session_directory = fs::read_dir(&directory.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let path = fs::read_dir(session_directory)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "bin")
            })
            .unwrap()
            .path();
        OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(0)
            .unwrap();
        assert!(matches!(event(&server), ReceiverEvent::Listening(_)));
        assert!(matches!(
            event(&server),
            ReceiverEvent::Connected { session: 1, .. }
        ));
        wait_for(|| stats.snapshot().last_failure.is_some());
        assert!(!stats.snapshot().connected);
        assert_eq!(stats.snapshot().failure_session, Some(1));
        let mut byte = [0];
        assert!(!matches!(client.read(&mut byte), Ok(n) if n > 0));
        match event(&server) {
            ReceiverEvent::Failed { session, message } => {
                assert_eq!(session, Some(1));
                assert!(message.contains("spool read failed"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(notifications.load(Ordering::Relaxed) >= 3);
        server.stop();
    }

    #[test]
    #[cfg(target_os = "windows")]
    #[ignore = "writes and verifies a 272 MiB disk backlog; requires at least 800 MiB free"]
    fn unlimited_spool_retains_more_than_256_mib_and_drains_exactly() {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            #[link_name = "GetDiskFreeSpaceExW"]
            fn get_disk_free_space_ex_w(
                directory: *const u16,
                available: *mut u64,
                total: *mut u64,
                free: *mut u64,
            ) -> i32;
        }

        fn available_space(path: &Path) -> io::Result<u64> {
            let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut available = 0;
            let result = unsafe {
                get_disk_free_space_ex_w(
                    path.as_ptr(),
                    &mut available,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(available)
            }
        }

        fn fill_pattern(bytes: &mut [u8], offset: u64) {
            let mut filled = 0;
            while filled < bytes.len() {
                let position = offset + filled as u64;
                let word = (position / 8).to_le_bytes();
                let start = (position % 8) as usize;
                let count = (8 - start).min(bytes.len() - filled);
                bytes[filled..filled + count].copy_from_slice(&word[start..start + count]);
                filled += count;
            }
        }

        const TOTAL: u64 = (272 << 20) + 113;
        const RESERVE: u64 = 512 << 20;
        let directory = TestDirectory::new();
        fs::create_dir_all(&directory.0).unwrap();
        assert!(
            available_space(&directory.0).unwrap() >= TOTAL + RESERVE + (16 << 20),
            "test needs a 272 MiB backlog plus the 512 MiB disk reserve and margin"
        );
        let space_directory = directory.0.clone();
        let mut options = directory.options(0);
        options.segment_bytes = 16 << 20;
        options.space_check = Some(Arc::new(move |bytes| {
            if available_space(&space_directory)? < RESERVE.saturating_add(bytes) {
                return Err(io::Error::other("disk free space cannot preserve 512 MiB"));
            }
            Ok(())
        }));
        let mut server = CaptureServer::bind_spooled(
            ListenConfig {
                queue_capacity: 32,
                ..config()
            },
            options,
            || {},
        )
        .unwrap();
        let stats = server.spool_stats().unwrap();
        let address = server.address();
        let started = Instant::now();
        let producer = thread::spawn(move || {
            let mut client = TcpStream::connect(address).unwrap();
            client
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut buffer = vec![0; CHUNK_SIZE];
            let mut offset = 0;
            while offset < TOTAL {
                let count = (TOTAL - offset).min(buffer.len() as u64) as usize;
                fill_pattern(&mut buffer[..count], offset);
                client.write_all(&buffer[..count]).unwrap();
                offset += count as u64;
            }
            client.shutdown(Shutdown::Write).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let status = stats.snapshot();
            assert!(status.last_failure.is_none(), "{status:?}");
            if producer.is_finished() && status.session == 1 && !status.connected {
                break;
            }
            assert!(Instant::now() < deadline, "stalled receiver: {status:?}");
            thread::sleep(Duration::from_millis(10));
        }
        producer.join().unwrap();
        let received_seconds = started.elapsed().as_secs_f64();
        let status = stats.snapshot();
        assert_eq!(status.received_bytes, TOTAL);
        assert_eq!(status.limit_bytes, 0);
        assert!(status.queued_bytes > 256 << 20);
        assert_eq!(status.retained_disk_bytes, TOTAL);
        let spool_directory = fs::read_dir(&directory.0)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let actual_disk_bytes: u64 = fs::read_dir(spool_directory)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "bin")
            })
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert_eq!(actual_disk_bytes, TOTAL);

        let mut expected = vec![0; CHUNK_SIZE];
        let mut delivered = 0u64;
        let mut connected = false;
        loop {
            match event(&server) {
                ReceiverEvent::Listening(observed) => assert_eq!(observed, address),
                ReceiverEvent::Connected { session, .. } => {
                    assert_eq!(session, 1);
                    assert!(!connected);
                    connected = true;
                }
                ReceiverEvent::Data { session, bytes } => {
                    assert!(connected);
                    assert_eq!(session, 1);
                    assert!(bytes.len() <= CHUNK_SIZE);
                    fill_pattern(&mut expected[..bytes.len()], delivered);
                    if let Some(index) = bytes
                        .iter()
                        .zip(&expected)
                        .position(|(actual, expected)| actual != expected)
                    {
                        panic!("payload differs at byte {}", delivered + index as u64);
                    }
                    delivered += bytes.len() as u64;
                }
                ReceiverEvent::Disconnected { session, bytes, .. } => {
                    assert_eq!(session, 1);
                    assert_eq!(bytes, TOTAL);
                    assert_eq!(delivered, TOTAL);
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        wait_for(|| stats.snapshot().retained_disk_bytes == 0);
        assert!(stats.snapshot().last_failure.is_none());
        assert_eq!(stats.snapshot().queued_bytes, 0);
        server.stop();
        assert!(fs::read_dir(&directory.0).unwrap().next().is_none());
        eprintln!(
            "real disk backlog {actual_disk_bytes} bytes, queued {} bytes, intake {received_seconds:.3}s, exact ordered drain and cleanup {:.3}s total",
            status.queued_bytes,
            started.elapsed().as_secs_f64()
        );
    }

    #[test]
    #[ignore = "requires PVD_SDK34_PRODUCER pointing to the official SDK sample executable"]
    fn official_sdk_finishes_while_the_decoder_consumer_is_stalled() {
        use std::process::{Command, Stdio};
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let producer = std::env::var_os("PVD_SDK34_PRODUCER").expect("set PVD_SDK34_PRODUCER");
        let directory = TestDirectory::new();
        let mut options = directory.options(128 << 20);
        options.segment_bytes = 4 << 20;
        let mut server = CaptureServer::bind_spooled(config(), options, || {}).unwrap();
        let stats = server.spool_stats().unwrap();
        let mut command = Command::new(producer);
        command
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &server.address().port().to_string(),
                "--frames",
                "10000",
                "--flags",
                "all",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let started = Instant::now();
        let mut child = Child(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "SDK producer blocked while decoder consumer was stalled"
            );
            thread::sleep(Duration::from_millis(5));
        };
        assert!(status.success());
        let producer_seconds = started.elapsed().as_secs_f64();
        wait_for(|| !stats.snapshot().connected);
        assert!(stats.snapshot().last_failure.is_none());
        let received = stats.snapshot().received_bytes;
        assert!(received > 16 << 20);
        assert!(stats.snapshot().queued_bytes > 0);
        let mut delivered = 0u64;
        loop {
            match event(&server) {
                ReceiverEvent::Data { session, bytes } => {
                    assert_eq!(session, 1);
                    delivered += bytes.len() as u64;
                }
                ReceiverEvent::Disconnected { bytes, .. } => {
                    assert_eq!(bytes, received);
                    break;
                }
                ReceiverEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert_eq!(delivered, received);
        server.stop();
        assert_eq!(stats.snapshot().retained_disk_bytes, 0);
        eprintln!(
            "SDK producer completed with decoder stalled: {producer_seconds:.3}s, {received} bytes; exact delivery count and spool cleanup passed"
        );
    }
}
