use super::{Source as SdkSource, TimelineProgress as SdkProgress};
use crate::i18n::Message;
use pvd_core::{
    Recording,
    analysis_export::AnalysisExportProgress,
    profile_curves::SourceIdentity,
    profile_timeline::{self, WindowRequest, native},
};
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Identity {
    Sdk(SourceIdentity),
    Native(native::Identity),
}

pub(crate) type ThreadCursor = profile_timeline::ThreadCursor<Identity>;
pub(crate) type ThreadPageReport = profile_timeline::ThreadPageReport<Identity>;
pub(crate) type WindowReport = profile_timeline::WindowReport<Identity>;

#[derive(Clone, Copy)]
pub(crate) enum TimelineProgress {
    Streaming(AnalysisExportProgress),
    Index(pvd_core::profile_disk_index::Progress),
    Native(native::Progress),
}

impl From<SdkProgress> for TimelineProgress {
    fn from(progress: SdkProgress) -> Self {
        match progress {
            SdkProgress::Streaming(progress) => Self::Streaming(progress),
            SdkProgress::Index(progress) => Self::Index(progress),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Source {
    Sdk(SdkSource),
    Native(Arc<Recording>),
}

impl From<SdkSource> for Source {
    fn from(source: SdkSource) -> Self {
        Self::Sdk(source)
    }
}

fn wrong_source() -> Message {
    Message::new(
        "The timeline belongs to another recording; reload its thread catalog.",
        "时间线属于另一份录制，请重新载入线程目录。",
    )
}

fn native_error(error: pvd_core::Error) -> Message {
    Message::detail(
        "Native timeline query failed",
        "原版录制时间线查询失败",
        error,
    )
}

impl Source {
    pub fn native(recording: Arc<Recording>) -> Option<Self> {
        (recording.source == pvd_core::RecordingSource::Pvd3Database && recording.is_disk_backed())
            .then_some(Self::Native(recording))
    }

    pub fn can_rebuild_cpu_index(&self) -> bool {
        matches!(self, Self::Sdk(source) if source.can_rebuild_cpu_index())
    }

    pub fn rebuild_cpu_index(&self) {
        if let Self::Sdk(source) = self {
            source.rebuild_cpu_index();
        }
    }

    pub fn thread_page(
        &self,
        cursor: Option<&ThreadCursor>,
        limit: usize,
        cancel: &AtomicBool,
        mut progress: impl FnMut(TimelineProgress) -> bool,
    ) -> Result<ThreadPageReport, Message> {
        match self {
            Self::Sdk(source) => {
                let cursor = cursor
                    .map(|cursor| {
                        let Identity::Sdk(identity) = &cursor.identity else {
                            return Err(wrong_source());
                        };
                        Ok(profile_timeline::ThreadCursor {
                            identity: identity.clone(),
                            after: cursor.after,
                        })
                    })
                    .transpose()?;
                source
                    .thread_page(cursor.as_ref(), limit, cancel, |status| {
                        progress(TimelineProgress::Streaming(status))
                    })
                    .map(|report| report.map_identity(Identity::Sdk))
            }
            Self::Native(recording) => {
                let cursor = cursor
                    .map(|cursor| {
                        let Identity::Native(identity) = &cursor.identity else {
                            return Err(wrong_source());
                        };
                        Ok(profile_timeline::ThreadCursor {
                            identity: identity.clone(),
                            after: cursor.after,
                        })
                    })
                    .transpose()?;
                native::catalog_page(recording, cursor.as_ref(), limit, cancel, |status| {
                    progress(TimelineProgress::Native(status))
                })
                .map(|report| report.map_identity(Identity::Native))
                .map_err(native_error)
            }
        }
    }

    pub fn thread_window(
        &self,
        identity: &Identity,
        request: WindowRequest,
        cancel: &AtomicBool,
        mut progress: impl FnMut(TimelineProgress) -> bool,
    ) -> Result<WindowReport, Message> {
        match (self, identity) {
            (Self::Sdk(source), Identity::Sdk(identity)) => source
                .thread_window(identity, request, cancel, |status| progress(status.into()))
                .map(|report| report.map_identity(Identity::Sdk)),
            (Self::Native(recording), Identity::Native(identity)) => {
                native::window(recording, identity, request, cancel, |status| {
                    progress(TimelineProgress::Native(status))
                })
                .map(|report| report.map_identity(Identity::Native))
                .map_err(native_error)
            }
            _ => Err(wrong_source()),
        }
    }

    pub fn thread_window_streaming(
        &self,
        identity: &Identity,
        request: WindowRequest,
        cancel: &AtomicBool,
        mut progress: impl FnMut(TimelineProgress) -> bool,
    ) -> Result<WindowReport, Message> {
        if let (Self::Sdk(source), Identity::Sdk(identity)) = (self, identity) {
            source
                .thread_window_streaming(identity, request, cancel, |status| {
                    progress(status.into())
                })
                .map(|report| report.map_identity(Identity::Sdk))
        } else {
            self.thread_window(identity, request, cancel, progress)
        }
    }

    #[cfg(test)]
    pub fn memory(bytes: Arc<Vec<u8>>, identity: crate::clip_store::Identity) -> Self {
        SdkSource::memory(bytes, identity).into()
    }

    #[cfg(test)]
    pub fn file(path: std::path::PathBuf, identity: crate::clip_store::Identity) -> Self {
        SdkSource::file(path, identity).into()
    }

    #[cfg(test)]
    pub fn gated_test_bytes(
        bytes: Vec<u8>,
    ) -> (
        Self,
        Arc<Recording>,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (source, recording, started, release) = SdkSource::gated_test_bytes(bytes);
        (source.into(), recording, started, release)
    }
}
