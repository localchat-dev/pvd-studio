use super::*;
use crate::analysis::view::{OpenProfileEvent, ProfileKey, ProfileNames, ProfileSummary};
use crate::analysis::{Analysis, ProfileSpan, ProfileValue};

struct SparseProfile {
    spans: BTreeMap<usize, Box<ProfileSpan>>,
    names: Analysis,
}

impl ProfileSource for SparseProfile {
    fn profile_summary(&self) -> ProfileSummary {
        ProfileSummary {
            complete_spans: self.spans.len(),
            open_spans: 2,
            unmatched_stops: 3,
            ..ProfileSummary::default()
        }
    }

    fn profile_span(&self, index: usize) -> Option<&ProfileSpan> {
        self.spans.get(&index).map(Box::as_ref)
    }

    fn profile_spans(&self) -> impl DoubleEndedIterator<Item = &ProfileSpan> + ExactSizeIterator {
        self.spans.values().map(Box::as_ref)
    }

    fn profile_values(&self) -> impl DoubleEndedIterator<Item = &ProfileValue> + ExactSizeIterator {
        std::iter::empty()
    }

    fn profile_zone_name(&self, zone: u64) -> Option<&str> {
        self.names.profile_zone_name(zone)
    }

    fn profile_event_name(&self, zone: u64, event: u16) -> Option<&str> {
        self.names.profile_event_name(zone, event)
    }

    fn profile_names(&self) -> impl Iterator<Item = ProfileNames<'_>> {
        self.names.profile_names()
    }

    fn profile_metadata_ambiguous(&self, zone: u64, event: u16) -> bool {
        (zone, event) == (7, 3)
    }

    fn profile_open_counts(&self) -> impl Iterator<Item = (ProfileKey, usize)> {
        [((7, 1, 4, 9), 2)].into_iter()
    }

    fn open_profile_events(&self) -> impl Iterator<Item = OpenProfileEvent> {
        [OpenProfileEvent {
            key: (7, 1, 4, 9),
            count: 2,
            earliest: 5,
        }]
        .into_iter()
    }

    fn unmatched_profile_events(&self) -> impl Iterator<Item = (ProfileKey, u64)> {
        [((7, 3, 4, 9), 3)].into_iter()
    }

    fn profile_open_event(&self, key: ProfileKey) -> Option<OpenProfileEvent> {
        self.open_profile_events().find(|event| event.key == key)
    }

    fn unmatched_profile_event(&self, key: ProfileKey) -> u64 {
        if key == (7, 3, 4, 9) { 3 } else { 0 }
    }
}

#[test]
fn catalog_borrows_noncontiguous_spans_and_preserves_incomplete_identity() {
    let mut names = Analysis::default();
    names.add_zone(7, "physics".into()).unwrap();
    names.add_event(7, 1, "simulate".into()).unwrap();
    let source = SparseProfile {
        names,
        spans: [(0, 2, 30), (1, 1, 10), (2, 2, 20)]
            .map(|(index, event, start)| {
                (
                    index,
                    Box::new(ProfileSpan {
                        zone: 7,
                        event,
                        thread: 4,
                        context: 9,
                        start,
                        end: start + 1,
                        received_frame: 0,
                    }),
                )
            })
            .into(),
    };
    let mut spans = source.profile_spans();
    assert_eq!(spans.len(), 3);
    assert!(std::ptr::eq(
        spans.next().unwrap(),
        source.spans[&0].as_ref()
    ));
    assert!(std::ptr::eq(
        spans.next_back().unwrap(),
        source.spans[&2].as_ref()
    ));
    assert_eq!(spans.len(), 1);
    assert_eq!(source.profile_span(1).unwrap().start, 10);
    assert!(source.profile_span(3).is_none());
    let rows = catalog_source(&source, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.key.event).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        (
            rows[0].first_start,
            rows[0].complete_spans,
            rows[0].open_spans
        ),
        (Some(5), 1, 2)
    );
    assert_eq!((rows[1].first_start, rows[1].complete_spans), (Some(20), 2));
    assert_eq!(
        (
            rows[2].first_start,
            rows[2].unmatched_stops,
            rows[2].metadata_ambiguous
        ),
        (None, 3, true)
    );
    assert!(catalog_source(&source, &AtomicBool::new(true)).is_err());
}

#[test]
fn borrowed_profile_snapshot_keeps_names_and_spans_on_its_revision() {
    let mut analysis = Analysis::default();
    analysis.add_zone(7, "physics".into()).unwrap();
    analysis.add_event(7, 1, "simulate".into()).unwrap();
    analysis.spans.push(ProfileSpan {
        zone: 7,
        event: 1,
        thread: 4,
        context: 9,
        start: 10,
        end: 20,
        received_frame: 0,
    });
    let mut shared: crate::analysis::SharedAnalysis = analysis.into();
    let held = shared.clone();
    let previous = held.revision();
    shared.add_event(7, 1, "renamed".into()).unwrap();
    shared.retain_recent(5).unwrap();
    assert_ne!(previous, shared.revision());
    assert_eq!(held.profile_event_name(7, 1), Some("simulate"));
    assert!(!held.profile_metadata_ambiguous(7, 1));
    assert_eq!(held.profile_summary().first_retained_frame, 0);
    assert_eq!(held.profile_spans().len(), 1);
    assert_eq!(held.profile_span(0).unwrap().end, 20);
    assert_eq!(shared.profile_summary().first_retained_frame, 5);
    assert_eq!(shared.profile_spans().len(), 0);
    assert!(shared.profile_metadata_ambiguous(7, 1));
}
