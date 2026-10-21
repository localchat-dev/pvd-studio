use super::*;
use crate::analysis::SharedAnalysis;
use crate::analysis::resident::ResidentBudgetExceeded;

mod external_cache;
pub(super) use external_cache::ExternalCache;

pub(super) const ANALYSIS_CHECKPOINT_FRAMES: usize = 32;
const MAX_ANALYSIS_REPLAY_FRAMES: usize = 512;
const UNDO_FRAMES: usize = 120;
const UNDO_BYTES: usize = 8 << 20;
const REUSED_ANALYSIS_BYTES: usize = 1 << 20;

pub struct SceneReplayFrame {
    pub frame: usize,
    pub update: SceneUpdate,
}

pub struct ReplayFrame {
    pub update: SceneUpdate,
    pub analysis: SharedAnalysis,
}

/// Baseline and cache timings are part of `analysis`; applying the scene update is excluded.
#[derive(Clone, Debug, Default)]
pub struct ReplaySeekMetrics {
    pub total: Duration,
    pub index: Duration,
    pub scene: Duration,
    pub analysis: Duration,
    pub baseline_io: Duration,
    pub baseline_decode: Duration,
    pub baseline_pages: Duration,
    pub baseline_sharing: Duration,
    pub cache_maintenance: Duration,
    pub baseline_bytes: usize,
    pub baseline_loads: usize,
    pub analysis_frames: usize,
    pub analysis_inputs: usize,
    pub cache_hits: usize,
    pub shared_cache_entries: usize,
    pub shared_cache_bytes: usize,
    pub cursor_cache_bytes: usize,
    pub resident_domain_bytes: usize,
    pub analysis_root_hits: usize,
}

#[derive(Default)]
struct SeekProfile {
    enabled: bool,
    metrics: ReplaySeekMetrics,
}

impl SeekProfile {
    fn start(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    fn elapsed(start: Option<Instant>) -> Duration {
        start.map_or(Duration::ZERO, |start| start.elapsed())
    }
}

/// Display updates must be applied in order; clear the cursor if the display replaces their base state.
#[derive(Default)]
pub struct ReplayCursor {
    current: Option<Cursor>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SceneRouteCounts {
    pub index: usize,
    pub memory_undo: usize,
    pub persistent_undo: usize,
    pub adjacent_forward: usize,
}

impl ReplayCursor {
    #[cfg(test)]
    pub(super) fn scene_route_counts(&self) -> SceneRouteCounts {
        self.current
            .as_ref()
            .map_or_else(SceneRouteCounts::default, |cursor| cursor.scene_routes)
    }

    pub fn clear(&mut self) {
        self.current = None;
    }

