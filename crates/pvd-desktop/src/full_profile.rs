use crate::{clip_store::Identity, i18n::Message};
use pvd_core::{
    Recording,
    analysis::Analysis,
    analysis_export::AnalysisExportProgress,
    profile_curves::{self, ExactKey, ReferenceScene, SampleReport, SourceIdentity},
};
use std::{
    fs::File,
    io::{Cursor, Read, Seek},
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

mod disk_index;
pub(crate) use disk_index::IndexSlot;

#[derive(Clone, Copy)]
pub(crate) enum TimelineProgress {
    Streaming(AnalysisExportProgress),
    Index(pvd_core::profile_disk_index::Progress),
}

#[derive(Clone)]
pub(crate) struct Source {
    input: Input,
    identity: Identity,
    index: Option<(PathBuf, Arc<IndexSlot>)>,
}

#[derive(Clone)]
enum Input {
    Memory(Arc<Vec<u8>>),
    File(PathBuf),
    #[cfg(test)]
    Gated(Arc<std::sync::Mutex<GatedReader>>),
}

trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

pub(crate) struct SampleRequest<'a> {
    pub recording: &'a Recording,
    pub key: ExactKey,
    pub scene: ReferenceScene,
    pub frames: &'a [usize],
}

pub(crate) struct StageRequest<'a> {
    pub recording: &'a Recording,
    pub scene: ReferenceScene,
    pub metric: pvd_core::frame_metrics::Metric,
    pub frames: &'a [usize],
}

impl Source {
    #[cfg(test)]
    pub(crate) fn gated_test_capture() -> (
        Self,
        Arc<Recording>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let bytes = tests::capture("gated source");
        Self::gated_test_bytes(bytes)
    }

    #[cfg(test)]
    pub(crate) fn gated_test_bytes(
        bytes: Vec<u8>,
    ) -> (
        Self,
        Arc<Recording>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let recording = Arc::new(Recording::from_bytes(&bytes).unwrap());
        let identity = Identity::from_digest(
            blake3::hash(&bytes),
            bytes.len() as u64,
            recording.frame_count(),
        );
        let (started, start) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let reader = GatedReader {
            bytes: Cursor::new(bytes),
            started: Some(started),
            release: released,
        };
        (
            Self {
                input: Input::Gated(Arc::new(std::sync::Mutex::new(reader))),
                identity,
                index: None,
            },
            recording,
            start,
            release,
        )
    }

    pub fn memory(bytes: Arc<Vec<u8>>, identity: Identity) -> Self {
        Self {
            input: Input::Memory(bytes),
            identity,
            index: None,
        }
    }

    pub fn file(path: PathBuf, identity: Identity) -> Self {
        Self {
            input: Input::File(path),
            identity,
            index: None,
        }
    }

    pub fn with_cpu_index(mut self, path: PathBuf, index: Arc<IndexSlot>) -> Self {
        self.index = Some((path, index));
        self
    }

    pub fn can_rebuild_cpu_index(&self) -> bool {
        self.index.is_some()
    }

    pub fn rebuild_cpu_index(&self) {
        if let Some((_, index)) = &self.index {
            index.rebuild();
        }
    }

    #[cfg(test)]
    pub fn catalog(
        &self,
        cancel: &AtomicBool,
        progress: impl FnMut(AnalysisExportProgress) -> bool,
    ) -> Result<(Vec<profile_curves::CatalogEntry>, Analysis), Message> {
        let report =
            self.read(|reader| profile_curves::catalog_streaming(reader, cancel, progress))?;
        self.validate(&report.identity)?;
        let mut analysis = Analysis::default();
        analysis.zones = report.zones;
        Ok((report.entries, analysis))
    }

    pub fn catalog_page(
        &self,
        cursor: Option<&profile_curves::CatalogCursor>,
        limit: usize,
        cancel: &AtomicBool,
        progress: impl FnMut(AnalysisExportProgress) -> bool,
    ) -> Result<(profile_curves::CatalogPageReport, Analysis), Message> {
        let mut report = self.read(|reader| {
            profile_curves::catalog_page_streaming(reader, cursor, limit, cancel, progress)
        })?;
        self.validate(&report.identity)?;
        let mut analysis = Analysis::default();
        analysis.zones = std::mem::take(&mut report.zones);
        Ok((report, analysis))
    }

