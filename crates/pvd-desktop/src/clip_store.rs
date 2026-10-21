use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{clips::ClipSet, i18n::Message};

pub const MAX_CLIPS: usize = 64;
pub const MAX_NAME_CHARS: usize = 80;
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    algorithm: String,
    digest: String,
    byte_count: u64,
    frame_count: usize,
}

impl Identity {
    pub fn from_digest(digest: blake3::Hash, byte_count: u64, frame_count: usize) -> Self {
        Self {
            algorithm: "BLAKE3".into(),
            digest: digest.to_hex().to_string(),
            byte_count,
            frame_count,
        }
    }

    pub fn matches(&self, digest: [u8; 32], byte_count: u64, frame_count: usize) -> bool {
        self.byte_count == byte_count
            && self.frame_count == frame_count
            && self.digest == blake3::Hash::from(digest).to_hex().as_str()
    }

    pub fn fingerprint(&self) -> Option<(blake3::Hash, u64, usize)> {
        (self.algorithm == "BLAKE3").then_some(())?;
        Some((
            blake3::Hash::from_hex(&self.digest).ok()?,
            self.byte_count,
            self.frame_count,
        ))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    format: String,
    version: u32,
    capture: Identity,
    definitions: ClipSet,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub capture_path: PathBuf,
    pub identity: Identity,
    revision: Option<blake3::Hash>,
}

pub struct Loaded {
    pub binding: Binding,
    pub definitions: ClipSet,
    pub existed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Cancelled,
    Io(String),
    Invalid(String),
    CaptureChanged,
    IncompleteCapture,
    SidecarChanged,
    Busy,
}

impl Error {
    pub fn message(&self) -> Message {
        match self {
            Self::Cancelled => {
                Message::new("Clip definition operation cancelled", "片段定义操作已取消")
            }
            Self::Io(detail) => {
                Message::detail("Clip definition file error", "片段定义文件错误", detail)
            }
            Self::Invalid(detail) => Message::detail(
                "Clip definitions are invalid; the sidecar was preserved",
                "片段定义无效；已保留原附属文件",
                detail,
            ),
            Self::CaptureChanged => Message::new(
                "Clip definitions do not match the capture contents; reopen the correct recording. No definitions were applied or saved.",
                "片段定义与录制内容不匹配，请重新打开正确录制。未应用或保存这些定义。",
            ),
            Self::IncompleteCapture => Message::new(
                "Clip definitions require a completed recording; .partial captures are not eligible.",
                "片段定义需要完整保存的录制；不能绑定 .partial 捕获。",
            ),
            Self::SidecarChanged => Message::new(
                "The clip sidecar changed outside this session; reopen the recording before saving. Existing definitions were preserved.",
                "片段附属文件已被其他会话修改，请重新打开录制后再保存。已有定义已保留。",
            ),
            Self::Busy => Message::new(
                "The clip sidecar is being written or has a leftover .lock file. Retry after the writer finishes; inspect a stale lock before removing it.",
                "片段附属文件正在写入或存在遗留 .lock 文件。请等待写入完成再试；移除遗留锁前请先确认没有其他写入进程。",
            ),
        }
    }
}

fn io(error: std::io::Error) -> Error {
    Error::Io(error.to_string())
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

pub fn sidecar_path(capture: &Path) -> PathBuf {
    let mut name = capture.as_os_str().to_owned();
    name.push(".pvdstudio-clips.json");
    PathBuf::from(name)
}

pub fn identity(bytes: &[u8], frame_count: usize, cancel: &AtomicBool) -> Result<Identity, Error> {
    let mut hasher = blake3::Hasher::new();
    for chunk in bytes.chunks(64 * 1024) {
        check_cancel(cancel)?;
        hasher.update(chunk);
    }
    check_cancel(cancel)?;
    Ok(Identity::from_digest(
        hasher.finalize(),
        bytes.len() as u64,
        frame_count,
    ))
}

fn validate(definitions: &ClipSet, frames: usize) -> Result<(), Error> {
    if definitions.clips.len() > MAX_CLIPS {
        return Err(Error::Invalid("more than 64 clip ranges".into()));
    }
    if definitions.selected >= definitions.clips.len().max(1) {
        return Err(Error::Invalid(
            "selected clip index is out of bounds".into(),
        ));
    }
    for clip in &definitions.clips {
        if clip.first > clip.last || clip.last >= frames {
            return Err(Error::Invalid(
                "clip range is outside the capture frame count".into(),
            ));
        }
        if clip.name.chars().count() > MAX_NAME_CHARS || clip.name.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "clip names allow at most 80 characters and no control characters".into(),
            ));
        }
    }
    Ok(())
}

