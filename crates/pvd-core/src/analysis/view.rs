use super::resident::ResidentMemoryCursor;
use super::shared::AnalysisBackend;
use super::{
    Allocation, MemoryAnalysis, MemoryChurn, MemoryCursor, MemoryEventKind, MemorySample,
    SharedAnalysis,
};
use crate::Error;
use std::ops::Range;

mod profile;
pub use profile::{
    OpenProfileEvent, ProfileKey, ProfileMetadataSource, ProfileNames, ProfileSource,
    ProfileSummary,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocationView<'a> {
    pub address: u64,
    pub size: u64,
    pub type_name: &'a str,
    pub file: &'a str,
    pub line: u32,
    pub received_frame: usize,
}

impl<'a> From<&'a Allocation> for AllocationView<'a> {
    fn from(value: &'a Allocation) -> Self {
        Self {
            address: value.address,
            size: value.size,
            type_name: &value.type_name,
            file: &value.file,
            line: value.line,
            received_frame: value.received_frame,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryAction {
    Allocate,
    Replace,
    Free,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryEventView<'a> {
    pub buffer: u64,
    pub address: u64,
    pub received_frame: usize,
    pub action: MemoryAction,
    pub allocation: Option<AllocationView<'a>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemorySummary {
    pub allocations: u64,
    pub frees: u64,
    pub peak_bytes: u64,
    pub unmatched_frees: u64,
    pub replaced_addresses: u64,
    pub live_bytes: u64,
    pub live_allocations: usize,
    pub total_allocated_bytes: u64,
}

impl From<&MemoryAnalysis> for MemorySummary {
    fn from(memory: &MemoryAnalysis) -> Self {
        Self {
            allocations: memory.allocations,
            frees: memory.frees,
            peak_bytes: memory.peak_bytes,
            unmatched_frees: memory.unmatched_frees,
            replaced_addresses: memory.replaced_addresses,
            live_bytes: memory.live_bytes,
            live_allocations: memory.live.len(),
            total_allocated_bytes: memory.total_allocated_bytes,
        }
    }
}

pub trait MemoryRows: Clone + Default {
    fn live_count(&self) -> usize;
    fn live_bytes(&self) -> u64;
    fn allocation_at(&self, index: usize) -> Option<AllocationView<'_>>;
}

pub trait MemorySource {
    type Cursor: MemoryRows;

    fn revision(&self) -> u64;
    fn first_retained_frame(&self) -> usize;
    fn summary(&self) -> MemorySummary;
    fn seek(&self, cursor: &mut Self::Cursor, frame: usize) -> Result<(), Error>;
    fn event_count(&self) -> usize;
    fn event_at(&self, index: usize) -> Option<MemoryEventView<'_>>;
    fn frame_events(&self, frame: usize) -> Range<usize>;
    fn churn(&self, frame: usize) -> MemoryChurn;
    fn sample_count(&self) -> usize;
    fn sample_at(&self, index: usize) -> Option<MemorySample>;

    fn last_event_frame(&self) -> Option<usize> {
        self.event_count()
            .checked_sub(1)
            .and_then(|index| self.event_at(index))
            .map(|event| event.received_frame)
    }
}

#[derive(Clone, Default)]
pub struct MemoryViewCursor {
    data: MemoryCursorBackend,
}

#[derive(Clone, Default)]
enum MemoryCursorBackend {
    #[default]
    Unbound,
    Legacy(LegacyViewCursor),
    Resident(ResidentMemoryCursor),
}

impl serde::Serialize for MemoryViewCursor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.data {
            MemoryCursorBackend::Unbound => {
                serde::Serialize::serialize(&super::MemoryState::default(), serializer)
            }
            MemoryCursorBackend::Legacy(cursor) => {
                serde::Serialize::serialize(cursor.legacy.state(), serializer)
            }
            MemoryCursorBackend::Resident(cursor) => {
                serde::Serialize::serialize(cursor, serializer)
            }
        }
    }
}

impl MemoryRows for MemoryViewCursor {
    fn live_count(&self) -> usize {
        match &self.data {
            MemoryCursorBackend::Unbound => 0,
            MemoryCursorBackend::Legacy(cursor) => cursor.live_count(),
            MemoryCursorBackend::Resident(cursor) => cursor.live_count(),
        }
    }

    fn live_bytes(&self) -> u64 {
        match &self.data {
            MemoryCursorBackend::Unbound => 0,
            MemoryCursorBackend::Legacy(cursor) => cursor.live_bytes(),
            MemoryCursorBackend::Resident(cursor) => cursor.live_bytes(),
        }
    }

