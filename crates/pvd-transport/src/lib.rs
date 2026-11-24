use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(40);
const CHUNK_SIZE: usize = 64 * 1024;

mod delivery;
mod spool;
mod spool_directory;
pub use delivery::EventReceiver;
use delivery::{DeliverySignal, Output};
pub use spool::{SpoolFailure, SpoolOptions, SpoolSnapshot, SpoolStats};
pub use spool_directory::{SpoolCleanupReport, cleanup_stale_spools};

#[derive(Clone, Debug)]
pub struct ListenConfig {
    pub address: SocketAddr,
    pub capture_directory: Option<PathBuf>,
    pub max_session_bytes: u64,
    pub queue_capacity: usize,
}

impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            address: "0.0.0.0:5425".parse().expect("literal socket address"),
            capture_directory: None,
            max_session_bytes: 512 * 1024 * 1024,
            queue_capacity: 32,
        }
    }
}

#[derive(Debug)]
pub enum ReceiverEvent {
    Listening(SocketAddr),
    Connected {
        session: u64,
        peer: SocketAddr,
    },
    CaptureStarted {
        session: u64,
        path: PathBuf,
    },
    Data {
        session: u64,
        bytes: Vec<u8>,
    },
    Disconnected {
        session: u64,
        bytes: u64,
        capture: Option<PathBuf>,
    },
    Failed {
        session: Option<u64>,
        message: String,
    },
}

pub struct CaptureServer {
    pub receiver: EventReceiver,
    address: SocketAddr,
    cancel: Arc<AtomicBool>,
    finish: Arc<AtomicBool>,
    intake_quiesced: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    spool_stats: Option<Arc<SpoolStats>>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    delivery_signal: Arc<DeliverySignal>,
}

#[derive(Clone)]
pub struct CaptureControl {
    cancel: Arc<AtomicBool>,
    finish: Arc<AtomicBool>,
    intake_quiesced: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    stats: Option<Arc<SpoolStats>>,
    delivery_signal: Arc<DeliverySignal>,
}

impl CaptureControl {
    /// After requesting stop/finish, true once socket intake and raw/spool
    /// writes have ended. Temporary-file deletion may still be running.
    pub fn intake_quiesced(&self) -> bool {
        self.intake_quiesced.load(Ordering::Acquire)
    }

    /// Stop intake while allowing already committed bytes to reach the consumer.
    pub fn request_finish(&self) {
        self.finish.store(true, Ordering::Release);
        if let Some(socket) = self.socket.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(stats) = &self.stats {
            stats.disconnected();
        }
    }

    pub fn request_stop(&self) {
        self.delivery_signal.stop(&self.cancel);
        if let Some(socket) = self.socket.lock().unwrap().take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Some(stats) = &self.stats {
            stats.disconnected();
        }
    }
}

struct IntakeCompletion(Arc<AtomicBool>);