fn read_sidecar(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SIDECAR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(Error::Invalid("sidecar exceeds 64 KiB".into()));
    }
    Ok(Some(bytes))
}

pub fn load(
    capture_path: &Path,
    bytes: &[u8],
    frame_count: usize,
    cancel: &AtomicBool,
) -> Result<Loaded, Error> {
    load_identified(capture_path, identity(bytes, frame_count, cancel)?, cancel)
}

pub fn load_identified(
    capture_path: &Path,
    expected: Identity,
    cancel: &AtomicBool,
) -> Result<Loaded, Error> {
    check_cancel(cancel)?;
    if capture_path
        .extension()
        .is_some_and(|extension| extension == "partial")
    {
        return Err(Error::IncompleteCapture);
    }
    let capture_path = fs::canonicalize(capture_path).map_err(io)?;
    let contents = read_sidecar(&sidecar_path(&capture_path))?;
    check_cancel(cancel)?;
    let existed = contents.is_some();
    let (definitions, revision) = if let Some(contents) = contents {
        let document: Document =
            serde_json::from_slice(&contents).map_err(|error| Error::Invalid(error.to_string()))?;
        if document.format != "pvd-studio.clip-definitions" || document.version != 1 {
            return Err(Error::Invalid(
                "unsupported sidecar format or version".into(),
            ));
        }
        if document.capture != expected {
            return Err(Error::CaptureChanged);
        }
        validate(&document.definitions, expected.frame_count)?;
        (document.definitions, Some(blake3::hash(&contents)))
    } else {
        (ClipSet::default(), None)
    };
    Ok(Loaded {
        binding: Binding {
            capture_path,
            identity: expected,
            revision,
        },
        definitions,
        existed,
    })
}

fn verify_capture(binding: &Binding, cancel: &AtomicBool) -> Result<(), Error> {
    let mut file = File::open(&binding.capture_path).map_err(io)?;
    if file.metadata().map_err(io)?.len() != binding.identity.byte_count {
        return Err(Error::CaptureChanged);
    }
    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer).map_err(io)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > binding.identity.byte_count {
            return Err(Error::CaptureChanged);
        }
        hasher.update(&buffer[..count]);
    }
    if total != binding.identity.byte_count
        || hasher.finalize().to_hex().as_str() != binding.identity.digest
    {
        return Err(Error::CaptureChanged);
    }
    Ok(())
}

struct OwnedFile(PathBuf);

impl Drop for OwnedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(target_os = "windows")]
fn publish_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{MOVE_FILE_FLAGS, MoveFileExW},
        core::PCWSTR,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // Both paths stay alive and terminated for the call; no replacement flag protects a new sidecar.
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVE_FILE_FLAGS(0),
        )
    }
    .map_err(|error| std::io::Error::from_raw_os_error(error.code().0 & 0xffff))
}

#[cfg(not(target_os = "windows"))]
fn publish_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(source, destination)
}