    pub fn catalog_page_first_start(
        &self,
        cursor: Option<&pvd_core::profile_catalog_index::FirstStartCursor>,
        limit: usize,
        cancel: &AtomicBool,
        mut progress: impl FnMut(TimelineProgress) -> bool,
    ) -> Result<
        (
            pvd_core::profile_catalog_index::FirstStartPageReport,
            Analysis,
        ),
        Message,
    > {
        let (path, index) = self.index.as_ref().ok_or_else(|| {
            Message::new(
                "First-start ordering requires an indexed capture file.",
                "首次开始时间排序需要可建立索引的录制文件。",
            )
        })?;
        let (digest, bytes, frames) = self.identity.fingerprint().ok_or_else(|| {
            Message::new(
                "The capture identity is unavailable; reopen the recording.",
                "录制身份不可用，请重新打开文件。",
            )
        })?;
        let identity = SourceIdentity {
            blake3: *digest.as_bytes(),
            bytes,
            frames,
        };
        let mut report = index.catalog_page(path, &identity, cursor, limit, cancel, |p| {
            progress(TimelineProgress::Index(p))
        })?;
        self.validate(&report.identity)?;
        let mut analysis = Analysis::default();
        analysis.zones = std::mem::take(&mut report.zones);
        Ok((report, analysis))
    }

    pub fn sample(
        &self,
        request: SampleRequest<'_>,
        cancel: &AtomicBool,
        progress: impl FnMut(AnalysisExportProgress) -> bool,
    ) -> Result<SampleReport, Message> {
        let report = self.read(|reader| {
            profile_curves::sample_streaming(
                reader,
                request.recording,
                request.key,
                request.scene,
                request.frames,
                cancel,
                progress,
            )
        })?;
        self.validate(&report.identity)?;
        Ok(report)
    }

    pub fn sample_stage(
        &self,
        request: StageRequest<'_>,
        cancel: &AtomicBool,
        progress: impl FnMut(AnalysisExportProgress) -> bool,
    ) -> Result<pvd_core::frame_metrics::StageSampleReport, Message> {
        let report = self.read(|reader| {
            pvd_core::frame_metrics::sample_streaming(
                reader,
                request.recording,
                request.scene,
                request.metric,
                request.frames,
                cancel,
                progress,
            )
        })?;
        self.validate(&report.identity)?;
        Ok(report)
    }

    pub fn thread_page(
        &self,
        cursor: Option<&pvd_core::profile_timeline::ThreadCursor>,
        limit: usize,
        cancel: &AtomicBool,
        progress: impl FnMut(AnalysisExportProgress) -> bool,
    ) -> Result<pvd_core::profile_timeline::ThreadPageReport, Message> {
        let report = self.read(|reader| {
            pvd_core::profile_timeline::catalog_page_streaming(
                reader, cursor, limit, cancel, progress,
            )
        })?;
        self.validate(&report.identity)?;
        Ok(report)
    }

    pub fn thread_window(
        &self,
        identity: &SourceIdentity,
        request: pvd_core::profile_timeline::WindowRequest,
        cancel: &AtomicBool,
        mut progress: impl FnMut(TimelineProgress) -> bool,
    ) -> Result<pvd_core::profile_timeline::WindowReport, Message> {
        self.validate(identity)?;
        if let Some((path, index)) = &self.index {
            let report = index.window(path, identity, request, cancel, |p| {
                progress(TimelineProgress::Index(p))
            })?;
            self.validate(&report.identity)?;
            return Ok(report);
        }
        self.thread_window_streaming(identity, request, cancel, progress)
    }

    pub fn thread_window_streaming(
        &self,
        identity: &SourceIdentity,
        request: pvd_core::profile_timeline::WindowRequest,
        cancel: &AtomicBool,
        mut progress: impl FnMut(TimelineProgress) -> bool,
    ) -> Result<pvd_core::profile_timeline::WindowReport, Message> {
        self.validate(identity)?;
        self.check_timeline_open(cancel)?;
        let file_source;
        let source = if let Some((path, _)) = &self.index {
            // A file-backed view must detect changes even when scene playback retains old bytes.
            file_source = Self::file(path.clone(), self.identity.clone());
            &file_source
        } else {
            self
        };
        let report = source.read(|reader| {
            pvd_core::profile_timeline::window_streaming(reader, identity, request, cancel, |p| {
                self.check_timeline_open(cancel).is_ok() && progress(TimelineProgress::Streaming(p))
            })
        })?;
        self.check_timeline_open(cancel)?;
        self.validate(&report.identity)?;
        Ok(report)
    }