impl Drop for IntakeCompletion {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl CaptureServer {
    pub fn bind(config: ListenConfig, wake: impl Fn() + Send + Sync + 'static) -> io::Result<Self> {
        if config.max_session_bytes == 0 || !(1..=1024).contains(&config.queue_capacity) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "capture limit must be positive and queue capacity must be 1..=1024",
            ));
        }
        if let Some(directory) = &config.capture_directory {
            fs::create_dir_all(directory)?;
        }
        let listener = TcpListener::bind(config.address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let finish = Arc::new(AtomicBool::new(false));
        let intake_quiesced = Arc::new(AtomicBool::new(false));
        let intake_completion = IntakeCompletion(intake_quiesced.clone());
        let finish_intake = finish.clone();
        let (output, receiver) = Output::channel(config.queue_capacity, cancel.clone(), wake);
        let delivery_signal = output.signal.clone();
        let socket = Arc::new(Mutex::new(None));
        let active_socket = socket.clone();
        let worker = thread::Builder::new()
            .name("pvd-receiver".into())
            .spawn(move || {
                let _intake_completion = intake_completion;
                if !output.send(ReceiverEvent::Listening(address)) {
                    return;
                }
                let mut session = 0;
                while !output.stopped() && !finish_intake.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, peer)) => {
                            {
                                let mut socket = active_socket.lock().unwrap();
                                if finish_intake.load(Ordering::Acquire) || output.stopped() {
                                    let _ = stream.shutdown(Shutdown::Both);
                                    break;
                                }
                                *socket = stream.try_clone().ok();
                            }
                            session += 1;
                            if !output.send(ReceiverEvent::Connected { session, peer }) {
                                break;
                            }
                            let result =
                                receive_session(stream, &config, session, &output, &finish_intake);
                            if let Some(socket) = active_socket.lock().unwrap().take() {
                                let _ = socket.shutdown(Shutdown::Both);
                            }
                            if let Err(error) = result
                                && !output.send(ReceiverEvent::Failed {
                                    session: Some(session),
                                    message: error.to_string(),
                                })
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::park_timeout(POLL)
                        }
                        Err(error) => {
                            output.send(ReceiverEvent::Failed {
                                session: None,
                                message: error.to_string(),
                            });
                            break;
                        }
                    }
                }
                if let Some(socket) = active_socket.lock().unwrap().take() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            })?;
        Ok(Self {
            receiver,
            address,
            cancel,
            finish,
            intake_quiesced,
            worker: Some(worker),
            spool_stats: None,
            socket,
            delivery_signal,
        })
    }

    pub fn spool_stats(&self) -> Option<Arc<SpoolStats>> {
        self.spool_stats.clone()
    }

    pub fn control(&self) -> CaptureControl {
        CaptureControl {
            cancel: self.cancel.clone(),
            finish: self.finish.clone(),
            intake_quiesced: self.intake_quiesced.clone(),
            socket: self.socket.clone(),
            stats: self.spool_stats.clone(),
            delivery_signal: self.delivery_signal.clone(),
        }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn stop(&mut self) {
        self.control().request_stop();
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

impl Drop for CaptureServer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct CaptureFile {
    writer: BufWriter<File>,
    partial: PathBuf,
    complete: PathBuf,
}

impl CaptureFile {
    fn create(directory: &Path, session: u64) -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("capture-{stamp}-{}-{session}.pvdstream", std::process::id());
        let complete = directory.join(name);
        let partial = complete.with_extension("pvdstream.partial");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial)?;
        Ok(Self {
            writer: BufWriter::new(file),
            partial,
            complete,
        })
    }

    fn finish(mut self) -> io::Result<PathBuf> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        drop(self.writer);
        fs::rename(&self.partial, &self.complete)?;
        Ok(self.complete)
    }
}

fn configure_receive_stream(stream: &TcpStream) -> io::Result<()> {
    // Windows accepted sockets inherit the listener's nonblocking mode.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))
}

