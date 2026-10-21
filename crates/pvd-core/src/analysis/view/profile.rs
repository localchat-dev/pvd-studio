use crate::analysis::shared::AnalysisBackend;
use crate::analysis::{Analysis, ProfileSpan, ProfileValue, SharedAnalysis};

pub type ProfileKey = (u64, u16, u32, u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProfileSummary {
    pub first_retained_frame: usize,
    pub complete_spans: usize,
    pub open_spans: usize,
    pub unmatched_stops: u64,
    pub legacy_cuda_buffers: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileNames<'a> {
    pub zone: u64,
    pub event: u16,
    pub zone_name: &'a str,
    pub event_name: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenProfileEvent {
    pub key: ProfileKey,
    pub count: usize,
    pub earliest: u64,
}

pub trait ProfileSource {
    fn profile_summary(&self) -> ProfileSummary;
    fn profile_span(&self, index: usize) -> Option<&ProfileSpan>;
    fn profile_spans(&self) -> impl DoubleEndedIterator<Item = &ProfileSpan> + ExactSizeIterator;
    fn profile_values(&self) -> impl DoubleEndedIterator<Item = &ProfileValue> + ExactSizeIterator;
    fn profile_zone_name(&self, zone: u64) -> Option<&str>;
    fn profile_event_name(&self, zone: u64, event: u16) -> Option<&str>;
    fn profile_names(&self) -> impl Iterator<Item = ProfileNames<'_>>;
    fn profile_metadata_ambiguous(&self, zone: u64, event: u16) -> bool;
    fn profile_open_counts(&self) -> impl Iterator<Item = (ProfileKey, usize)>;
    fn open_profile_events(&self) -> impl Iterator<Item = OpenProfileEvent>;
    fn profile_open_event(&self, key: ProfileKey) -> Option<OpenProfileEvent>;
    fn unmatched_profile_events(&self) -> impl Iterator<Item = (ProfileKey, u64)>;
    fn unmatched_profile_event(&self, key: ProfileKey) -> u64;
}

/// Metadata iterators preserve numeric key order; zones include entries without events.
pub trait ProfileMetadataSource: ProfileSource {
    fn profile_zones(&self) -> impl Iterator<Item = (u64, &str)>;
    fn profile_ambiguous_zones(&self) -> impl Iterator<Item = u64>;
    fn profile_ambiguous_events(&self) -> impl Iterator<Item = (u64, u16)>;
    fn profile_metadata_budget(&self) -> usize;
}

impl ProfileMetadataSource for Analysis {
    fn profile_zones(&self) -> impl Iterator<Item = (u64, &str)> {
        self.zones
            .iter()
            .map(|(&id, zone)| (id, zone.name.as_str()))
    }

    fn profile_ambiguous_zones(&self) -> impl Iterator<Item = u64> {
        self.ambiguous_zones.iter().copied()
    }

    fn profile_ambiguous_events(&self) -> impl Iterator<Item = (u64, u16)> {
        self.ambiguous_events.iter().copied()
    }

    fn profile_metadata_budget(&self) -> usize {
        self.export_metadata_budget()
    }
}

impl ProfileMetadataSource for SharedAnalysis {
    fn profile_zones(&self) -> impl Iterator<Item = (u64, &str)> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => ProfileIter::Legacy(analysis.profile_zones()),
            AnalysisBackend::Resident(analysis) => ProfileIter::Resident(analysis.profile_zones()),
        }
    }

    fn profile_ambiguous_zones(&self) -> impl Iterator<Item = u64> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => {
                ProfileIter::Legacy(analysis.profile_ambiguous_zones())
            }
            AnalysisBackend::Resident(analysis) => {
                ProfileIter::Resident(analysis.profile_ambiguous_zones())
            }
        }
    }

    fn profile_ambiguous_events(&self) -> impl Iterator<Item = (u64, u16)> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => {
                ProfileIter::Legacy(analysis.profile_ambiguous_events())
            }
            AnalysisBackend::Resident(analysis) => {
                ProfileIter::Resident(analysis.profile_ambiguous_events())
            }
        }
    }

    fn profile_metadata_budget(&self) -> usize {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_metadata_budget(),
            AnalysisBackend::Resident(analysis) => analysis.profile_metadata_budget(),
        }
    }
}