    fn check_timeline_open(&self, cancel: &AtomicBool) -> Result<(), Message> {
        if let Some((_, index)) = &self.index {
            index.check(cancel)
        } else if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            Err(Message::new("Timeline query cancelled", "时间线查询已取消"))
        } else {
            Ok(())
        }
    }

    fn read<T>(
        &self,
        operation: impl FnOnce(&mut dyn ReadSeek) -> Result<T, pvd_core::Error>,
    ) -> Result<T, Message> {
        let result = match &self.input {
            Input::Memory(bytes) => operation(&mut Cursor::new(bytes.as_slice())),
            Input::File(path) => {
                let mut file = File::open(path).map_err(|error| {
                    Message::detail("Cannot open profile source", "无法打开性能源文件", error)
                })?;
                operation(&mut file)
            }
            #[cfg(test)]
            Input::Gated(reader) => {
                let mut reader = reader
                    .lock()
                    .map_err(|_| Message::new("test source lock failed", "测试源锁失败"))?;
                operation(&mut *reader)
            }
        };
        result.map_err(|error| {
            Message::detail(
                "Complete profile indexing failed",
                "完整性能索引失败",
                error,
            )
        })
    }

    fn validate(&self, identity: &SourceIdentity) -> Result<(), Message> {
        if !self
            .identity
            .matches(identity.blake3, identity.bytes, identity.frames)
        {
            return Err(Message::new(
                "The complete capture changed since it was opened. Reopen the source before indexing its profile events.",
                "完整录制在打开后已改变，请重新打开源文件后再索引性能事件。",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
struct GatedReader {
    bytes: Cursor<Vec<u8>>,
    started: Option<std::sync::mpsc::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
impl Read for GatedReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let length = self.bytes.read(bytes)?;
        if length > 0
            && let Some(started) = self.started.take()
        {
            let _ = started.send(());
            self.release
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(std::io::Error::other)?;
        }
        Ok(length)
    }
}

#[cfg(test)]
impl Seek for GatedReader {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.bytes.seek(position)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::i18n::Language;

    pub(crate) fn timeline_capture(span_count: usize, name: &str) -> Vec<u8> {
        let text = |value: &str| {
            let mut bytes = (value.len() as u32 + 1).to_le_bytes().to_vec();
            bytes.extend(value.as_bytes());
            bytes.push(0);
            bytes
        };
        let mut events = Vec::new();
        for (id, value) in [
            (1u32, "physx3"),
            (2, "PxScene"),
            (3, "frame"),
            (4, "PxProfileZone"),
            (5, "events"),
            (6, "PvdU8"),
        ] {
            events.push(1);
            events.extend(text(value));
            events.extend(id.to_le_bytes());
        }
        for (class, id) in [(2u32, 9u64), (4, 10)] {
            events.push(6);
            events.extend(1u32.to_le_bytes());
            events.extend(class.to_le_bytes());
            events.extend(id.to_le_bytes());
        }
        events.push(24);
        events.extend(10u64.to_le_bytes());
        events.extend(text("Fixture"));
        events.push(25);
        events.extend(10u64.to_le_bytes());
        events.extend(text(name));
        events.extend(1u16.to_le_bytes());
        events.push(1);
        events.push(19);
        events.extend(9u64.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend(100u64.to_le_bytes());
        let mut payload = Vec::new();
        for index in 0..span_count {
            for (kind, tick) in [
                (1u8, 110u64 + index as u64 * 20),
                (2, 120 + index as u64 * 20),
            ] {
                payload.extend([kind, 3]);
                payload.extend(1u16.to_le_bytes());
                payload.extend((7 + index as u32 % 2).to_le_bytes());
                payload.extend([9 + index as u8 % 2, 0, 0]);
                payload.extend(tick.to_le_bytes());
            }
        }
        events.push(7);
        events.extend(10u64.to_le_bytes());
        for value in [5, 1, 6, payload.len() as u32, payload.len() as u32] {
            events.extend(value.to_le_bytes());
        }
        events.extend(payload);
        events.push(19);
        events.extend(9u64.to_le_bytes());
        events.extend(3u32.to_le_bytes());
        events.extend((200u64 + span_count as u64 * 20).to_le_bytes());
        let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1_000_000u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend((events.len() as u32).to_le_bytes());
        bytes.extend(13u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(events);
        bytes
    }

    pub(super) fn capture(label: &str) -> Vec<u8> {
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
    fn same_length_and_frame_count_cannot_bypass_source_identity() {
        let expected = b"source A";
        let source = Source::memory(
            Arc::new(b"source B".to_vec()),
            Identity::from_digest(blake3::hash(expected), expected.len() as u64, 7),
        );
        assert!(
            source
                .validate(&SourceIdentity {
                    blake3: *blake3::hash(expected).as_bytes(),
                    bytes: expected.len() as u64,
                    frames: 7
                })
                .is_ok()
        );
        let changed = SourceIdentity {
            blake3: *blake3::hash(b"source B").as_bytes(),
            bytes: expected.len() as u64,
            frames: 7,
        };
        assert!(
            source
                .validate(&changed)
                .unwrap_err()
                .text(Language::English)
                .contains("changed")
        );
        assert!(
            source
                .validate(&SourceIdentity {
                    frames: 8,
                    blake3: *blake3::hash(expected).as_bytes(),
                    ..changed
                })
                .is_err()
        );
    }

    #[test]
    fn changed_same_size_file_and_memory_never_return_catalog_or_points() {
        let original = capture("source A");
        let changed = capture("source B");
        assert_eq!(original.len(), changed.len());
        let recording = Recording::from_bytes(&original).unwrap();
        assert_eq!(
            recording.frame_count(),
            Recording::from_bytes(&changed).unwrap().frame_count()
        );
        let identity = Identity::from_digest(
            blake3::hash(&original),
            original.len() as u64,
            recording.frame_count(),
        );
        let cancelled = AtomicBool::new(false);
        let key = ExactKey {
            zone: 10,
            event: 1,
            thread: 7,
            context: 9,
        };
        let scene = ReferenceScene {
            id: 9,
            generation: 0,
        };
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/full-profile-source-tests")
            .join(format!(
                "{}-{}.pvdstream",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &original).unwrap();
        let source = Source::file(path.clone(), identity.clone());
        assert!(source.catalog(&cancelled, |_| true).is_ok());
        assert!(source.catalog_page(None, 1, &cancelled, |_| true).is_ok());
        assert!(
            source
                .sample_stage(
                    StageRequest {
                        recording: &recording,
                        scene,
                        metric: pvd_core::frame_metrics::Metric::Solver,
                        frames: &[0]
                    },
                    &cancelled,
                    |_| true
                )
                .is_ok()
        );
        assert!(
            source
                .sample(
                    SampleRequest {
                        recording: &recording,
                        key,
                        scene,
                        frames: &[0]
                    },
                    &cancelled,
                    |_| true
                )
                .is_ok()
        );
        std::fs::write(&path, &changed).unwrap();
        for source in [source, Source::memory(Arc::new(changed), identity)] {
            assert!(
                source
                    .catalog_page(None, 1, &cancelled, |_| true)
                    .unwrap_err()
                    .text(Language::English)
                    .contains("changed")
            );
            assert!(
                source
                    .sample_stage(
                        StageRequest {
                            recording: &recording,
                            scene,
                            metric: pvd_core::frame_metrics::Metric::Solver,
                            frames: &[0]
                        },
                        &cancelled,
                        |_| true
                    )
                    .unwrap_err()
                    .text(Language::English)
                    .contains("changed")
            );
            assert!(
                source
                    .catalog(&cancelled, |_| true)
                    .unwrap_err()
                    .text(Language::English)
                    .contains("changed")
            );
            assert!(
                source
                    .sample(
                        SampleRequest {
                            recording: &recording,
                            key,
                            scene,
                            frames: &[0]
                        },
                        &cancelled,
                        |_| true
                    )
                    .unwrap_err()
                    .text(Language::English)
                    .contains("changed")
            );
        }
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn full_file_curve_close_and_shutdown_wait_for_the_actual_worker() {
        use crate::{curve, studio::Studio};
        for (shutdown, fixed_stage) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let (source, recording, started, release) = Source::gated_test_capture();
            let mut studio = Studio::default();
            let jobs = studio.background_jobs.clone();
            let spec = curve::Spec {
                source: if fixed_stage {
                    curve::Source::Performance(pvd_core::frame_metrics::Metric::Solver)
                } else {
                    curve::Source::ProfileEvent(ExactKey {
                        zone: 10,
                        event: 1,
                        thread: 7,
                        context: 9,
                    })
                },
                object: 9,
                generation: 0,
                property: "event".into(),
                component: 0,
                component_name: "ms".into(),
            };
            let job = curve::Job::start(
                1,
                spec,
                recording.clone(),
                Some(source),
                bevy_egui::egui::Context::default(),
                &jobs,
            )
            .unwrap();
            started
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            studio.recording = Some(recording);
            studio.curve_job = Some(job);
            assert_eq!(jobs.active_count(), 1);
            if shutdown {
                assert!(!studio.poll_shutdown());
            } else {
                studio.close();
            }
            assert!(studio.curve_job.is_none());
            assert!(studio.recording.is_none());
            assert_eq!(jobs.active_count(), 1);
            release.send(()).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while jobs.active_count() != 0 {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(studio.curves.curves.is_empty());
            if shutdown {
                assert!(studio.poll_shutdown());
            }
        }
    }
}
