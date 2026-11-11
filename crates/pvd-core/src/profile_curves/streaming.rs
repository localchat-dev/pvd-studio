use super::*;
use crate::analysis::Analysis;
use crate::{
    RecordingSource,
    analysis::{ProfileSpan, ProfileZone},
    analysis_export::{
        AnalysisExportProgress, AnalysisExportReport, ProfileObserver, scan_profile,
    },
};
use pvd_protocol::Header;
use std::io::{Read, Seek};

pub const MAX_CATALOG_PAGE: usize = 4096;

const MAX_CATALOG_BYTES: usize = 32 << 20;
const MAX_INDEX_BYTES: usize = 64 << 20;
const CATALOG_ENTRY_BYTES: usize = size_of::<CatalogEntry>() + 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub blake3: [u8; 32],
    pub bytes: u64,
    pub frames: usize,
}

impl From<&AnalysisExportReport> for SourceIdentity {
    fn from(report: &AnalysisExportReport) -> Self {
        Self {
            blake3: report.source_blake3,
            bytes: report.source_bytes,
            frames: report.source_frames,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CatalogReport {
    pub identity: SourceIdentity,
    pub entries: Vec<CatalogEntry>,
    pub zones: BTreeMap<u64, ProfileZone>,
    /// Includes index nodes and owned names, separate from the decoder's state budget.
    pub estimated_index_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogCursor {
    pub identity: SourceIdentity,
    pub after: ExactKey,
}

#[derive(Clone, Debug)]
pub struct CatalogPageReport {
    pub identity: SourceIdentity,
    /// Ordered by zone, event, thread, then context, independently of payload arrival.
    pub entries: Vec<CatalogEntry>,
    pub zones: BTreeMap<u64, ProfileZone>,
    pub next: Option<CatalogCursor>,
    pub estimated_index_bytes: usize,
}

pub fn catalog_page_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    cursor: Option<&CatalogCursor>,
    limit: usize,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<CatalogPageReport, Error> {
    check_cancelled(cancelled)?;
    if !(1..=MAX_CATALOG_PAGE).contains(&limit) {
        return Err(error(
            "profile catalog page limit must be between 1 and 4096",
        ));
    }
    let mut catalog = Catalog {
        key: None,
        page: Some(Page {
            after: cursor.map(|cursor| cursor.after),
            limit,
            more: false,
        }),
        entries: BTreeMap::new(),
        bytes: 0,
        entry_limit: limit,
        byte_limit: MAX_CATALOG_BYTES,
        cancelled,
    };
    let report = scan_profile(source, &mut catalog, &mut |status| {
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    let identity = SourceIdentity::from(&report);
    if cursor.is_some_and(|cursor| cursor.identity != identity) {
        return Err(error(
            "profile catalog cursor belongs to a different source",
        ));
    }
    let mut zones = report.summary.zones;
    zones.retain(|&id, zone| {
        zone.events.retain(|&event, _| catalog.has_event(id, event));
        catalog.has_zone(id)
    });
    let next = catalog
        .page
        .as_ref()
        .filter(|page| page.more)
        .and_then(|_| {
            catalog
                .entries
                .last_key_value()
                .map(|(&after, _)| CatalogCursor {
                    identity: identity.clone(),
                    after,
                })
        });
    check_cancelled(cancelled)?;
    Ok(CatalogPageReport {
        identity,
        entries: catalog.entries.into_values().collect(),
        zones,
        next,
        estimated_index_bytes: catalog.bytes,
    })
}

#[derive(Clone, Debug)]
pub struct SampleReport {
    pub identity: SourceIdentity,
    pub points: Vec<Point>,
    pub estimated_index_bytes: usize,
}

pub fn catalog_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    cancelled: &AtomicBool,
    progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<CatalogReport, Error> {
    catalog_matching(source, None, cancelled, progress)
}

pub fn catalog_key_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    key: ExactKey,
    cancelled: &AtomicBool,
    progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<CatalogReport, Error> {
    catalog_matching(source, Some(key), cancelled, progress)
}

fn catalog_matching<R: Read + Seek + ?Sized>(
    source: &mut R,
    key: Option<ExactKey>,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<CatalogReport, Error> {
    check_cancelled(cancelled)?;
    let mut catalog = Catalog {
        key,
        page: None,
        entries: BTreeMap::new(),
        bytes: 0,
        entry_limit: MAX_SERIES,
        byte_limit: MAX_CATALOG_BYTES,
        cancelled,
    };
    let report = scan_profile(source, &mut catalog, &mut |status| {
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    let identity = SourceIdentity::from(&report);
    let mut entries: Vec<_> = catalog.entries.into_values().collect();
    entries
        .sort_unstable_by_key(|entry| (entry.first_start.is_none(), entry.first_start, entry.key));
    let mut zones = report.summary.zones;
    if let Some(key) = key {
        zones.retain(|&id, _| id == key.zone);
        for zone in zones.values_mut() {
            zone.events.retain(|&event, _| event == key.event);
        }
    }
    check_cancelled(cancelled)?;
    Ok(CatalogReport {
        identity,
        entries,
        zones,
        estimated_index_bytes: catalog.bytes,
    })
}

/// Match the returned source identity to the frame directory's original source before publishing.
pub fn sample_streaming<R: Read + Seek + ?Sized>(
    source: &mut R,
    recording: &Recording,
    key: ExactKey,
    scene: ReferenceScene,
    frames: &[usize],
    cancelled: &AtomicBool,
    mut progress: impl FnMut(AnalysisExportProgress) -> bool,
) -> Result<SampleReport, Error> {
    let mut sample = Samples::new(recording, key, scene, frames, cancelled)?;
    let report = scan_profile(source, &mut sample, &mut |status| {
        !cancelled.load(Ordering::Relaxed) && progress(status)
    })?;
    check_cancelled(cancelled)?;
    if report.source_frames != recording.frame_count() {
        return Err(error(
            "profile curve source frame count differs from its frame directory",
        ));
    }
    Ok(SampleReport {
        identity: SourceIdentity::from(&report),
        points: sample.points,
        estimated_index_bytes: sample.bytes,
    })
}

struct Page {
    after: Option<ExactKey>,
    limit: usize,
    more: bool,
}

struct Catalog<'a> {
    key: Option<ExactKey>,
    page: Option<Page>,
    entries: BTreeMap<ExactKey, CatalogEntry>,
    bytes: usize,
    entry_limit: usize,
    byte_limit: usize,
    cancelled: &'a AtomicBool,
}

impl Catalog<'_> {
    fn accepts(&self, key: ExactKey) -> bool {
        self.key.is_none_or(|expected| expected == key)
            && self
                .page
                .as_ref()
                .is_none_or(|page| page.after.is_none_or(|after| key > after))
    }

    fn has_zone(&self, zone: u64) -> bool {
        let first = ExactKey {
            zone,
            event: 0,
            thread: 0,
            context: 0,
        };
        self.entries
            .range(first..)
            .next()
            .is_some_and(|(key, _)| key.zone == zone)
    }

    fn has_event(&self, zone: u64, event: u16) -> bool {
        let first = ExactKey {
            zone,
            event,
            thread: 0,
            context: 0,
        };
        self.entries
            .range(first..)
            .next()
            .is_some_and(|(key, _)| key.zone == zone && key.event == event)
    }

    fn entry(&mut self, key: ExactKey) -> Result<Option<&mut CatalogEntry>, Error> {
        if !self.accepts(key) {
            return Ok(None);
        }
        if let Some(page) = &mut self.page
            && !self.entries.contains_key(&key)
            && self.entries.len() == page.limit
        {
            page.more = true;
            if self
                .entries
                .last_key_value()
                .is_some_and(|(&last, _)| key > last)
            {
                return Ok(None);
            }
            // The admission threshold only decreases, so an excluded key cannot reenter later.
            self.entries.pop_last();
            self.bytes -= CATALOG_ENTRY_BYTES;
        }
        let full = self.entries.len() >= self.entry_limit;
        Ok(Some(match self.entries.entry(key) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                if full {
                    return Err(error("complete profile catalog exceeds its series budget"));
                }
                charge(&mut self.bytes, CATALOG_ENTRY_BYTES, self.byte_limit)?;
                entry.insert(CatalogEntry {
                    key,
                    first_start: None,
                    complete_spans: 0,
                    open_spans: 0,
                    unmatched_stops: 0,
                    metadata_ambiguous: false,
                })
            }
        }))
    }
}

impl ProfileObserver for Catalog<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        for span in &analysis.spans {
            check_cancelled(self.cancelled)?;
            let key = (span.zone, span.event, span.thread, span.context).into();
            let Some(entry) = self.entry(key)? else {
                continue;
            };
            entry.complete_spans = entry
                .complete_spans
                .checked_add(1)
                .ok_or_else(|| error("profile catalog occurrence count overflow"))?;
            entry.first_start = Some(
                entry
                    .first_start
                    .map_or(span.start, |old| old.min(span.start)),
            );
        }
        Ok(())
    }

    fn finish(&mut self, analysis: &Analysis, _: &Header) -> Result<(), Error> {
        for (key, starts) in analysis.open_event_starts() {
            check_cancelled(self.cancelled)?;
            let Some(entry) = self.entry((*key).into())? else {
                continue;
            };
            entry.open_spans = starts.len();
            if let Some(&first) = starts.iter().min() {
                entry.first_start = Some(entry.first_start.map_or(first, |old| old.min(first)));
            }
        }
        for (key, &count) in &analysis.unmatched_stop_events {
            check_cancelled(self.cancelled)?;
            if let Some(entry) = self.entry((*key).into())? {
                entry.unmatched_stops = count;
            }
        }
        for entry in self.entries.values_mut() {
            check_cancelled(self.cancelled)?;
            entry.metadata_ambiguous =
                analysis.profile_metadata_ambiguous(entry.key.zone, entry.key.event);
        }
        for (&id, zone) in &analysis.zones {
            check_cancelled(self.cancelled)?;
            if self.key.is_some_and(|key| key.zone != id)
                || (self.page.is_some() && !self.has_zone(id))
            {
                continue;
            }
            charge(
                &mut self.bytes,
                size_of::<ProfileZone>() + 128 + zone.name.capacity(),
                self.byte_limit,
            )?;
            for (&event, name) in &zone.events {
                check_cancelled(self.cancelled)?;
                if self.key.is_some_and(|key| key.event != event)
                    || (self.page.is_some() && !self.has_event(id, event))
                {
                    continue;
                }
                charge(&mut self.bytes, 96 + name.capacity(), self.byte_limit)?;
            }
        }
        Ok(())
    }
}

struct Samples<'a> {
    key: ExactKey,
    points: Vec<Point>,
    windows: Vec<Window>,
    ticks: Vec<u128>,
    crossing: Vec<i64>,
    bytes: usize,
    cancelled: &'a AtomicBool,
}

impl<'a> Samples<'a> {
    fn new(
        recording: &Recording,
        key: ExactKey,
        scene: ReferenceScene,
        frames: &[usize],
        cancelled: &'a AtomicBool,
    ) -> Result<Self, Error> {
        check_cancelled(cancelled)?;
        if recording.source != RecordingSource::SdkStream
            || recording.frames.len() > MAX_RECORDS
            || recording.frames.len() != recording.frame_count()
        {
            return Err(error(
                "complete profile curves require a complete SDK frame directory",
            ));
        }
        if frames.len() > MAX_SAMPLES
            || frames.windows(2).any(|pair| pair[0] >= pair[1])
            || frames
                .iter()
                .any(|&frame| recording.frame_info(frame).is_none())
        {
            return Err(error(
                "profile curve sample indices must be sorted, unique, and within the 5000 point budget",
            ));
        }
        let mut bytes = 0;
        charge(
            &mut bytes,
            recording
                .frames
                .len()
                .saturating_mul(size_of::<usize>() * 4)
                .saturating_add(256),
            MAX_INDEX_BYTES,
        )?;
        charge(
            &mut bytes,
            frames
                .len()
                .saturating_mul(
                    size_of::<Point>() + size_of::<Window>() + size_of::<u128>() + size_of::<i64>(),
                )
                .saturating_add(size_of::<i64>()),
            MAX_INDEX_BYTES,
        )?;
        let reference = reference_frame_indices(recording, scene, cancelled)?;
        let monotonic = reference.windows(2).all(|pair| {
            recording.frame_info(pair[0]).unwrap().timestamp
                < recording.frame_info(pair[1]).unwrap().timestamp
        });
        let mut boundaries = Vec::new();
        for (index, frame) in recording.frames.iter().enumerate() {
            check_cancelled(cancelled)?;
            if index != frame.index {
                return Err(error(
                    "complete profile curves require a contiguous SDK frame directory",
                ));
            }
            if recording.frame_matches_scene(frame, scene.id) {
                boundaries.push(frame);
            }
        }
        let mut points = Vec::with_capacity(frames.len());
        let mut windows = Vec::with_capacity(frames.len());
        for &frame in frames {
            check_cancelled(cancelled)?;
            let position = boundaries.partition_point(|boundary| boundary.index < frame);
            let status = if boundaries
                .get(position)
                .is_none_or(|boundary| boundary.index != frame)
            {
                Status::OtherScene
            } else if !monotonic {
                Status::NonMonotonicClock
            } else if position == 0 {
                Status::FirstFrame
            } else if reference.binary_search(&frame).is_err()
                || reference
                    .binary_search(&boundaries[position - 1].index)
                    .is_err()
            {
                Status::OtherGeneration
            } else {
                windows.push(Window {
                    point: points.len(),
                    lower: boundaries[position - 1].timestamp,
                    upper: boundaries[position].timestamp,
                });
                Status::Missing
            };
            points.push(Point {
                frame,
                milliseconds: None,
                occurrence_count: 0,
                status,
            });
        }
        Ok(Self {
            key,
            ticks: vec![0; points.len()],
            crossing: vec![0; windows.len() + 1],
            points,
            windows,
            bytes,
            cancelled,
        })
    }

    fn span(&mut self, span: &ProfileSpan) -> Result<(), Error> {
        if (span.zone, span.event, span.thread, span.context) != self.key.tuple() {
            return Ok(());
        }
        if span.start > span.end {
            return Err(error("profile curve span ends before its start"));
        }
        let slot = self
            .windows
            .partition_point(|window| window.upper < span.end);
        if let Some(window) = self
            .windows
            .get(slot)
            .filter(|window| span.end > window.lower && span.start >= window.lower)
        {
            let point = &mut self.points[window.point];
            point.occurrence_count = point
                .occurrence_count
                .checked_add(1)
                .ok_or_else(|| error("profile curve occurrence count overflow"))?;
            self.ticks[window.point] = self.ticks[window.point]
                .checked_add(u128::from(span.end - span.start))
                .ok_or_else(|| error("profile curve duration overflow"))?;
        } else {
            let first = self
                .windows
                .partition_point(|window| window.upper <= span.start);
            let after = self
                .windows
                .partition_point(|window| window.lower < span.end);
            if first < after {
                self.crossing[first] = self.crossing[first]
                    .checked_add(1)
                    .ok_or_else(|| error("profile curve crossing count overflow"))?;
                self.crossing[after] = self.crossing[after]
                    .checked_sub(1)
                    .ok_or_else(|| error("profile curve crossing count overflow"))?;
            }
        }
        Ok(())
    }
}

impl ProfileObserver for Samples<'_> {
    fn batch(&mut self, analysis: &Analysis) -> Result<(), Error> {
        for span in &analysis.spans {
            check_cancelled(self.cancelled)?;
            self.span(span)?;
        }
        Ok(())
    }

    fn finish(&mut self, analysis: &Analysis, header: &Header) -> Result<(), Error> {
        let scale = header.timestamp_numerator as f64 / header.timestamp_denominator as f64 / 1e6;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(error("invalid profile curve clock ratio"));
        }
        let ambiguous = analysis.profile_metadata_ambiguous(self.key.zone, self.key.event);
        let unmatched = analysis
            .unmatched_stop_events
            .get(&self.key.tuple())
            .copied()
            .unwrap_or(0)
            != 0;
        if ambiguous || unmatched {
            for point in &mut self.points {
                check_cancelled(self.cancelled)?;
                if point.status != Status::OtherScene {
                    point.status = if ambiguous {
                        Status::MetadataAmbiguous
                    } else {
                        Status::UnmatchedStop
                    };
                    point.occurrence_count = 0;
                }
            }
            return Ok(());
        }
        let earliest_open = analysis
            .open_event_starts()
            .filter(|(key, _)| **key == self.key.tuple())
            .flat_map(|(_, starts)| starts.iter().copied())
            .min();
        let mut crossing = 0i64;
        for (index, window) in self.windows.iter().enumerate() {
            check_cancelled(self.cancelled)?;
            crossing = crossing
                .checked_add(self.crossing[index])
                .ok_or_else(|| error("profile curve crossing count overflow"))?;
            let point = &mut self.points[window.point];
            if crossing > 0 {
                point.status = Status::CrossBoundary;
            } else if earliest_open.is_some_and(|start| start <= window.upper) {
                point.status = Status::OpenSpan;
            } else if point.occurrence_count > 0 {
                point.status = Status::Recorded;
                point.milliseconds = Some(self.ticks[window.point] as f64 * scale);
            }
        }
        Ok(())
    }
}