    pub fn seek(
        &mut self,
        recording: &crate::Recording,
        frame: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<ReplayFrame, Error> {
        self.seek_inner(
            recording,
            frame,
            &mut cancelled,
            &mut SeekProfile::default(),
        )
    }

    pub fn seek_scene(
        &mut self,
        recording: &crate::Recording,
        frame: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<SceneReplayFrame, Error> {
        self.with_cursor(
            recording,
            frame,
            &mut cancelled,
            |cursor, disk, segment, cancelled| {
                let (update, _) = cursor.read_scene(
                    disk,
                    segment,
                    frame,
                    false,
                    cancelled,
                    &mut SeekProfile::default(),
                )?;
                cursor.frame = Some(frame);
                Ok(SceneReplayFrame { frame, update })
            },
        )
    }

    pub fn seek_profiled(
        &mut self,
        recording: &crate::Recording,
        frame: usize,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<(ReplayFrame, ReplaySeekMetrics), Error> {
        let mut profile = SeekProfile {
            enabled: true,
            ..Default::default()
        };
        let start = Instant::now();
        let replay = self.seek_inner(recording, frame, &mut cancelled, &mut profile)?;
        profile.metrics.total = start.elapsed();
        Ok((replay, profile.metrics))
    }

    fn seek_inner(
        &mut self,
        recording: &crate::Recording,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
        profile: &mut SeekProfile,
    ) -> Result<ReplayFrame, Error> {
        self.with_cursor(
            recording,
            frame,
            cancelled,
            |cursor, disk, segment, cancelled| {
                let result = cursor.seek(disk, segment, frame, cancelled, profile)?;
                if profile.enabled {
                    let (entries, bytes) = disk.analysis_cache.stats();
                    profile.metrics.shared_cache_entries = entries;
                    profile.metrics.shared_cache_bytes = bytes;
                    profile.metrics.resident_domain_bytes = result
                        .analysis
                        .resident()
                        .map_or(0, |analysis| analysis.domain().statistics().logical_used);
                }
                Ok(result)
            },
        )
    }

    fn with_cursor<T, F: FnMut() -> bool>(
        &mut self,
        recording: &crate::Recording,
        frame: usize,
        cancelled: &mut F,
        operation: impl FnOnce(&mut Cursor, &DiskRecording, &SegmentView, &mut F) -> Result<T, Error>,
    ) -> Result<T, Error> {
        check_replay_cancelled(cancelled)?;
        let disk = recording
            .disk
            .as_ref()
            .ok_or_else(|| error("a disk-backed recording is required for the replay cursor"))?;
        let segment = disk.segment(frame)?;
        if !segment.lease.available() {
            return Err(error("history segment expired"));
        }
        let mut cursor = match self.current.take() {
            Some(cursor) if cursor.resources.same(&segment.resources) => cursor,
            _ => Cursor::open(segment)?,
        };
        let result = operation(&mut cursor, disk, segment, cancelled)?;
        check_replay_cancelled(cancelled)?;
        self.current = Some(cursor);
        Ok(result)
    }
}

#[derive(Clone)]
struct AnalysisCheckpoint {
    frame: usize,
    data: SharedAnalysis,
}

#[derive(Default)]
struct UndoCache {
    frames: BTreeMap<usize, UndoFrame>,
    bytes: usize,
}

struct UndoFrame {
    changes: IndexUpdate,
    removed_generations: BTreeSet<u64>,
    bytes: usize,
}

type IndexChanges = (IndexUpdate, BTreeSet<u64>);

impl UndoCache {
    fn remember(&mut self, frame: usize, changes: &IndexUpdate, before: &StateIndex) {
        let bytes = 512usize
            .saturating_add(changes.objects.len().saturating_mul(32))
            .saturating_add(changes.debug.len().saturating_mul(32))
            .saturating_add(changes.contacts.len().saturating_mul(32))
            .saturating_add(
                changes
                    .cameras
                    .keys()
                    .map(|name| camera_entry_bytes(name))
                    .sum::<usize>(),
            )
            .saturating_add(changes.generations.len().saturating_mul(160))
            .saturating_add(if changes.errors.is_some() {
                before
                    .errors
                    .iter()
                    .map(|event| 128 + event.message.len() + event.file.len())
                    .sum()
            } else {
                0
            });
        if bytes > UNDO_BYTES {
            *self = Self::default();
            return;
        }
        let prior = |changes: &[(u64, Option<u64>)], values: &BTreeMap<u64, u64>| {
            changes
                .iter()
                .map(|&(id, _)| (id, values.get(&id).copied()))
                .collect()
        };
        let undo = UndoFrame {
            changes: IndexUpdate {
                objects: prior(&changes.objects, &before.objects),
                debug: prior(&changes.debug, &before.debug),
                contacts: prior(&changes.contacts, &before.contacts),
                cameras: changes
                    .cameras
                    .keys()
                    .map(|name| (name.clone(), before.cameras.get(name).copied()))
                    .collect(),
                generations: changes
                    .generations
                    .keys()
                    .filter_map(|id| {
                        before
                            .generations
                            .get(id)
                            .map(|generation| (*id, *generation))
                    })
                    .collect(),
                errors: changes.errors.as_ref().map(|_| before.errors.clone()),
            },
            removed_generations: changes
                .generations
                .keys()
                .filter(|id| !before.generations.contains_key(id))
                .copied()
                .collect(),
            bytes,
        };
        if let Some(old) = self.frames.insert(frame, undo) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
        while self.frames.len() > UNDO_FRAMES || self.bytes > UNDO_BYTES {
            self.bytes -= self.frames.pop_first().unwrap().1.bytes;
        }
    }

    fn rewind(
        &self,
        index: &mut StateIndex,
        from: usize,
        to: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<IndexChanges>, Error> {
        if self.frames.range(to + 1..=from).count() != from - to {
            return Ok(None);
        }
        let mut objects = BTreeSet::new();
        let mut debug = BTreeSet::new();
        let mut contacts = BTreeSet::new();
        let mut cameras = BTreeSet::new();
        let mut generations = BTreeSet::new();
        let mut errors = false;
        for (_, undo) in self.frames.range(to + 1..=from).rev() {
            check_replay_cancelled(cancelled)?;
            objects.extend(undo.changes.objects.iter().map(|entry| entry.0));
            debug.extend(undo.changes.debug.iter().map(|entry| entry.0));
            contacts.extend(undo.changes.contacts.iter().map(|entry| entry.0));
            cameras.extend(undo.changes.cameras.keys().cloned());
            generations.extend(undo.changes.generations.keys().copied());
            generations.extend(&undo.removed_generations);
            errors |= undo.changes.errors.is_some();
            for id in &undo.removed_generations {
                index.generations.remove(id);
            }
            undo.changes.apply(index);
        }
        let offsets = |ids: BTreeSet<u64>, values: &BTreeMap<u64, u64>| {
            ids.into_iter()
                .map(|id| (id, values.get(&id).copied()))
                .collect()
        };
        let removed = generations
            .iter()
            .filter(|id| !index.generations.contains_key(id))
            .copied()
            .collect();
        Ok(Some((
            IndexUpdate {
                objects: offsets(objects, &index.objects),
                debug: offsets(debug, &index.debug),
                contacts: offsets(contacts, &index.contacts),
                cameras: cameras
                    .into_iter()
                    .map(|name| {
                        let value = index.cameras.get(&name).copied();
                        (name, value)
                    })
                    .collect(),
                generations: generations
                    .into_iter()
                    .filter_map(|id| {
                        index
                            .generations
                            .get(&id)
                            .map(|generation| (id, *generation))
                    })
                    .collect(),
                errors: errors.then(|| index.errors.clone()),
            },
            removed,
        )))
    }
}

struct Cursor {
    path: PathBuf,
    resources: Resources,
    reader: HistoryReader,
    frame: Option<usize>,
    index: StateIndex,
    analysis: Option<AnalysisCheckpoint>,
    undo: UndoCache,
    external_cache: ExternalCache,
    #[cfg(test)]
    scene_routes: SceneRouteCounts,
}

impl Cursor {
    fn open(segment: &SegmentView) -> Result<Self, Error> {
        Ok(Self {
            path: segment.path.clone(),
            resources: segment.resources.clone(),
            reader: DiskRecording::open(segment)?,
            frame: None,
            index: StateIndex::default(),
            analysis: None,
            undo: UndoCache::default(),
            external_cache: ExternalCache::default(),
            #[cfg(test)]
            scene_routes: SceneRouteCounts::default(),
        })
    }

    fn select_segment(&mut self, segment: &SegmentView) -> Result<(), Error> {
        if self.path != segment.path {
            self.reader = DiskRecording::open(segment)?;
            self.path = segment.path.clone();
        }
        Ok(())
    }

    fn seek(
        &mut self,
        disk: &DiskRecording,
        segment: &SegmentView,
        frame: usize,
        cancelled: &mut impl FnMut() -> bool,
        profile: &mut SeekProfile,
    ) -> Result<ReplayFrame, Error> {
        if self.frame == Some(frame)
            && self
                .analysis
                .as_ref()
                .is_some_and(|analysis| analysis.frame == frame)
        {
            self.select_segment(segment)?;
            return Ok(ReplayFrame {
                update: SceneUpdate::default(),
                analysis: self.analysis.as_ref().unwrap().data.clone(),
            });
        }
        let (update, analysis_inputs) =
            self.read_scene(disk, segment, frame, true, cancelled, profile)?;
        let started = profile.start();
        self.seek_analysis(disk, segment, frame, analysis_inputs, cancelled, profile)?;
        profile.metrics.analysis += SeekProfile::elapsed(started);
        self.frame = Some(frame);
        Ok(ReplayFrame {
            update,
            analysis: self.analysis.as_ref().unwrap().data.clone(),
        })
    }

    fn read_scene(
        &mut self,
        disk: &DiskRecording,
        segment: &SegmentView,
        frame: usize,
        reuse_analysis: bool,
        cancelled: &mut impl FnMut() -> bool,
        profile: &mut SeekProfile,
    ) -> Result<(SceneUpdate, Option<Vec<AnalysisInput>>), Error> {
        self.select_segment(segment)?;
        if self.frame == Some(frame) {
            #[cfg(all(test, feature = "scene-read-census"))]
            census::route(
                &self.resources,
                frame,
                self.frame,
                segment.frames[0].info.index,
                "same_frame",
            );
            return Ok((SceneUpdate::default(), None));
        }
        let started = profile.start();
        let replace_all = self.frame.is_none();
        let rewound = match self.frame {
            Some(from) if from > frame => {
                self.undo.rewind(&mut self.index, from, frame, cancelled)?
            }
            _ => None,
        };
        let mut undo_source = None;
        let persistent = if rewound.is_none() && frame.checked_add(1) == self.frame {
            let from = self.frame.unwrap();
            let source = disk.segment(from)?;
            if !source.resources.same(&segment.resources) || !source.lease.available() {
                return Err(error("scene undo source changed or expired"));
            }
            let position = source
                .frames
                .binary_search_by_key(&from, |location| location.info.index)
                .map_err(|_| error("scene undo source frame is absent"))?;
            let location = &source.frames[position];
            if location.undo.is_some() {
                self.select_segment(source)?;
                let stored = undo::read(&mut self.reader, source, location, frame, cancelled)?;
                if stored.is_some() {
                    undo_source = Some(source);
                }
                self.select_segment(segment)?;
                stored
            } else {
                None
            }
        } else {
            None
        };
        #[cfg(test)]
        let persistent_route = persistent.is_some();
        let adjacent = self.frame.and_then(|f| f.checked_add(1)) == Some(frame)
            && (segment.frames.first().unwrap().info.index != frame
                || segment.previous_frame == self.frame);
        #[cfg(all(test, feature = "scene-read-census"))]
        census::route(
            &self.resources,
            frame,
            self.frame,
            segment.frames[0].info.index,
            if adjacent {
                "adjacent_forward"
            } else if persistent.is_some() {
                "persistent_undo"
            } else if rewound.is_some() {
                "undo"
            } else {
                "index"
            },
        );
        #[cfg(test)]
        let memory_route = rewound.is_some();
        let mut analysis_inputs = None;
        let (changes, removed_generations) = if adjacent {
            let stored = read_frame(&mut self.reader, segment, frame, cancelled)?;
            self.undo.remember(frame, &stored.update, &self.index);
            if reuse_analysis {
                analysis_inputs = reusable_analysis_inputs(stored.analysis);
            }
            (stored.update, BTreeSet::new())
        } else if let Some(stored) = persistent {
            (stored.changes, stored.removed_generations)
        } else if let Some(changes) = rewound {
            changes
        } else {
            let index = read_index(&mut self.reader, segment, frame, &mut self.undo, cancelled)?;
            let removed = self
                .index
                .generations
                .keys()
                .filter(|id| !index.generations.contains_key(id))
                .copied()
                .collect();
            let changes = IndexUpdate {
                objects: changed_offsets(&self.index.objects, &index.objects),
                debug: changed_offsets(&self.index.debug, &index.debug),
                contacts: changed_offsets(&self.index.contacts, &index.contacts),
                cameras: camera_changes(&self.index.cameras, &index.cameras),
                generations: index
                    .generations
                    .iter()
                    .filter(|(id, generation)| self.index.generations.get(id) != Some(generation))
                    .map(|(&id, &generation)| (id, generation))
                    .collect(),
                errors: (replace_all || self.index.errors != index.errors).then_some(index.errors),
            };
            (changes, removed)
        };
        profile.metrics.index += SeekProfile::elapsed(started);
        let started = profile.start();
        let mut update = read_update(
            &mut self.reader,
            segment,
            &changes,
            &mut self.external_cache,
            cancelled,
        )?;
        update.replace_all = replace_all;
        update.removed_generations = removed_generations;
        for id in &update.removed_generations {
            self.index.generations.remove(id);
        }
        changes.apply(&mut self.index);
        check_replay_cancelled(cancelled)?;
        if !segment.lease.available() || undo_source.is_some_and(|source| !source.lease.available())
        {
            return Err(error("scene undo source or target segment expired"));
        }
        #[cfg(test)]
        {
            if adjacent {
                self.scene_routes.adjacent_forward += 1;
            } else if persistent_route {
                self.scene_routes.persistent_undo += 1;
            } else if memory_route {
                self.scene_routes.memory_undo += 1;
            } else {
                self.scene_routes.index += 1;
            }
        }
        profile.metrics.scene += SeekProfile::elapsed(started);
        Ok((update, analysis_inputs))
    }

    fn baseline(
        &mut self,
        disk: &DiskRecording,
        segment: &SegmentView,
        cancelled: &mut impl FnMut() -> bool,
        profile: &mut SeekProfile,
    ) -> Result<AnalysisCheckpoint, Error> {
        self.select_segment(segment)?;
        let first = segment.frames.first().unwrap().info.index;
        if let Some((frame, data)) = disk.analysis_cache.best(first, first) {
            profile.metrics.cache_hits += 1;
            return Ok(AnalysisCheckpoint { frame, data });
        }
        let started = profile.start();
        let bytes = read_record_bytes_with_cancel(
            &mut self.reader,
            segment.analysis_base,
            segment.committed_end,
            cancelled,
        )?;
        profile.metrics.baseline_io += SeekProfile::elapsed(started);
        profile.metrics.baseline_bytes += bytes.len();
        profile.metrics.baseline_loads += 1;
        let started = profile.start();
        let data = match self.reader.decode(&bytes, cancelled)? {
            Record::Analysis(mut analysis) => {
                profile.metrics.baseline_decode += SeekProfile::elapsed(started);
                let sharing = profile.start();
                analysis.restore_sharing();
                profile.metrics.baseline_sharing += SeekProfile::elapsed(sharing);
                (*analysis).into()
            }
            Record::AnalysisPages(root) => {
                let mut reader = disk
                    .analysis_pages
                    .as_ref()
                    .ok_or_else(|| error("history has no shared analysis reader"))?
                    .clone();
                let analysis = match reader.read(root, &segment.resources, cancelled) {
                    Ok(analysis) => analysis,
                    Err(failure) if failure.downcast_ref::<ResidentBudgetExceeded>().is_some() => {
                        disk.analysis_cache.clear_resident();
                        reader.reclaim_with_cancel(cancelled)?;
                        reader.read(root, &segment.resources, cancelled)?
                    }
                    Err(failure) => return Err(failure),
                };
                profile.metrics.baseline_pages += SeekProfile::elapsed(started);
                analysis.into()
            }
            _ => return Err(error("history analysis record type mismatch")),
        };
        check_replay_cancelled(cancelled)?;
        let checkpoint = AnalysisCheckpoint { frame: first, data };
        let started = profile.start();
        disk.analysis_cache
            .insert(first, checkpoint.data.clone(), &segment.lease);
        profile.metrics.cache_maintenance += SeekProfile::elapsed(started);
        Ok(checkpoint)
    }

    fn remember_analysis(&self, disk: &DiskRecording, profile: &mut SeekProfile) {
        let started = profile.start();
        let checkpoint = self.analysis.as_ref().unwrap();
        if let Ok(segment) = disk.segment(checkpoint.frame) {
            if segment.frames.first().unwrap().info.index == checkpoint.frame {
                disk.analysis_cache.insert_segment_checkpoint(
                    checkpoint.frame,
                    checkpoint.data.clone(),
                    &segment.lease,
                );
            } else {
                disk.analysis_cache.insert_checkpoint(
                    checkpoint.frame,
                    checkpoint.data.clone(),
                    &segment.lease,
                );
            }
        }
        profile.metrics.cache_maintenance += SeekProfile::elapsed(started);
    }

    fn seek_analysis(
        &mut self,
        disk: &DiskRecording,
        segment: &SegmentView,
        frame: usize,
        mut analysis_inputs: Option<Vec<AnalysisInput>>,
        cancelled: &mut impl FnMut() -> bool,
        profile: &mut SeekProfile,
    ) -> Result<(), Error> {
        let first = disk
            .segments
            .iter()
            .map(Arc::as_ref)
            .chain(&disk.active)
            .filter(|segment| segment.lease.available())
            .find_map(|segment| segment.frames.first().map(|location| location.info.index))
            .ok_or_else(|| error("history segment expired"))?;
        let cache_first = first.max(frame.saturating_sub(MAX_ANALYSIS_REPLAY_FRAMES));
        let mut best = self
            .analysis
            .as_ref()
            .filter(|value| value.frame >= cache_first && value.frame <= frame)
            .cloned();
        if let Some((frame, data)) = disk.analysis_cache.best(cache_first, frame)
            && best.as_ref().is_none_or(|value| value.frame < frame)
        {
            best = Some(AnalysisCheckpoint { frame, data });
        }
        let baseline = best.is_none();
        profile.metrics.cache_hits += usize::from(!baseline);
        profile.metrics.analysis_root_hits += usize::from(
            best.as_ref()
                .is_some_and(|root| root.frame == frame && root.data.resident().is_some()),
        );
        if baseline {
            self.analysis = None;
        }
        self.analysis = Some(match best {
            Some(value) => value,
            None => self.baseline(disk, segment, cancelled, profile)?,
        });
        let start = self.analysis.as_ref().unwrap().frame;
        let checkpoint = (frame / ANALYSIS_CHECKPOINT_FRAMES * ANALYSIS_CHECKPOINT_FRAMES)
            .max(segment.frames.first().unwrap().info.index);
        for (source, location) in disk.locations(start.saturating_add(1), frame) {
            check_replay_cancelled(cancelled)?;
            if location.info.index <= self.analysis.as_ref().unwrap().frame {
                continue;
            }
            if !source.lease.available() {
                self.analysis = None;
                self.analysis = Some(self.baseline(disk, segment, cancelled, profile)?);
                continue;
            }
            self.select_segment(source)?;
            let segment_first = source.frames.first().unwrap().info.index;
            if location.info.index == segment_first
                && (!source.analysis_continuity
                    || self.analysis.as_ref().unwrap().frame.checked_add(1) != Some(segment_first))
            {
                self.analysis = None;
                self.analysis = Some(match self.baseline(disk, source, cancelled, profile) {
                    Ok(value) => value,
                    Err(_) if !source.lease.available() => {
                        self.baseline(disk, segment, cancelled, profile)?
                    }
                    Err(error) => return Err(error),
                });
            } else {
                let inputs = if location.info.index == frame && std::ptr::eq(source, segment) {
                    analysis_inputs.take()
                } else {
                    None
                };
                let inputs = match inputs {
                    Some(inputs) => inputs,
                    None => {
                        read_frame(&mut self.reader, source, location.info.index, cancelled)?
                            .analysis
                    }
                };
                let analysis = self.analysis.as_mut().unwrap();
                if analysis.data.resident().is_some() {
                    analysis.data = apply_analysis_frame(disk, &analysis.data, &inputs, cancelled)?;
                } else {
                    for input in &inputs {
                        check_replay_cancelled(cancelled)?;
                        input.apply_shared(&mut analysis.data, cancelled)?;
                    }
                }
                profile.metrics.analysis_frames += 1;
                profile.metrics.analysis_inputs += inputs.len();
                analysis.frame = location.info.index;
            }
            if location.info.index == checkpoint
                || self
                    .analysis
                    .as_ref()
                    .is_some_and(|root| root.data.resident().is_some())
            {
                self.remember_analysis(disk, profile);
            }
        }
        self.select_segment(segment)?;
        check_replay_cancelled(cancelled)
    }
}

fn apply_analysis_frame(
    disk: &DiskRecording,
    before: &SharedAnalysis,
    inputs: &[AnalysisInput],
    cancelled: &mut impl FnMut() -> bool,
) -> Result<SharedAnalysis, Error> {
    let stage = |cancelled: &mut _| {
        let mut draft = before.clone();
        for input in inputs {
            check_replay_cancelled(cancelled)?;
            input.apply_shared(&mut draft, cancelled)?;
        }
        check_replay_cancelled(cancelled)?;
        Ok::<_, Error>(draft)
    };
    match stage(cancelled) {
        Ok(draft) => Ok(draft),
        Err(failure) if failure.downcast_ref::<ResidentBudgetExceeded>().is_some() => {
            let Some(root) = before.resident() else {
                return Err(failure);
            };
            disk.analysis_cache.reclaim_resident(root.domain());
            if let Some(mut reader) = disk.analysis_pages.clone() {
                reader.reclaim_with_cancel(cancelled)?;
            }
            stage(cancelled)
        }
        Err(failure) => Err(failure),
    }
}

fn reusable_analysis_inputs(inputs: Vec<AnalysisInput>) -> Option<Vec<AnalysisInput>> {
    let fixed = inputs
        .capacity()
        .checked_mul(size_of::<AnalysisInput>())?
        .checked_add(size_of::<Vec<AnalysisInput>>())?;
    let bytes = inputs.iter().try_fold(fixed, |total, input| {
        let dynamic = match input {
            AnalysisInput::Reset(_) | AnalysisInput::Retain { .. } => 0,
            AnalysisInput::Zone(_, name) | AnalysisInput::Event(_, _, name) => name.capacity(),
            AnalysisInput::Buffer { kind, bytes, .. } => kind
                .capacity()
                .checked_add(bytes.len())?
                .checked_add(2 * size_of::<usize>())?,
        };
        let total = total.checked_add(dynamic)?;
        (total <= REUSED_ANALYSIS_BYTES).then_some(total)
    })?;
    (bytes <= REUSED_ANALYSIS_BYTES).then_some(inputs)
}

fn read_frame(
    reader: &mut HistoryReader,
    segment: &SegmentView,
    frame: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<StoredFrame, Error> {
    let position = segment
        .frames
        .binary_search_by_key(&frame, |location| location.info.index)
        .map_err(|_| error(format!("frame {frame} is missing from disk history")))?;
    let location = &segment.frames[position];
    #[cfg(all(test, feature = "replay-test-read-counts"))]
    frame_reuse_tests::record_read(frame);
    let Record::Frame(stored) =
        read_record_with_cancel(reader, location.offset, segment.committed_end, cancelled)?
    else {
        return Err(error("history frame record type mismatch"));
    };
    if stored.info.index != frame
        || stored.info.timestamp != location.info.timestamp
        || stored.info.scene_id != location.info.scene_id
    {
        return Err(error("history frame metadata differs from its index"));
    }
    #[cfg(all(test, feature = "replay-test-read-counts"))]
    frame_reuse_tests::record_validated(frame, &stored.analysis);
    Ok(stored)
}

fn read_index(
    reader: &mut HistoryReader,
    segment: &SegmentView,
    frame: usize,
    undo: &mut UndoCache,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<StateIndex, Error> {
    let (&checkpoint, &offset) = segment
        .checkpoints
        .range(..=frame)
        .next_back()
        .ok_or_else(|| error("history checkpoint missing"))?;
    let Record::Checkpoint(mut index) =
        read_record_with_cancel(reader, offset, segment.committed_end, cancelled)?
    else {
        return Err(error("history checkpoint record type mismatch"));
    };
    *undo = UndoCache::default();
    let mut previous = checkpoint;
    for location in segment
        .frames
        .iter()
        .filter(|location| location.info.index > checkpoint && location.info.index <= frame)
    {
        let stored = read_frame(reader, segment, location.info.index, cancelled)?;
        if previous.checked_add(1) == Some(location.info.index) {
            undo.remember(location.info.index, &stored.update, &index);
        } else {
            *undo = UndoCache::default();
        }
        stored.update.apply(&mut index);
        previous = location.info.index;
    }
    Ok(index)
}

fn changed_offsets(
    previous: &BTreeMap<u64, u64>,
    next: &BTreeMap<u64, u64>,
) -> Vec<(u64, Option<u64>)> {
    let mut result = Vec::new();
    let mut previous = previous.iter().peekable();
    let mut next = next.iter().peekable();
    loop {
        match (previous.peek(), next.peek()) {
            (Some(&(&id, &offset)), Some(&(&next_id, &next_offset))) if id == next_id => {
                if offset != next_offset {
                    result.push((id, Some(next_offset)));
                }
                previous.next();
                next.next();
            }
            (Some(&(&id, _)), next_entry)
                if next_entry.is_none_or(|&(&next_id, _)| id < next_id) =>
            {
                result.push((id, None));
                previous.next();
            }
            (_, Some(&(&id, &offset))) => {
                result.push((id, Some(offset)));
                next.next();
            }
            (None, None) => break,
            _ => unreachable!(),
        }
    }
    result
}

fn camera_changes(
    before: &BTreeMap<String, SourceCamera>,
    after: &BTreeMap<String, SourceCamera>,
) -> BTreeMap<String, Option<SourceCamera>> {
    before
        .keys()
        .filter(|name| before.get(*name) != after.get(*name))
        .map(|name| (name.clone(), after.get(name).copied()))
        .chain(
            after
                .iter()
                .filter(|(name, _)| !before.contains_key(*name))
                .map(|(name, camera)| (name.clone(), Some(*camera))),
        )
        .collect()
}

fn read_update(
    reader: &mut HistoryReader,
    segment: &SegmentView,
    changes: &IndexUpdate,
    external_cache: &mut ExternalCache,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<SceneUpdate, Error> {
    #[cfg(all(test, feature = "scene-read-census"))]
    census::update(changes.objects.len());
    let mut update = SceneUpdate {
        generations: changes.generations.clone(),
        errors: changes.errors.clone(),
        cameras: changes.cameras.clone(),
        ..Default::default()
    };
    let mut values = BTreeMap::new();
    let mut references = BTreeMap::new();
    for &(id, offset) in &changes.objects {
        let object = offset
            .map(|offset| {
                let object = read_object_cached(
                    reader,
                    offset,
                    segment.committed_end,
                    ObjectRecords {
                        values: &mut values,
                        references: &mut references,
                        external: Some(&mut *external_cache),
                    },
                    cancelled,
                )?;
                if object.object.id != id {
                    return Err(error("history object ID mismatch"));
                }
                Ok(Arc::new(object))
            })
            .transpose()?;
        update.objects.insert(id, object);
    }
    for &(id, offset) in &changes.debug {
        let draw = offset
            .map(|offset| {
                let Record::Debug(owner, draw) =
                    read_record_with_cancel(reader, offset, segment.committed_end, cancelled)?
                else {
                    return Err(error("history draw record type mismatch"));
                };
                if owner != id {
                    return Err(error("history renderer ID mismatch"));
                }
                Ok(draw)
            })
            .transpose()?;
        update.debug.insert(id, draw);
    }
    for &(id, offset) in &changes.contacts {
        let contacts = offset
            .map(|offset| {
                let Record::Contacts(owner, contacts) =
                    read_record_with_cancel(reader, offset, segment.committed_end, cancelled)?
                else {
                    return Err(error("history contacts record type mismatch"));
                };
                if owner != id {
                    return Err(error("history contact owner mismatch"));
                }
                Ok(contacts)
            })
            .transpose()?;
        update.contacts.insert(id, contacts);
    }
    check_replay_cancelled(cancelled)?;
    Ok(update)
}

pub(super) fn read_object(
    reader: &mut HistoryReader,
    offset: u64,
    size: u64,
    values: &mut BTreeMap<u64, Value>,
    references: &mut BTreeMap<u64, Arc<BTreeMap<u64, u32>>>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<ObjectState, Error> {
    read_object_cached(
        reader,
        offset,
        size,
        ObjectRecords {
            values,
            references,
            external: None,
        },
        cancelled,
    )
}

struct ObjectRecords<'a> {
    values: &'a mut BTreeMap<u64, Value>,
    references: &'a mut BTreeMap<u64, Arc<BTreeMap<u64, u32>>>,
    external: Option<&'a mut ExternalCache>,
}

fn read_object_cached(
    reader: &mut HistoryReader,
    offset: u64,
    size: u64,
    records: ObjectRecords<'_>,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<ObjectState, Error> {
    let ObjectRecords {
        values,
        references,
        external: mut external_cache,
    } = records;
    #[cfg(all(test, feature = "scene-read-census"))]
    census::object_read();
    let mut object = match read_record_with_cancel(reader, offset, size, cancelled)? {
        Record::Object(object) => object,
        Record::KeyedObject(object) => object_keys::expand(
            object,
            reader,
            offset,
            external_cache.as_deref_mut(),
            cancelled,
        )?,
        _ => return Err(error("history object record type mismatch")),
    };
    #[cfg(all(test, feature = "scene-read-census"))]
    census::object_decoded(&object.object);
    for (key, value_offset) in object.large_properties {
        check_replay_cancelled(cancelled)?;
        if value_offset >= offset || object.object.properties.contains_key(&key) {
            return Err(error(
                "invalid shared history property offset or duplicate key",
            ));
        }
        #[cfg(all(test, feature = "scene-read-census"))]
        let census_local_hit = values.contains_key(&value_offset);
        if let std::collections::btree_map::Entry::Vacant(entry) = values.entry(value_offset) {
            let value = if let Some(cache) = external_cache.as_deref_mut()
                && value_offset & (1 << 63) != 0
            {
                cache.value(reader, value_offset, size, cancelled)?
            } else {
                let Record::Value(value) =
                    read_record_with_cancel(reader, value_offset, size, cancelled)?
                else {
                    return Err(error("history property record type mismatch"));
                };
                #[cfg(all(test, feature = "scene-read-census"))]
                census::decoded_value();
                value
            };
            entry.insert(value);
        }
        #[cfg(all(test, feature = "scene-read-census"))]
        census::value(
            &object.object,
            &key,
            value_offset,
            census_local_hit,
            &values[&value_offset],
        );
        object
            .object
            .properties
            .insert(key, values[&value_offset].clone());
    }
    let mut bindings = BTreeMap::new();
    for (key, targets) in object.references {
        check_replay_cancelled(cancelled)?;
        bindings.insert(key, Arc::new(targets));
    }
    for (key, reference_offset) in object.large_references {
        check_replay_cancelled(cancelled)?;
        if reference_offset >= offset || bindings.contains_key(&key) {
            return Err(error(
                "invalid shared history reference offset or duplicate key",
            ));
        }
        #[cfg(all(test, feature = "scene-read-census"))]
        let census_local_hit = references.contains_key(&reference_offset);
        if let std::collections::btree_map::Entry::Vacant(entry) =
            references.entry(reference_offset)
        {
            let value = if let Some(cache) = external_cache.as_deref_mut()
                && reference_offset & (1 << 63) != 0
            {
                cache.references(reader, reference_offset, size, cancelled)?
            } else {
                let Record::References(value) =
                    read_record_with_cancel(reader, reference_offset, size, cancelled)?
                else {
                    return Err(error("history reference record type mismatch"));
                };
                #[cfg(all(test, feature = "scene-read-census"))]
                census::decoded_references();
                Arc::new(value)
            };
            entry.insert(value);
        }
        #[cfg(all(test, feature = "scene-read-census"))]
        census::references(
            &object.object,
            &key,
            reference_offset,
            census_local_hit,
            references[&reference_offset].len(),
        );
        bindings.insert(key, references[&reference_offset].clone());
    }
    Ok(ObjectState {
        object: object.object,
        references: bindings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_cache_limits_frames_and_bytes_and_rejects_unavailable_rewinds_atomically() {
        let mut cache = UndoCache::default();
        let mut index = StateIndex::default();
        for frame in 0..240 {
            let changes = IndexUpdate {
                objects: vec![(1, Some(frame as u64))],
                ..Default::default()
            };
            cache.remember(frame, &changes, &index);
            changes.apply(&mut index);
            assert!(cache.frames.len() <= UNDO_FRAMES);
            assert!(cache.bytes <= UNDO_BYTES);
        }
        let before = index.clone();
        assert!(
            cache
                .rewind(&mut index, 239, 10, &mut || false)
                .unwrap()
                .is_none()
        );
        assert_eq!(index, before);
        assert!(
            cache
                .rewind(&mut index, 239, 200, &mut || false)
                .unwrap()
                .is_some()
        );
        assert_eq!(index.objects[&1], 200);
        let large = IndexUpdate {
            objects: (0..UNDO_BYTES / 32 + 1)
                .map(|id| (id as u64, None))
                .collect(),
            ..Default::default()
        };
        cache.remember(240, &large, &index);
        assert!(cache.frames.is_empty());
        assert_eq!(cache.bytes, 0);
    }
}

#[cfg(test)]
mod frame_reuse_tests;

#[cfg(test)]
mod resident_tests;

#[cfg(all(test, feature = "scene-read-census"))]
mod census;