    fn allocation_at(&self, index: usize) -> Option<AllocationView<'_>> {
        match &self.data {
            MemoryCursorBackend::Unbound => None,
            MemoryCursorBackend::Legacy(cursor) => cursor.allocation_at(index),
            MemoryCursorBackend::Resident(cursor) => cursor.allocation_at(index),
        }
    }
}

pub struct MemorySnapshot<'a> {
    data: MemorySnapshotBackend<'a>,
}

enum MemorySnapshotBackend<'a> {
    Legacy(LegacyMemorySnapshot<'a>),
    Resident(&'a super::resident::ResidentAnalysis),
}

impl SharedAnalysis {
    pub fn memory_snapshot(&self) -> MemorySnapshot<'_> {
        let data = match &self.data {
            AnalysisBackend::Legacy(analysis) => {
                MemorySnapshotBackend::Legacy(LegacyMemorySnapshot {
                    analysis,
                    revision: self.revision(),
                })
            }
            AnalysisBackend::Resident(analysis) => MemorySnapshotBackend::Resident(analysis),
        };
        MemorySnapshot { data }
    }
}

impl MemorySource for MemorySnapshot<'_> {
    type Cursor = MemoryViewCursor;

    fn revision(&self) -> u64 {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.revision(),
            MemorySnapshotBackend::Resident(analysis) => analysis.revision(),
        }
    }

    fn seek(&self, cursor: &mut Self::Cursor, frame: usize) -> Result<(), Error> {
        match (&self.data, &mut cursor.data) {
            (MemorySnapshotBackend::Legacy(analysis), MemoryCursorBackend::Legacy(current)) => {
                analysis.seek(current, frame)?;
            }
            (MemorySnapshotBackend::Resident(analysis), MemoryCursorBackend::Resident(current)) => {
                analysis.seek(current, frame)?;
            }
            (MemorySnapshotBackend::Legacy(analysis), _) => {
                let mut draft = LegacyViewCursor::default();
                analysis.seek(&mut draft, frame)?;
                cursor.data = MemoryCursorBackend::Legacy(draft);
            }
            (MemorySnapshotBackend::Resident(analysis), _) => {
                let mut draft = ResidentMemoryCursor::default();
                analysis.seek(&mut draft, frame)?;
                cursor.data = MemoryCursorBackend::Resident(draft);
            }
        }
        Ok(())
    }

    fn first_retained_frame(&self) -> usize {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.first_retained_frame(),
            MemorySnapshotBackend::Resident(analysis) => analysis.first_retained_frame(),
        }
    }

    fn summary(&self) -> MemorySummary {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.summary(),
            MemorySnapshotBackend::Resident(analysis) => analysis.summary(),
        }
    }

    fn event_count(&self) -> usize {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.event_count(),
            MemorySnapshotBackend::Resident(analysis) => analysis.event_count(),
        }
    }

    fn event_at(&self, index: usize) -> Option<MemoryEventView<'_>> {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.event_at(index),
            MemorySnapshotBackend::Resident(analysis) => analysis.event_at(index),
        }
    }

    fn frame_events(&self, frame: usize) -> Range<usize> {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.frame_events(frame),
            MemorySnapshotBackend::Resident(analysis) => analysis.frame_events(frame),
        }
    }

    fn churn(&self, frame: usize) -> MemoryChurn {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.churn(frame),
            MemorySnapshotBackend::Resident(analysis) => analysis.churn(frame),
        }
    }

    fn sample_count(&self) -> usize {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.sample_count(),
            MemorySnapshotBackend::Resident(analysis) => analysis.sample_count(),
        }
    }

    fn sample_at(&self, index: usize) -> Option<MemorySample> {
        match &self.data {
            MemorySnapshotBackend::Legacy(analysis) => analysis.sample_at(index),
            MemorySnapshotBackend::Resident(analysis) => analysis.sample_at(index),
        }
    }
}

#[derive(Clone, Default)]
struct LegacyViewCursor {
    legacy: MemoryCursor,
}

impl MemoryRows for LegacyViewCursor {
    fn live_count(&self) -> usize {
        self.legacy.state().live.len()
    }

    fn live_bytes(&self) -> u64 {
        self.legacy.state().live_bytes
    }

    fn allocation_at(&self, index: usize) -> Option<AllocationView<'_>> {
        self.legacy
            .state()
            .live
            .get_index(index)
            .map(|(_, value)| value.as_ref().into())
    }
}

struct LegacyMemorySnapshot<'a> {
    analysis: &'a super::Analysis,
    revision: u64,
}