fn charge(bytes: &mut usize, additional: usize, limit: usize) -> Result<(), Error> {
    *bytes = bytes
        .checked_add(additional)
        .filter(|&total| total <= limit)
        .ok_or_else(|| error("complete profile curve index exceeds its metadata budget"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_checks_series_and_bytes_before_inserting() {
        let cancelled = AtomicBool::new(false);
        let mut catalog = Catalog {
            key: None,
            page: None,
            entries: BTreeMap::new(),
            bytes: 0,
            entry_limit: 1,
            byte_limit: CATALOG_ENTRY_BYTES,
            cancelled: &cancelled,
        };
        let key = ExactKey {
            zone: 1,
            event: 1,
            thread: 1,
            context: 1,
        };
        catalog.entry(key).unwrap();
        catalog.entry(key).unwrap();
        assert_eq!(catalog.entries.len(), 1);
        assert!(catalog.entry(ExactKey { context: 2, ..key }).is_err());
        let mut catalog = Catalog {
            key: None,
            page: None,
            entries: BTreeMap::new(),
            bytes: 0,
            entry_limit: 2,
            byte_limit: CATALOG_ENTRY_BYTES - 1,
            cancelled: &cancelled,
        };
        assert!(catalog.entry(key).is_err());
        assert!(catalog.entries.is_empty());
        let mut analysis = Analysis::default();
        analysis.add_zone(1, "a metadata name".into()).unwrap();
        catalog.bytes = 0;
        catalog.byte_limit = 1;
        assert!(
            catalog
                .finish(
                    &analysis,
                    &Header {
                        version: 1,
                        timestamp_numerator: 1,
                        timestamp_denominator: 1,
                        flags: 1,
                        endian: pvd_protocol::Endian::Little
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn page_replacement_releases_entry_charges_and_only_selected_names_are_charged() {
        let cancelled = AtomicBool::new(false);
        let mut analysis = Analysis::default();
        analysis.add_zone(1, "kept zone".into()).unwrap();
        analysis.add_zone(2, "unselected zone".repeat(100)).unwrap();
        analysis
            .zones
            .get_mut(&1)
            .unwrap()
            .events
            .insert(1, "kept event".into());
        analysis
            .zones
            .get_mut(&1)
            .unwrap()
            .events
            .insert(2, "unselected event".repeat(100));
        let zone = &analysis.zones[&1];
        let limit = CATALOG_ENTRY_BYTES
            + size_of::<ProfileZone>()
            + 128
            + zone.name.capacity()
            + 96
            + zone.events[&1].capacity();
        let mut catalog = Catalog {
            key: None,
            page: Some(Page {
                after: None,
                limit: 1,
                more: false,
            }),
            entries: BTreeMap::new(),
            bytes: 0,
            entry_limit: 1,
            byte_limit: limit,
            cancelled: &cancelled,
        };
        let key = ExactKey {
            zone: 1,
            event: 1,
            thread: 1,
            context: 9,
        };
        assert!(catalog.entry(key).unwrap().is_some());
        assert!(
            catalog
                .entry(ExactKey { context: 10, ..key })
                .unwrap()
                .is_none()
        );
        assert!(
            catalog
                .entry(ExactKey { context: 1, ..key })
                .unwrap()
                .is_some()
        );
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.bytes, CATALOG_ENTRY_BYTES);
        assert!(catalog.page.as_ref().unwrap().more);
        let header = Header {
            version: 1,
            timestamp_numerator: 1,
            timestamp_denominator: 1,
            flags: 1,
            endian: pvd_protocol::Endian::Little,
        };
        catalog.finish(&analysis, &header).unwrap();
        assert_eq!(catalog.bytes, limit);
        catalog.bytes = CATALOG_ENTRY_BYTES;
        catalog.byte_limit = limit - 1;
        assert!(catalog.finish(&analysis, &header).is_err());
    }
}
