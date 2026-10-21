use crate::Result;
use pvd_core::{
    LiveRecording, Recording,
    analysis::ProfileZone,
    live::HistoryConfig,
    profile_curves::{self, CatalogEntry, ExactKey, Point, ReferenceScene, SourceIdentity},
};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
    pub zones: BTreeMap<u64, ProfileZone>,
}

pub struct Capture {
    pub recording: Recording,
    source: Option<(File, SourceIdentity)>,
}

fn check_cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(
            io::Error::new(io::ErrorKind::Interrupted, "profile operation cancelled").into(),
        );
    }
    Ok(())
}

fn progress<'a>(label: &'static str, cancel: &'a AtomicBool) -> impl FnMut(u64, u64) -> bool + 'a {
    let mut updated = None;
    move |read, total| {
        if updated.is_none_or(|time: Instant| time.elapsed() >= Duration::from_secs(1)) {
            eprintln!("{label}: {read} / {total} bytes");
            updated = Some(Instant::now());
        }
        !cancel.load(Ordering::Relaxed)
    }
}

fn validate_history(recording: &Recording) -> Result<()> {
    let stats = recording
        .disk_usage()
        .ok_or("profile source has no disk history")?;
    if stats.first_frame != 0
        || stats.retained_frames != recording.frame_count()
        || stats.warning.is_some()
    {
        return Err(format!(
            "the complete capture exceeds the profile command's disk history budget: {}",
            stats.warning.unwrap_or_default()
        )
        .into());
    }
    Ok(())
}

fn load_sdk(mut file: File, history: HistoryConfig, cancel: &AtomicBool) -> Result<Capture> {
    check_cancelled(cancel)?;
    file.rewind()?;
    let total = file.metadata()?.len();
    let mut live = LiveRecording::with_history(history)?;
    let mut digest = blake3::Hasher::new();
    let mut bytes = [0; 64 << 10];
    let mut received = 0u64;
    let mut checked = 0u64;
    let mut report = progress("indexing complete profile source", cancel);
    loop {
        if !report(received, total) {
            check_cancelled(cancel)?;
        }
        let length = match file.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            break;
        }
        received = received
            .checked_add(length as u64)
            .ok_or("profile source size overflow")?;
        if received > total {
            return Err("profile source grew while indexing".into());
        }
        digest.update(&bytes[..length]);
        live.feed(&bytes[..length])?;
        let _ = live.take_update();
        if live.frame_count() > 0 && received.saturating_sub(checked) >= 4 << 20 {
            validate_history(&live.snapshot()?)?;
            checked = received;
        }
    }
    if received != total {
        return Err("profile source changed length while indexing".into());
    }
    check_cancelled(cancel)?;
    let recording = live.finish()?;
    validate_history(&recording)?;
    let identity = SourceIdentity {
        blake3: *digest.finalize().as_bytes(),
        bytes: received,
        frames: recording.frame_count(),
    };
    Ok(Capture {
        recording,
        source: Some((file, identity)),
    })
}

pub fn catalog(path: &str, cancel: &AtomicBool) -> Result<Catalog> {
    crate::project_capture::require_full_source(path)?;
    check_cancelled(cancel)?;
    if crate::is_native_capture(path)? {
        let recording = Recording::from_path_with_progress(
            path,
            progress("loading native profile source", cancel),
        )?;
        crate::report_diagnostics(&recording);
        return Ok(Catalog {
            entries: profile_curves::catalog(&recording, cancel)?,
            zones: recording
                .analysis
                .legacy()
                .ok_or("native profile catalog requires legacy analysis storage")?
                .zones
                .clone(),
        });
    }
    let mut file = File::open(path)?;
    let mut report = progress("indexing complete profile catalog", cancel);
    let catalog = profile_curves::catalog_streaming(&mut file, cancel, |status| {
        report(status.bytes_read, status.total_bytes)
    })?;
    Ok(Catalog {
        entries: catalog.entries,
        zones: catalog.zones,
    })
}

impl Capture {
    pub fn open(path: &str, cancel: &AtomicBool) -> Result<Self> {
        crate::project_capture::require_full_source(path)?;
        check_cancelled(cancel)?;
        if crate::is_native_capture(path)? {
            Ok(Self {
                recording: Recording::from_path_with_progress(
                    path,
                    progress("loading native profile source", cancel),
                )?,
                source: None,
            })
        } else {
            load_sdk(File::open(path)?, HistoryConfig::default(), cancel)
        }
    }

    pub fn catalog(&mut self, cancel: &AtomicBool) -> Result<Catalog> {
        let Some((file, expected)) = &mut self.source else {
            return Ok(Catalog {
                entries: profile_curves::catalog(&self.recording, cancel)?,
                zones: self
                    .recording
                    .analysis
                    .legacy()
                    .ok_or("native profile catalog requires legacy analysis storage")?
                    .zones
                    .clone(),
            });
        };
        let mut report = progress("indexing complete profile catalog", cancel);
        let catalog = profile_curves::catalog_streaming(file, cancel, |status| {
            report(status.bytes_read, status.total_bytes)
        })?;
        validate_identity(expected, &catalog.identity)?;
        Ok(Catalog {
            entries: catalog.entries,
            zones: catalog.zones,
        })
    }

