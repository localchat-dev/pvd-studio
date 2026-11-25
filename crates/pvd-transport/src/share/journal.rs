use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender},
};
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use super::{HEARTBEAT, QUEUE_BYTES};

const MAX_PREFIX_BYTES: u64 = 512 << 20;

pub(super) struct Journal {
    reader: File,
    progress: Mutex<Progress>,
    changed: Condvar,
}

#[derive(Default)]
struct Progress {
    bytes: u64,
    complete: Option<bool>,
}

impl Journal {
    fn commit(&self, written: u64) {
        self.progress.lock().unwrap().bytes = written;
        self.changed.notify_one();
    }

    fn finish(&self, complete: bool) {
        let mut progress = self.progress.lock().unwrap();
        if progress.complete.is_none() {
            progress.complete = Some(complete);
        }
        self.changed.notify_one();
    }

    pub(super) fn available(&self) -> bool {
        self.progress.lock().unwrap().complete != Some(false)
    }

    pub(super) fn read(
        &self,
        offset: u64,
        buffer: &mut [u8],
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<usize> {
        let mut progress = self.progress.lock().unwrap();
        while progress.bytes == offset && progress.complete.is_none() && !stopped() {
            let timeout;
            (progress, timeout) = self.changed.wait_timeout(progress, HEARTBEAT).unwrap();
            if timeout.timed_out() && progress.bytes == offset && progress.complete.is_none() {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
        }
        if stopped() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if progress.complete == Some(false) {
            return Err(io::Error::other(
                "shared PVD prefix ended before the complete stream",
            ));
        }
        let count = (progress.bytes - offset).min(buffer.len() as u64) as usize;
        drop(progress);
        if count == 0 {
            return Ok(0);
        }
        let read = self.reader.seek_read(&mut buffer[..count], offset)?;
        if read == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        Ok(read)
    }
}

pub(super) struct JournalWriter {
    pub(super) journal: Arc<Journal>,
    sender: SyncSender<Vec<u8>>,
    complete: Arc<AtomicBool>,
    queued_bytes: u64,
}

impl JournalWriter {
    pub(super) fn start(
        directory: &Path,
        epoch: u64,
        sequence: u64,
    ) -> io::Result<(Self, JoinHandle<()>)> {
        fs::create_dir_all(directory)?;
        let path = directory.join(format!("pvd-share-{epoch:016x}-{sequence:016x}.tmp"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(&path)?;
        let journal = Arc::new(Journal {
            reader: File::open(path)?,
            progress: Mutex::default(),
            changed: Condvar::new(),
        });
        let progress = journal.clone();
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(QUEUE_BYTES / crate::CHUNK_SIZE);
        let complete = Arc::new(AtomicBool::new(false));
        let finished = complete.clone();
        let worker = thread::Builder::new()
            .name("pvd-share-prefix".into())
            .spawn(move || {
                let mut file = file;
                let mut written = 0;
                for bytes in receiver {
                    if !progress.available() || file.write_all(&bytes).is_err() {
                        progress.finish(false);
                        return;
                    }
                    written += bytes.len() as u64;
                    progress.commit(written);
                }
                progress.finish(finished.load(Ordering::Acquire));
            })?;
        Ok((
            Self {
                journal,
                sender,
                complete,
                queued_bytes: 0,
            },
            worker,
        ))
    }

    pub(super) fn append(&mut self, bytes: &[u8]) -> bool {
        let next = self.queued_bytes + bytes.len() as u64;
        if next > MAX_PREFIX_BYTES
            || !self.journal.available()
            || self.sender.try_send(bytes.to_vec()).is_err()
        {
            self.journal.finish(false);
            return false;
        }
        self.queued_bytes = next;
        true
    }

    pub(super) fn finish(self, complete: bool) {
        self.complete.store(complete, Ordering::Release);
        if !complete {
            self.journal.finish(false);
        }
    }
}
