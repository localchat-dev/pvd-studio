use super::resident::ResidentAnalysis;
use super::view::ProfileSource;
use super::{ANALYSIS_REVISION, Analysis, Error, RetentionSchedule};
use pvd_protocol::Endian;
use std::sync::{Arc, atomic::Ordering};

#[derive(Clone)]
pub(super) enum AnalysisBackend {
    Legacy(Arc<Analysis>),
    Resident(ResidentAnalysis),
}

/// Published roots own their storage; borrowed views never materialize a second analysis.
#[derive(Clone)]
pub struct SharedAnalysis {
    pub(super) data: AnalysisBackend,
    revision: u64,
    retention: RetentionSchedule,
}

impl std::fmt::Debug for SharedAnalysis {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedAnalysis")
            .field("revision", &self.revision())
            .field("resident", &self.resident().is_some())
            .field("summary", &self.profile_summary())
            .finish()
    }
}

impl serde::Serialize for SharedAnalysis {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => serde::Serialize::serialize(analysis, serializer),
            AnalysisBackend::Resident(analysis) => {
                serde::Serialize::serialize(analysis, serializer)
            }
        }
    }
}

impl Default for SharedAnalysis {
    fn default() -> Self {
        Analysis::default().into()
    }
}

impl From<Analysis> for SharedAnalysis {
    fn from(analysis: Analysis) -> Self {
        Arc::new(analysis).into()
    }
}

impl From<Arc<Analysis>> for SharedAnalysis {
    fn from(analysis: Arc<Analysis>) -> Self {
        Self {
            data: AnalysisBackend::Legacy(analysis),
            revision: ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed),
            retention: RetentionSchedule::default(),
        }
    }
}

impl From<ResidentAnalysis> for SharedAnalysis {
    fn from(analysis: ResidentAnalysis) -> Self {
        Self {
            revision: analysis.revision(),
            data: AnalysisBackend::Resident(analysis),
            retention: RetentionSchedule::default(),
        }
    }
}

impl SharedAnalysis {
    pub fn revision(&self) -> u64 {
        match &self.data {
            AnalysisBackend::Legacy(_) => self.revision,
            AnalysisBackend::Resident(analysis) => analysis.revision(),
        }
    }

    pub fn legacy(&self) -> Option<&Analysis> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => Some(analysis),
            AnalysisBackend::Resident(_) => None,
        }
    }

    pub(crate) fn legacy_mut(&mut self) -> Result<&mut Analysis, Error> {
        match &mut self.data {
            AnalysisBackend::Legacy(analysis) => {
                self.revision = ANALYSIS_REVISION.fetch_add(1, Ordering::Relaxed);
                Ok(Arc::make_mut(analysis))
            }
            AnalysisBackend::Resident(_) => Err(crate::error("analysis uses resident storage")),
        }
    }

    pub(crate) fn resident(&self) -> Option<&ResidentAnalysis> {
        match &self.data {
            AnalysisBackend::Legacy(_) => None,
            AnalysisBackend::Resident(analysis) => Some(analysis),
        }
    }

    pub fn first_retained_frame(&self) -> usize {
        self.profile_summary().first_retained_frame
    }

    pub fn accounted_bytes(&self) -> usize {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.accounted_bytes(),
            AnalysisBackend::Resident(analysis) => analysis.accounted_bytes(),
        }
    }

    pub fn open_spans(&self) -> usize {
        self.profile_summary().open_spans
    }

    pub fn add_zone(&mut self, id: u64, name: String) -> Result<(), Error> {
        match &mut self.data {
            AnalysisBackend::Legacy(_) => self.legacy_mut()?.add_zone(id, name),
            AnalysisBackend::Resident(analysis) => analysis.add_zone(id, &name, &mut || false),
        }
    }

    pub fn add_event(&mut self, zone: u64, event: u16, name: String) -> Result<(), Error> {
        match &mut self.data {
            AnalysisBackend::Legacy(_) => self.legacy_mut()?.add_event(zone, event, name),
            AnalysisBackend::Resident(analysis) => {
                analysis.add_event(zone, event, &name, &mut || false)
            }
        }
    }

    pub fn ingest(
        &mut self,
        id: u64,
        type_name: &str,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
    ) -> Result<(), Error> {
        self.ingest_with_cancel(id, type_name, bytes, endian, frame, &mut || false)
    }

    pub(crate) fn ingest_with_cancel(
        &mut self,
        id: u64,
        type_name: &str,
        bytes: &[u8],
        endian: Endian,
        frame: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        crate::check_replay_cancelled(&mut || cancelled())?;
        match &mut self.data {
            AnalysisBackend::Legacy(_) => self
                .legacy_mut()?
                .ingest(id, type_name, bytes, endian, frame),
            AnalysisBackend::Resident(analysis) => {
                analysis.ingest(id, type_name, bytes, endian, frame, cancelled)
            }
        }
    }

    pub fn retain_recent(&mut self, first_frame: usize) -> Result<(), Error> {
        match &mut self.data {
            AnalysisBackend::Legacy(_) => self.legacy_mut()?.retain_recent(first_frame),
            AnalysisBackend::Resident(analysis) => {
                analysis.try_retain_recent(first_frame, &mut || false)?;
            }
        }
        self.retention.retained_bytes = self.accounted_bytes();
        Ok(())
    }

    pub(crate) fn reset_at(&mut self, frame: usize) -> Result<(), Error> {
        match &mut self.data {
            AnalysisBackend::Legacy(_) => {
                *self = Analysis {
                    first_retained_frame: frame,
                    ..Default::default()
                }
                .into();
            }
            AnalysisBackend::Resident(analysis) => {
                analysis.reset_at(frame, &mut || false)?;
                self.retention = RetentionSchedule::default();
            }
        }
        Ok(())
    }

    pub(crate) fn retain_captured(
        &mut self,
        first_frame: usize,
        accounted_bytes: usize,
    ) -> Result<(), Error> {
        match &mut self.data {
            AnalysisBackend::Legacy(_) => self
                .legacy_mut()?
                .retain_captured(first_frame, accounted_bytes)?,
            AnalysisBackend::Resident(analysis) => {
                analysis.try_retain_captured(first_frame, accounted_bytes, &mut || false)?;
            }
        }
        self.retention.retained_bytes = self.accounted_bytes();
        Ok(())
    }

    pub(crate) fn maintain_live_history(&mut self, frame: usize) -> Result<bool, Error> {
        let limit = self.accounted_bytes().saturating_add(match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.export_metadata_budget(),
            AnalysisBackend::Resident(analysis) => analysis.semantic_budget_remaining(),
        });
        let Some(cutoff) = self.retention.cutoff_usage(
            self.accounted_bytes(),
            limit,
            self.first_retained_frame(),
            frame,
        ) else {
            return Ok(false);
        };
        self.retain_recent(cutoff)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