    pub fn catalog_key(&mut self, key: ExactKey, cancel: &AtomicBool) -> Result<Catalog> {
        let Some((file, expected)) = &mut self.source else {
            let mut catalog = self.catalog(cancel)?;
            catalog.entries.retain(|entry| entry.key == key);
            return Ok(catalog);
        };
        let mut report = progress("indexing exact profile key", cancel);
        let catalog = profile_curves::catalog_key_streaming(file, key, cancel, |status| {
            report(status.bytes_read, status.total_bytes)
        })?;
        validate_identity(expected, &catalog.identity)?;
        Ok(Catalog {
            entries: catalog.entries,
            zones: catalog.zones,
        })
    }

    pub fn sample(
        &mut self,
        key: ExactKey,
        scene: ReferenceScene,
        frames: &[usize],
        cancel: &AtomicBool,
    ) -> Result<Vec<Point>> {
        let Some((file, expected)) = &mut self.source else {
            return profile_curves::sample(&self.recording, key, scene, frames, cancel);
        };
        let mut report = progress("sampling complete profile curve", cancel);
        let sample = profile_curves::sample_streaming(
            file,
            &self.recording,
            key,
            scene,
            frames,
            cancel,
            |status| report(status.bytes_read, status.total_bytes),
        )?;
        validate_identity(expected, &sample.identity)?;
        Ok(sample.points)
    }
}

fn validate_identity(expected: &SourceIdentity, actual: &SourceIdentity) -> Result<()> {
    if expected != actual {
        return Err(
            "profile source changed between indexing and sampling; reopen the capture".into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../output/cli-profile-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn history(&self) -> HistoryConfig {
            HistoryConfig {
                directory: self.0.join("history"),
                ..Default::default()
            }
        }

        fn assert_history_empty(&self) {
            let path = self.0.join("history");
            assert!(!path.exists() || std::fs::read_dir(path).unwrap().next().is_none());
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            self.assert_history_empty();
            let source = self.0.join("source.pvdstream");
            if source.exists() {
                std::fs::remove_file(source).unwrap();
            }
            let history = self.0.join("history");
            if history.exists() {
                std::fs::remove_dir(history).unwrap();
            }
            std::fs::remove_dir(&self.0).unwrap();
        }
    }

    fn capture(label: &str) -> Vec<u8> {
        let string = |id: u32, value: &str| {
            let mut bytes = vec![1];
            bytes.extend((value.len() as u32 + 1).to_le_bytes());
            bytes.extend(value.as_bytes());
            bytes.push(0);
            bytes.extend(id.to_le_bytes());
            bytes
        };
        let mut class = vec![2];
        class.extend(1u32.to_le_bytes());
        class.extend(2u32.to_le_bytes());
        let mut instance = vec![6];
        instance.extend(1u32.to_le_bytes());
        instance.extend(2u32.to_le_bytes());
        instance.extend(9u64.to_le_bytes());
        let mut frame = vec![19];
        frame.extend(9u64.to_le_bytes());
        frame.extend(3u32.to_le_bytes());
        frame.extend(100u64.to_le_bytes());
        let events = [
            string(1, "physx3"),
            string(2, "PxScene"),
            string(3, "frame"),
            string(4, label),
            class,
            instance,
            frame,
        ];
        let payload: Vec<_> = events.iter().flatten().copied().collect();
        let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend((payload.len() as u32).to_le_bytes());
        bytes.extend((events.len() as u32).to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(payload);
        bytes
    }

    #[test]
    fn changed_same_size_source_is_rejected_before_catalog_or_points_return() {
        let scratch = Scratch::new();
        let path = scratch.0.join("source.pvdstream");
        let original = capture("source A");
        let changed = capture("source B");
        assert_eq!(original.len(), changed.len());
        std::fs::write(&path, &original).unwrap();
        let cancel = AtomicBool::new(false);
        let mut source = load_sdk(File::open(&path).unwrap(), scratch.history(), &cancel).unwrap();
        assert!(source.catalog(&cancel).is_ok());
        let key = ExactKey {
            zone: 1,
            event: 1,
            thread: 1,
            context: 1,
        };
        let scene = ReferenceScene {
            id: 9,
            generation: 0,
        };
        assert!(source.sample(key, scene, &[0], &cancel).is_ok());
        std::fs::write(&path, changed).unwrap();
        assert!(
            source
                .catalog(&cancel)
                .err()
                .unwrap()
                .to_string()
                .contains("changed")
        );
        assert!(
            source
                .sample(key, scene, &[0], &cancel)
                .err()
                .unwrap()
                .to_string()
                .contains("changed")
        );
        drop(source);
        scratch.assert_history_empty();
    }

    #[test]
    fn cancelled_or_truncated_load_releases_its_history() {
        let scratch = Scratch::new();
        let path = scratch.0.join("source.pvdstream");
        let bytes = capture("source A");
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        for cancelled in [true, false] {
            let cancel = AtomicBool::new(cancelled);
            assert!(load_sdk(File::open(&path).unwrap(), scratch.history(), &cancel).is_err());
            scratch.assert_history_empty();
        }
    }
}