impl MemorySource for LegacyMemorySnapshot<'_> {
    type Cursor = LegacyViewCursor;

    fn revision(&self) -> u64 {
        self.revision
    }

    fn first_retained_frame(&self) -> usize {
        self.analysis.first_retained_frame
    }

    fn summary(&self) -> MemorySummary {
        self.analysis.memory.as_ref().into()
    }

    fn seek(&self, cursor: &mut Self::Cursor, frame: usize) -> Result<(), Error> {
        self.analysis.memory.seek(&mut cursor.legacy, frame);
        Ok(())
    }

    fn event_count(&self) -> usize {
        self.analysis.memory.events().len()
    }

    fn event_at(&self, index: usize) -> Option<MemoryEventView<'_>> {
        let event = self.analysis.memory.events().get(index)?;
        let (action, allocation) = match &event.kind {
            MemoryEventKind::Allocate {
                allocation,
                replaced,
            } => (
                if replaced.is_some() {
                    MemoryAction::Replace
                } else {
                    MemoryAction::Allocate
                },
                Some(allocation.as_ref().into()),
            ),
            MemoryEventKind::Free { allocation } => {
                (MemoryAction::Free, allocation.as_deref().map(Into::into))
            }
        };
        Some(MemoryEventView {
            buffer: event.buffer,
            address: event.address,
            received_frame: event.received_frame,
            action,
            allocation,
        })
    }

    fn frame_events(&self, frame: usize) -> Range<usize> {
        let events = self.analysis.memory.events();
        events.partition_point(|event| event.received_frame < frame)
            ..events.partition_point(|event| event.received_frame <= frame)
    }

    fn churn(&self, frame: usize) -> MemoryChurn {
        self.analysis.memory.churn_at_frame(frame)
    }

    fn sample_count(&self) -> usize {
        self.analysis.memory.samples.len()
    }

    fn sample_at(&self, index: usize) -> Option<MemorySample> {
        self.analysis.memory.samples.get(index).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{Analysis, MemoryEvent};
    use std::sync::Arc;

    #[test]
    fn borrowed_legacy_rows_follow_replacement_free_and_reverse_seek() {
        let allocation = |size| {
            Arc::new(Allocation {
                address: 7,
                size,
                type_name: "body".into(),
                file: "test.cpp".into(),
                line: 8,
                received_frame: 1,
            })
        };
        let first = allocation(16);
        let second = allocation(32);
        let mut analysis = Analysis::default();
        Arc::make_mut(&mut analysis.memory).events = vec![
            Arc::new(MemoryEvent {
                buffer: 3,
                address: 7,
                received_frame: 1,
                kind: MemoryEventKind::Allocate {
                    allocation: first.clone(),
                    replaced: None,
                },
            }),
            Arc::new(MemoryEvent {
                buffer: 3,
                address: 7,
                received_frame: 2,
                kind: MemoryEventKind::Allocate {
                    allocation: second.clone(),
                    replaced: Some(first),
                },
            }),
            Arc::new(MemoryEvent {
                buffer: 3,
                address: 7,
                received_frame: 3,
                kind: MemoryEventKind::Free {
                    allocation: Some(second),
                },
            }),
            Arc::new(MemoryEvent {
                buffer: 3,
                address: 19,
                received_frame: 4,
                kind: MemoryEventKind::Free { allocation: None },
            }),
        ];
        let shared: SharedAnalysis = analysis.into();
        let snapshot = shared.memory_snapshot();
        let mut cursor = MemoryViewCursor::default();
        for (frame, size) in [(1, 16), (2, 32), (3, 0), (4, 0), (2, 32), (1, 16), (0, 0)] {
            snapshot.seek(&mut cursor, frame).unwrap();
            assert_eq!(cursor.live_bytes(), size);
            assert_eq!(cursor.live_count(), usize::from(size != 0));
            assert_eq!(
                cursor.allocation_at(0).map(|value| value.size),
                (size != 0).then_some(size)
            );
        }
        assert_eq!(snapshot.event_at(1).unwrap().action, MemoryAction::Replace);
        assert!(snapshot.event_at(3).unwrap().allocation.is_none());
        assert_eq!(snapshot.frame_events(2), 1..2);
        assert_eq!(snapshot.frame_events(0), 0..0);
        assert_eq!(snapshot.last_event_frame(), Some(4));
        assert_eq!(snapshot.churn(2).live_byte_change(), 16);
        assert_eq!(snapshot.churn(4).unmatched_frees, 1);
    }
}