pub fn save(
    binding: &Binding,
    definitions: &ClipSet,
    cancel: &AtomicBool,
) -> Result<Binding, Error> {
    validate(definitions, binding.identity.frame_count)?;
    check_cancel(cancel)?;
    let path = sidecar_path(&binding.capture_path);
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let lock = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::Busy
            } else {
                io(error)
            }
        })?;
    let _lock_guard = OwnedFile(lock_path);
    drop(lock);
    verify_capture(binding, cancel)?;
    let current = read_sidecar(&path)?.map(|bytes| blake3::hash(&bytes));
    if current != binding.revision {
        return Err(Error::SidecarChanged);
    }
    let contents = serde_json::to_vec_pretty(&Document {
        format: "pvd-studio.clip-definitions".into(),
        version: 1,
        capture: binding.identity.clone(),
        definitions: definitions.clone(),
    })
    .map_err(|error| Error::Invalid(error.to_string()))?;
    if contents.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(Error::Invalid("sidecar exceeds 64 KiB".into()));
    }
    let mut temp_path = path.as_os_str().to_owned();
    temp_path.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp_path = PathBuf::from(temp_path);
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(io)?;
    let _temp_guard = OwnedFile(temp_path.clone());
    temp.write_all(&contents)
        .and_then(|()| temp.sync_all())
        .map_err(io)?;
    drop(temp);
    check_cancel(cancel)?;
    if read_sidecar(&path)?.map(|bytes| blake3::hash(&bytes)) != binding.revision {
        return Err(Error::SidecarChanged);
    }
    if binding.revision.is_some() {
        fs::rename(&temp_path, &path).map_err(io)?;
    } else {
        publish_new(&temp_path, &path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::SidecarChanged
            } else {
                io(error)
            }
        })?;
    }
    let mut result = binding.clone();
    result.revision = Some(blake3::hash(&contents));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{clips::Clip, i18n::Language, studio::Studio};
    use std::time::{Duration, Instant};

    struct Scratch {
        directory: PathBuf,
        capture: PathBuf,
    }

    impl Scratch {
        fn new(bytes: &[u8]) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "pvd-studio-clip-store-{}-{}",
                std::process::id(),
                TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&directory).unwrap();
            let capture = directory.join("录制.pvdstream");
            fs::write(&capture, bytes).unwrap();
            Self { directory, capture }
        }
        fn entries(&self) -> usize {
            fs::read_dir(&self.directory).unwrap().count()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for entry in fs::read_dir(&self.directory).unwrap() {
                fs::remove_file(entry.unwrap().path()).unwrap();
            }
            fs::remove_dir(&self.directory).unwrap();
        }
    }

    fn ranges() -> ClipSet {
        let mut definitions = ClipSet::default();
        assert!(definitions.add(10, 30));
        definitions.clips[0].name = "落地 / impact".into();
        assert!(definitions.split(20));
        definitions.selected = 1;
        definitions
    }

    #[test]
    fn clip_store_roundtrip_and_removal_preserve_capture_bytes() {
        let bytes = b"content identity test fixture";
        let scratch = Scratch::new(bytes);
        let cancel = AtomicBool::new(false);
        let loaded = load(&scratch.capture, bytes, 40, &cancel).unwrap();
        assert!(!loaded.existed);
        assert!(loaded.definitions.clips.is_empty());
        let definitions = ranges();
        let binding = save(&loaded.binding, &definitions, &cancel).unwrap();
        assert_eq!(scratch.entries(), 2);
        let reopened = load(&scratch.capture, bytes, 40, &cancel).unwrap();
        assert!(reopened.existed);
        assert_eq!(reopened.definitions, definitions);
        assert_eq!(reopened.binding.revision, binding.revision);
        save(&binding, &ClipSet::default(), &cancel).unwrap();
        assert!(
            load(&scratch.capture, bytes, 40, &cancel)
                .unwrap()
                .definitions
                .clips
                .is_empty()
        );
        assert_eq!(fs::read(&scratch.capture).unwrap(), bytes);
        assert_eq!(scratch.entries(), 2);
    }

    #[test]
    fn clip_store_identity_checks_full_content_and_frame_count() {
        let scratch = Scratch::new(b"source A");
        let cancel = AtomicBool::new(false);
        let original = load(&scratch.capture, b"source A", 40, &cancel).unwrap();
        let binding = save(&original.binding, &ranges(), &cancel).unwrap();
        let sidecar = fs::read(sidecar_path(&scratch.capture)).unwrap();
        fs::write(&scratch.capture, b"source B").unwrap();
        assert!(matches!(
            load(&scratch.capture, b"source B", 40, &cancel),
            Err(Error::CaptureChanged)
        ));
        assert!(matches!(
            load(&scratch.capture, b"source A", 41, &cancel),
            Err(Error::CaptureChanged)
        ));
        assert!(matches!(
            save(&binding, &ClipSet::default(), &cancel),
            Err(Error::CaptureChanged)
        ));
        assert_eq!(fs::read(sidecar_path(&scratch.capture)).unwrap(), sidecar);
        assert_eq!(scratch.entries(), 2);
    }

    #[test]
    fn clip_store_external_edits_and_stale_writers_do_not_overwrite() {
        let bytes = b"source";
        let scratch = Scratch::new(bytes);
        let cancel = AtomicBool::new(false);
        let first = load(&scratch.capture, bytes, 40, &cancel).unwrap();
        let second = load(&scratch.capture, bytes, 40, &cancel).unwrap();
        let binding = save(&first.binding, &ranges(), &cancel).unwrap();
        let path = sidecar_path(&scratch.capture);
        let saved = fs::read(&path).unwrap();
        assert!(matches!(
            save(&second.binding, &ClipSet::default(), &cancel),
            Err(Error::SidecarChanged)
        ));
        assert_eq!(fs::read(&path).unwrap(), saved);
        fs::write(&path, b"external edit").unwrap();
        assert!(matches!(
            save(&binding, &ranges(), &cancel),
            Err(Error::SidecarChanged)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"external edit");
        assert_eq!(scratch.entries(), 2);
    }

    #[test]
    fn clip_store_cancel_and_lock_leave_existing_files_untouched() {
        let bytes = b"source";
        let scratch = Scratch::new(bytes);
        let cancel = AtomicBool::new(false);
        let loaded = load(&scratch.capture, bytes, 40, &cancel).unwrap();
        let path = sidecar_path(&scratch.capture);
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            load(&scratch.capture, bytes, 40, &cancel),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            save(&loaded.binding, &ranges(), &cancel),
            Err(Error::Cancelled)
        ));
        assert_eq!(scratch.entries(), 1);
        cancel.store(false, Ordering::Relaxed);
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        fs::write(&lock_path, b"other writer").unwrap();
        assert!(matches!(
            save(&loaded.binding, &ranges(), &cancel),
            Err(Error::Busy)
        ));
        assert_eq!(fs::read(&lock_path).unwrap(), b"other writer");
        assert!(!path.exists());
        fs::remove_file(lock_path).unwrap();
        let binding = save(&loaded.binding, &ranges(), &cancel).unwrap();
        let contents = fs::read(&path).unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            save(&binding, &ClipSet::default(), &cancel),
            Err(Error::Cancelled)
        ));
        assert_eq!(fs::read(&path).unwrap(), contents);
        assert_eq!(scratch.entries(), 2);
    }

    #[test]
    fn clip_store_first_publication_never_replaces_an_existing_path() {
        let scratch = Scratch::new(b"source");
        let target = scratch.directory.join("target.json");
        let temporary = scratch.directory.join("temporary.json");
        fs::write(&target, b"existing").unwrap();
        fs::write(&temporary, b"replacement").unwrap();
        assert_eq!(
            publish_new(&temporary, &target).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert_eq!(fs::read(&temporary).unwrap(), b"replacement");
    }

    #[test]
    fn clip_store_partial_capture_cannot_acquire_a_definition_binding() {
        let scratch = Scratch::new(b"source");
        let partial = scratch.directory.join("capture.pvdstream.partial");
        fs::write(&partial, b"source").unwrap();
        assert!(matches!(
            load(&partial, b"source", 40, &AtomicBool::new(false)),
            Err(Error::IncompleteCapture)
        ));
        assert!(!sidecar_path(&partial).exists());
    }

    #[test]
    fn clip_store_rejects_invalid_ranges_names_and_selected_index() {
        let mut definitions = ClipSet::default();
        for _ in 0..MAX_CLIPS {
            assert!(definitions.add(0, 0));
        }
        definitions.clips[0].name = "界".repeat(MAX_NAME_CHARS);
        validate(&definitions, 1).unwrap();
        let mut invalid = definitions.clone();
        invalid.clips.push(Clip {
            name: "x".into(),
            first: 0,
            last: 0,
        });
        assert!(validate(&invalid, 1).is_err());
        for (first, last, name) in [
            (1, 0, "ok".into()),
            (0, 1, "ok".into()),
            (0, 0, "界".repeat(81)),
            (0, 0, "line\nbreak".into()),
        ] {
            let invalid = ClipSet {
                clips: vec![Clip { name, first, last }],
                selected: 0,
            };
            assert!(validate(&invalid, 1).is_err());
        }
        definitions.selected = MAX_CLIPS;
        assert!(validate(&definitions, 1).is_err());
        assert!(
            validate(
                &ClipSet {
                    selected: 1,
                    ..Default::default()
                },
                0
            )
            .is_err()
        );
        validate(&ClipSet::default(), 0).unwrap();
    }

    #[test]
    fn clip_store_rejects_corrupt_oversized_and_unknown_documents_without_rewriting() {
        let bytes = b"source";
        let scratch = Scratch::new(bytes);
        let cancel = AtomicBool::new(false);
        let loaded = load(&scratch.capture, bytes, 40, &cancel).unwrap();
        save(&loaded.binding, &ranges(), &cancel).unwrap();
        let path = sidecar_path(&scratch.capture);
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let mut malformed = vec![b"{".to_vec(), vec![b' '; MAX_SIDECAR_BYTES as usize + 1]];
        for (pointer, value) in [
            ("/version", serde_json::json!(2)),
            ("/format", serde_json::json!("pxd")),
            ("/definitions/selected", serde_json::json!(99)),
            ("/definitions/clips/0/last", serde_json::json!(100)),
            ("/definitions/clips/0/first", serde_json::json!(-1)),
        ] {
            let mut document = original.clone();
            *document.pointer_mut(pointer).unwrap() = value;
            malformed.push(serde_json::to_vec(&document).unwrap());
        }
        let mut document = original.clone();
        document["definitions"]["unexpected"] = true.into();
        malformed.push(serde_json::to_vec(&document).unwrap());
        for contents in malformed {
            fs::write(&path, &contents).unwrap();
            assert!(matches!(
                load(&scratch.capture, bytes, 40, &cancel),
                Err(Error::Invalid(_))
            ));
            assert_eq!(fs::read(&path).unwrap(), contents);
        }
        assert_eq!(fs::read(&scratch.capture).unwrap(), bytes);
    }

    fn wait_for_studio(studio: &mut Studio, context: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while studio.job.is_some() || studio.clip_persistence.busy() {
            assert!(
                Instant::now() < deadline,
                "clip persistence worker timed out"
            );
            studio.poll(context);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    #[ignore = "Requires official PhysX 3.4.2 baseline and rich captures"]
    fn clip_store_real_sdk_reopen_replace_cancel_and_tamper() {
        let fixture = |variable, filename| {
            let path = std::env::var_os(variable)
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../captures")
                        .join(filename)
                });
            let bytes = fs::read(path).unwrap();
            let recording = pvd_core::Recording::from_bytes(&bytes).unwrap();
            assert_eq!(recording.frame_count(), 120);
            bytes
        };
        let baseline = fixture("PVD_SDK34_CAPTURE", "physx-3.4.pvdstream");
        let rich = fixture("PVD_SDK34_RICH_CAPTURE", "physx-3.4-rich.pvdstream");
        assert_ne!(blake3::hash(&baseline), blake3::hash(&rich));
        let scratch = Scratch::new(&baseline);
        let context = egui::Context::default();
        let mut studio = Studio::default();
        studio.open(scratch.capture.clone(), context.clone());
        wait_for_studio(&mut studio, &context);
        assert_eq!(studio.count(), 120);
        assert!(studio.clip_persistence.binding.is_some());
        studio.clip_set = ranges();
        studio
            .clip_persistence
            .save(&studio.clip_set, context.clone(), &studio.background_jobs);
        wait_for_studio(&mut studio, &context);
        assert!(!studio.clip_persistence.dirty(&studio.clip_set));
        studio.close();
        for _ in 0..3 {
            studio.open(scratch.capture.clone(), context.clone());
            wait_for_studio(&mut studio, &context);
            assert_eq!(studio.clip_set, ranges());
            studio.seek(20);
            assert!(!studio.state.objects.is_empty());
            studio.close();
            assert!(studio.clip_set.clips.is_empty());
        }
        studio.open(scratch.capture.clone(), context.clone());
        studio.close();
        std::thread::sleep(Duration::from_millis(100));
        studio.poll(&context);
        assert!(studio.recording.is_none());
        assert!(studio.clip_persistence.binding.is_none());
        let path = sidecar_path(&scratch.capture);
        let original_sidecar = fs::read(&path).unwrap();
        fs::write(&scratch.capture, &rich).unwrap();
        studio.open(scratch.capture.clone(), context.clone());
        wait_for_studio(&mut studio, &context);
        assert_eq!(studio.count(), 120);
        assert!(studio.clip_set.clips.is_empty());
        assert!(studio.clip_persistence.binding.is_none());
        assert!(
            studio
                .clip_persistence
                .issue
                .as_ref()
                .unwrap()
                .text(Language::English)
                .contains("do not match")
        );
        studio.close();
        fs::write(&scratch.capture, &baseline).unwrap();
        fs::write(&path, b"{tampered").unwrap();
        studio.open(scratch.capture.clone(), context.clone());
        wait_for_studio(&mut studio, &context);
        assert_eq!(studio.count(), 120);
        assert!(studio.clip_set.clips.is_empty());
        assert!(studio.clip_persistence.binding.is_none());
        assert!(
            studio
                .clip_persistence
                .issue
                .as_ref()
                .unwrap()
                .text(Language::Chinese)
                .contains("无效")
        );
        assert_eq!(fs::read(&path).unwrap(), b"{tampered");
        assert_eq!(fs::read(&scratch.capture).unwrap(), baseline);
        studio.close();
        fs::write(&path, original_sidecar).unwrap();
        studio.open(scratch.capture.clone(), context.clone());
        wait_for_studio(&mut studio, &context);
        assert_eq!(studio.clip_set, ranges());
    }
}