impl ProfileSource for Analysis {
    fn profile_summary(&self) -> ProfileSummary {
        ProfileSummary {
            first_retained_frame: self.first_retained_frame,
            complete_spans: self.spans.len(),
            open_spans: self.open_spans(),
            unmatched_stops: self.unmatched_stops,
            legacy_cuda_buffers: self.legacy_cuda_buffers,
        }
    }

    fn profile_span(&self, index: usize) -> Option<&ProfileSpan> {
        self.spans.get(index)
    }

    fn profile_spans(&self) -> impl DoubleEndedIterator<Item = &ProfileSpan> + ExactSizeIterator {
        self.spans.iter()
    }

    fn profile_values(&self) -> impl DoubleEndedIterator<Item = &ProfileValue> + ExactSizeIterator {
        self.values.iter()
    }

    fn profile_zone_name(&self, zone: u64) -> Option<&str> {
        self.zones.get(&zone).map(|zone| zone.name.as_str())
    }

    fn profile_event_name(&self, zone: u64, event: u16) -> Option<&str> {
        self.zones
            .get(&zone)?
            .events
            .get(&event)
            .map(String::as_str)
    }

    fn profile_names(&self) -> impl Iterator<Item = ProfileNames<'_>> {
        self.zones.iter().flat_map(|(&zone, data)| {
            data.events.iter().map(move |(&event, name)| ProfileNames {
                zone,
                event,
                zone_name: &data.name,
                event_name: name,
            })
        })
    }

    fn profile_metadata_ambiguous(&self, zone: u64, event: u16) -> bool {
        Analysis::profile_metadata_ambiguous(self, zone, event)
    }

    fn profile_open_counts(&self) -> impl Iterator<Item = (ProfileKey, usize)> {
        self.open_events().map(|(&key, count)| (key, count))
    }

    fn open_profile_events(&self) -> impl Iterator<Item = OpenProfileEvent> {
        self.open_event_starts().filter_map(|(&key, starts)| {
            starts
                .iter()
                .copied()
                .min()
                .map(|earliest| OpenProfileEvent {
                    key,
                    count: starts.len(),
                    earliest,
                })
        })
    }

    fn unmatched_profile_events(&self) -> impl Iterator<Item = (ProfileKey, u64)> {
        self.unmatched_stop_events
            .iter()
            .map(|(&key, &count)| (key, count))
    }

    fn profile_open_event(&self, key: ProfileKey) -> Option<OpenProfileEvent> {
        let starts = self.starts.get(&key)?;
        Some(OpenProfileEvent {
            key,
            count: starts.len(),
            earliest: starts.iter().copied().min()?,
        })
    }

    fn unmatched_profile_event(&self, key: ProfileKey) -> u64 {
        self.unmatched_stop_events.get(&key).copied().unwrap_or(0)
    }
}