fn receive_session(
    mut stream: TcpStream,
    config: &ListenConfig,
    session: u64,
    output: &Output,
    finish: &AtomicBool,
) -> io::Result<()> {
    configure_receive_stream(&stream)?;
    let mut capture = config
        .capture_directory
        .as_deref()
        .map(|dir| CaptureFile::create(dir, session))
        .transpose()?;
    if let Some(file) = &capture
        && !output.send(ReceiverEvent::CaptureStarted {
            session,
            path: file.partial.clone(),
        })
    {
        return Ok(());
    }
    let mut total = 0u64;
    let mut buffer = vec![0u8; CHUNK_SIZE];
    while !output.stopped() {
        let received = if finish.load(Ordering::Acquire) {
            Ok(0)
        } else {
            stream.read(&mut buffer)
        };
        let received = match received {
            Err(_) if finish.load(Ordering::Acquire) => Ok(0),
            value => value,
        };
        match received {
            Ok(0) => {
                let path = capture.map(CaptureFile::finish).transpose()?;
                output.send(ReceiverEvent::Disconnected {
                    session,
                    bytes: total,
                    capture: path,
                });
                return Ok(());
            }
            Ok(count) => {
                total = total
                    .checked_add(count as u64)
                    .ok_or_else(|| io::Error::other("capture size overflow"))?;
                if total > config.max_session_bytes {
                    return Err(io::Error::other(format!(
                        "session {session} exceeds capture limit of {} bytes; partial capture retained",
                        config.max_session_bytes
                    )));
                }
                if let Some(file) = &mut capture {
                    file.writer.write_all(&buffer[..count])?;
                }
                if !output.send(ReceiverEvent::Data {
                    session,
                    bytes: buffer[..count].to_vec(),
                }) {
                    break;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(mut file) = capture {
        file.writer.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn config() -> ListenConfig {
        ListenConfig {
            address: "127.0.0.1:0".parse().unwrap(),
            ..Default::default()
        }
    }

    fn collect(server: &CaptureServer) -> (u64, Vec<u8>, Option<PathBuf>) {
        let mut data = Vec::new();
        let mut id = 0;
        loop {
            match server
                .receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
            {
                ReceiverEvent::Data { session, bytes } => {
                    id = session;
                    data.extend(bytes);
                }
                ReceiverEvent::Disconnected {
                    session,
                    bytes,
                    capture,
                } => {
                    assert_eq!(session, id);
                    assert_eq!(bytes, data.len() as u64);
                    return (id, data, capture);
                }
                ReceiverEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
    }

    #[test]
    fn fragmented_bytes_and_reconnect_are_lossless_and_separate() {
        let server = CaptureServer::bind(config(), || {}).unwrap();
        for session in 1..=2 {
            let bytes: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
            let address = server.address();
            let expected = bytes.clone();
            let sender = thread::spawn(move || {
                let mut stream = TcpStream::connect(address).unwrap();
                for chunk in bytes.chunks(271) {
                    stream.write_all(chunk).unwrap();
                }
            });
            let (id, actual, _) = collect(&server);
            sender.join().unwrap();
            assert_eq!(id, session);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn cancelling_full_queue_does_not_deadlock() {
        let mut server = CaptureServer::bind(
            ListenConfig {
                queue_capacity: 1,
                ..config()
            },
            || {},
        )
        .unwrap();
        let mut stream = TcpStream::connect(server.address()).unwrap();
        stream.write_all(&[1; 8192]).unwrap();
        let started = Instant::now();
        server.stop();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn capture_is_exact_and_published_after_disconnect() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("pvd-transport-{}-{stamp}", std::process::id()));
        let server = CaptureServer::bind(
            ListenConfig {
                capture_directory: Some(directory.clone()),
                ..config()
            },
            || {},
        )
        .unwrap();
        let payload = b"raw stream bytes including \0\xff";
        let mut stream = TcpStream::connect(server.address()).unwrap();
        let partial = loop {
            if let ReceiverEvent::CaptureStarted { session, path } = server
                .receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
            {
                assert_eq!(session, 1);
                break path;
            }
        };
        assert!(partial.exists());
        assert!(!partial.with_extension("").exists());
        stream.write_all(payload).unwrap();
        drop(stream);
        let (_, data, path) = collect(&server);
        assert_eq!(data, payload);
        let path = path.unwrap();
        assert_eq!(path, partial.with_extension(""));
        assert!(!partial.exists());
        assert_eq!(fs::read(&path).unwrap(), payload);
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn oversized_session_reports_failure() {
        let server = CaptureServer::bind(
            ListenConfig {
                max_session_bytes: 16,
                ..config()
            },
            || {},
        )
        .unwrap();
        let mut stream = TcpStream::connect(server.address()).unwrap();
        stream.write_all(&[0; 32]).unwrap();
        loop {
            if let ReceiverEvent::Failed { message, .. } = server
                .receiver
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
            {
                assert!(message.contains("capture limit"));
                break;
            }
        }
    }
}