impl ProfileSource for SharedAnalysis {
    fn profile_summary(&self) -> ProfileSummary {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_summary(),
            AnalysisBackend::Resident(analysis) => analysis.profile_summary(),
        }
    }

    fn profile_span(&self, index: usize) -> Option<&ProfileSpan> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_span(index),
            AnalysisBackend::Resident(analysis) => analysis.profile_span(index),
        }
    }

    fn profile_spans(&self) -> impl DoubleEndedIterator<Item = &ProfileSpan> + ExactSizeIterator {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => ProfileIter::Legacy(analysis.profile_spans()),
            AnalysisBackend::Resident(analysis) => ProfileIter::Resident(analysis.profile_spans()),
        }
    }

    fn profile_values(&self) -> impl DoubleEndedIterator<Item = &ProfileValue> + ExactSizeIterator {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => ProfileIter::Legacy(analysis.profile_values()),
            AnalysisBackend::Resident(analysis) => ProfileIter::Resident(analysis.profile_values()),
        }
    }

    fn profile_zone_name(&self, zone: u64) -> Option<&str> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_zone_name(zone),
            AnalysisBackend::Resident(analysis) => analysis.profile_zone_name(zone),
        }
    }

    fn profile_event_name(&self, zone: u64, event: u16) -> Option<&str> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_event_name(zone, event),
            AnalysisBackend::Resident(analysis) => analysis.profile_event_name(zone, event),
        }
    }

    fn profile_names(&self) -> impl Iterator<Item = ProfileNames<'_>> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => ProfileIter::Legacy(analysis.profile_names()),
            AnalysisBackend::Resident(analysis) => ProfileIter::Resident(analysis.profile_names()),
        }
    }

    fn profile_metadata_ambiguous(&self, zone: u64, event: u16) -> bool {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_metadata_ambiguous(zone, event),
            AnalysisBackend::Resident(analysis) => analysis.profile_metadata_ambiguous(zone, event),
        }
    }

    fn profile_open_counts(&self) -> impl Iterator<Item = (ProfileKey, usize)> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => {
                ProfileIter::Legacy(analysis.profile_open_counts())
            }
            AnalysisBackend::Resident(analysis) => {
                ProfileIter::Resident(analysis.profile_open_counts())
            }
        }
    }

    fn open_profile_events(&self) -> impl Iterator<Item = OpenProfileEvent> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => {
                ProfileIter::Legacy(analysis.open_profile_events())
            }
            AnalysisBackend::Resident(analysis) => {
                ProfileIter::Resident(analysis.open_profile_events())
            }
        }
    }

    fn profile_open_event(&self, key: ProfileKey) -> Option<OpenProfileEvent> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.profile_open_event(key),
            AnalysisBackend::Resident(analysis) => analysis.profile_open_event(key),
        }
    }

    fn unmatched_profile_events(&self) -> impl Iterator<Item = (ProfileKey, u64)> {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => {
                ProfileIter::Legacy(analysis.unmatched_profile_events())
            }
            AnalysisBackend::Resident(analysis) => {
                ProfileIter::Resident(analysis.unmatched_profile_events())
            }
        }
    }

    fn unmatched_profile_event(&self, key: ProfileKey) -> u64 {
        match &self.data {
            AnalysisBackend::Legacy(analysis) => analysis.unmatched_profile_event(key),
            AnalysisBackend::Resident(analysis) => analysis.unmatched_profile_event(key),
        }
    }
}

enum ProfileIter<L, R> {
    Legacy(L),
    Resident(R),
}

impl<L, R> Iterator for ProfileIter<L, R>
where
    L: Iterator,
    R: Iterator<Item = L::Item>,
{
    type Item = L::Item;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Legacy(iter) => iter.next(),
            Self::Resident(iter) => iter.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Legacy(iter) => iter.size_hint(),
            Self::Resident(iter) => iter.size_hint(),
        }
    }
}

impl<L, R> DoubleEndedIterator for ProfileIter<L, R>
where
    L: DoubleEndedIterator,
    R: DoubleEndedIterator<Item = L::Item>,
{
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Legacy(iter) => iter.next_back(),
            Self::Resident(iter) => iter.next_back(),
        }
    }
}

impl<L, R> ExactSizeIterator for ProfileIter<L, R>
where
    L: ExactSizeIterator,
    R: ExactSizeIterator<Item = L::Item>,
{
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_open_event_uses_only_the_matching_stack_and_ignores_empty_stacks() {
        let key = (7, 1, 4, 9);
        let empty = (7, 2, 4, 9);
        let mut analysis = Analysis::default();
        analysis.starts.insert(key, vec![20, 4, 10]);
        analysis.starts.insert(empty, Vec::new());
        analysis.starts.insert((9, 1, 4, 9), vec![1]);
        let shared: SharedAnalysis = analysis.into();
        assert_eq!(
            shared.profile_open_event(key),
            Some(OpenProfileEvent {
                key,
                count: 3,
                earliest: 4,
            })
        );
        assert_eq!(
            shared.profile_open_counts().collect::<Vec<_>>(),
            [(key, 3), ((9, 1, 4, 9), 1)]
        );
        assert_eq!(shared.profile_open_event(empty), None);
        assert_eq!(shared.profile_open_event((0, 0, 0, 0)), None);
    }
}
