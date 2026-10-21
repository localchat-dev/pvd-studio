use super::*;
use resources::accounting::{ANALYSIS, PhysicalResources, SCENE};

pub(super) mod scene_records;
mod tests;

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub items: usize,
    pub segment_files: usize,
    pub record_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            items: 1_048_576,
            segment_files: 16_384,
            record_bytes: MAX_RECORD,
        }
    }
}

pub(super) fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b)
        .ok_or_else(|| error("physical accounting byte count overflow"))
}

pub(super) fn limited(value: usize, maximum: usize) -> Result<(), Error> {
    if value > maximum {
        return Err(error("physical accounting diagnostic item limit exceeded"));
    }
    Ok(())
}

#[derive(Debug, Default, serde::Serialize)]
pub(super) struct SegmentBytes {
    pub file_headers: u64,
    pub frame_record_headers: u64,
    pub frame_non_analysis_payload: u64,
    pub frame_inline_analysis_raw_buffers: u64,
    pub frame_inline_analysis_structure: u64,
    pub undo_record_headers: u64,
    pub undo_payload: u64,
    pub undos: u64,
    pub unpublished_tail_bytes: u64,
    pub checkpoint_record_headers: u64,
    pub checkpoint_payload: u64,
    pub frames: u64,
    pub checkpoints: u64,
    pub files: u64,
    pub total_bytes: u64,
    pub retired_files: u64,
    pub retired_bytes: u64,
}

impl SegmentBytes {
    fn partition_sum(&self) -> Result<u64, Error> {
        [
            self.file_headers,
            self.frame_record_headers,
            self.frame_non_analysis_payload,
            self.frame_inline_analysis_raw_buffers,
            self.frame_inline_analysis_structure,
            self.undo_record_headers,
            self.undo_payload,
            self.unpublished_tail_bytes,
            self.checkpoint_record_headers,
            self.checkpoint_payload,
        ]
        .into_iter()
        .try_fold(0, add)
    }
}

#[derive(Debug, serde::Serialize)]
pub(super) struct Report {
    pub schema: &'static str,
    pub history_stats_disk_bytes: u64,
    pub segment: SegmentBytes,
    pub resources: PhysicalResources,
    pub partition_sum: u64,
    pub logical_root_ids: usize,
    pub resource_checkpoint_records: u64,
    pub resource_checkpoint_logical_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_records: Option<scene_records::SceneRecords>,
}

struct CountWriter(u64);

impl Write for CountWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("accounting serialization length overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn encoded_bytes(value: &impl Serialize) -> Result<u64, Error> {
    let mut writer = CountWriter(0);
    postcard::to_io(value, &mut writer)?;
    Ok(writer.0)
}

fn root(roots: &mut BTreeMap<u64, u8>, id: u64, mask: u8, limits: Limits) -> Result<(), Error> {
    *roots.entry(id).or_default() |= mask;
    limited(roots.len(), limits.items)
}

fn scene_roots(
    index: &StateIndex,
    roots: &mut BTreeMap<u64, u8>,
    limits: Limits,
) -> Result<(), Error> {
    for &id in index
        .objects
        .values()
        .chain(index.debug.values())
        .chain(index.contacts.values())
    {
        root(roots, id, SCENE, limits)?;
    }
    Ok(())
}

fn segment(
    path: &Path,
    (length, committed_end, retired): (u64, u64, bool),
    report: &mut SegmentBytes,
    roots: &mut BTreeMap<u64, u8>,
    limits: Limits,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Error> {
    check_replay_cancelled(cancelled)?;
    let mut file = BufReader::new(File::open(path)?);
    if file.get_ref().metadata()?.len() != length || length < 8 {
        return Err(error(
            "accounting segment length disagrees with history statistics",
        ));
    }
    let mut magic = [0_u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(error("accounting segment magic mismatch"));
    }
    report.file_headers = add(report.file_headers, 8)?;
    report.files = add(report.files, 1)?;
    report.total_bytes = add(report.total_bytes, length)?;
    if retired {
        report.retired_files = add(report.retired_files, 1)?;
        report.retired_bytes = add(report.retired_bytes, length)?;
    }
    if !(8..=length).contains(&committed_end) {
        return Err(error("accounting committed segment boundary is invalid"));
    }
    report.unpublished_tail_bytes = add(report.unpublished_tail_bytes, length - committed_end)?;
    let mut offset = 8;
    while offset < committed_end {
        check_replay_cancelled(cancelled)?;
        let mut header = [0_u8; 36];
        file.read_exact(&mut header)?;
        let size = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        limited(size, limits.record_bytes.min(MAX_RECORD))?;
        let next = add(add(offset, 36)?, size as u64)?;
        if next > committed_end {
            return Err(error("accounting segment contains an incomplete record"));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| error("accounting record allocation failed"))?;
        bytes.resize(size, 0);
        file.read_exact(&mut bytes)?;
        if blake3::hash(&bytes).as_bytes() != &header[4..] {
            return Err(error("accounting segment record checksum mismatch"));
        }
        let record: Record = if bytes.first() == Some(&undo::RECORD_TAG) {
            Record::Undo(undo::decode(&bytes, cancelled)?)
        } else if std::env::var_os("PVD_SCENE_RECORD_ACCOUNTING").is_some() {
            capture::record::decode_for_accounting(
                &bytes,
                cancelled,
                scene_records::DECODE_ALLOWANCE,
            )?
        } else {
            postcard::from_bytes(&bytes)?
        };
        if encoded_bytes(&record)? != size as u64 {
            return Err(error(
                "accounting record is not the canonical history encoding",
            ));
        }
        match record {
            Record::Frame(frame) => {
                let analysis = encoded_bytes(&frame.analysis)?;
                let raw = frame.analysis.iter().try_fold(0, |total, input| {
                    add(
                        total,
                        match input {
                            AnalysisInput::Buffer { bytes, .. } => bytes.len() as u64,
                            _ => 0,
                        },
                    )
                })?;
                if raw > analysis || analysis > size as u64 {
                    return Err(error(
                        "accounting inline AnalysisInput lengths are inconsistent",
                    ));
                }
                report.frames = add(report.frames, 1)?;
                report.frame_record_headers = add(report.frame_record_headers, 36)?;
                report.frame_non_analysis_payload =
                    add(report.frame_non_analysis_payload, size as u64 - analysis)?;
                report.frame_inline_analysis_raw_buffers =
                    add(report.frame_inline_analysis_raw_buffers, raw)?;
                report.frame_inline_analysis_structure =
                    add(report.frame_inline_analysis_structure, analysis - raw)?;
                for (_, id) in frame
                    .update
                    .objects
                    .iter()
                    .chain(&frame.update.debug)
                    .chain(&frame.update.contacts)
                {
                    if let Some(id) = id {
                        root(roots, *id, SCENE, limits)?;
                    }
                }
            }
            Record::Undo(undo) => {
                report.undos = add(report.undos, 1)?;
                report.undo_record_headers = add(report.undo_record_headers, 36)?;
                report.undo_payload = add(report.undo_payload, size as u64)?;
                for id in undo
                    .changes
                    .objects
                    .iter()
                    .chain(&undo.changes.debug)
                    .chain(&undo.changes.contacts)
                    .filter_map(|(_, id)| *id)
                {
                    root(roots, id, SCENE, limits)?;
                }
            }
            Record::Checkpoint(index) => {
                report.checkpoints = add(report.checkpoints, 1)?;
                report.checkpoint_record_headers = add(report.checkpoint_record_headers, 36)?;
                report.checkpoint_payload = add(report.checkpoint_payload, size as u64)?;
                scene_roots(&index, roots, limits)?;
            }
            _ => {
                return Err(error(
                    "accounting found a resource record inside a segment file",
                ));
            }
        }
        offset = next;
    }
    Ok(())
}

impl HistoryWriter {
    pub(super) fn physical_accounting(
        &mut self,
        limits: Limits,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Report, Error> {
        check_replay_cancelled(cancelled)?;
        if let Some(active) = &mut self.active {
            active.file.flush()?;
        }
        let before = self.bytes();
        let mut paths = BTreeMap::<PathBuf, (u64, u64, bool)>::new();
        let mut roots = BTreeMap::new();
        let mut resource_checkpoint_records = 0;
        let mut resource_checkpoint_logical_bytes = 0;
        for view in self
            .completed
            .iter()
            .map(AsRef::as_ref)
            .chain(self.active.iter().map(|writer| &writer.view))
        {
            check_replay_cancelled(cancelled)?;
            if !self
                .resources
                .as_ref()
                .is_some_and(|resources| resources.same(&view.resources))
            {
                return Err(error(
                    "accounting segment belongs to another resource store",
                ));
            }
            if paths
                .insert(view.path.clone(), (view.bytes, view.committed_end, false))
                .is_some()
            {
                return Err(error("accounting segment path appears more than once"));
            }
            if view.analysis_base != 0 {
                root(&mut roots, view.analysis_base, ANALYSIS, limits)?;
            }
            let mut reader = None;
            for &id in view.checkpoints.values().filter(|id| **id & (1 << 63) != 0) {
                check_replay_cancelled(cancelled)?;
                limited(
                    view.resources.pin(id)?.info().0,
                    limits.record_bytes.min(MAX_RECORD),
                )?;
                if reader.is_none() {
                    reader = Some(DiskRecording::open(view)?);
                }
                let reader = reader.as_mut().unwrap();
                let bytes =
                    read_record_bytes_with_cancel(reader, id, view.committed_end, cancelled)?;
                limited(bytes.len(), limits.record_bytes.min(MAX_RECORD))?;
                let record = if std::env::var_os("PVD_SCENE_RECORD_ACCOUNTING").is_some() {
                    capture::record::decode_for_accounting(
                        &bytes,
                        cancelled,
                        scene_records::DECODE_ALLOWANCE,
                    )?
                } else {
                    reader.decode(&bytes, cancelled)?
                };
                if encoded_bytes(&record)? != bytes.len() as u64 {
                    return Err(error("accounting checkpoint is not canonically encoded"));
                }
                let Record::Checkpoint(index) = record else {
                    return Err(error("accounting checkpoint resource type mismatch"));
                };
                root(&mut roots, id, SCENE, limits)?;
                scene_roots(&index, &mut roots, limits)?;
                resource_checkpoint_records = add(resource_checkpoint_records, 1)?;
                limited(resource_checkpoint_records as usize, limits.items)?;
                resource_checkpoint_logical_bytes =
                    add(resource_checkpoint_logical_bytes, bytes.len() as u64)?;
            }
        }
        for retired in &self.retired {
            if paths
                .insert(
                    retired.path.clone(),
                    (retired.bytes, retired.committed_end, true),
                )
                .is_some()
            {
                return Err(error(
                    "accounting retired segment path overlaps a retained segment",
                ));
            }
        }
        limited(paths.len(), limits.segment_files)?;
        let mut segments = SegmentBytes::default();
        for (path, metadata) in paths {
            segment(
                &path,
                metadata,
                &mut segments,
                &mut roots,
                limits,
                cancelled,
            )?;
        }
        if let Some(previous) = &self.previous {
            scene_roots(&previous.index, &mut roots, limits)?;
        }
        if segments.partition_sum()? != segments.total_bytes {
            return Err(error("segment accounting partition does not reconcile"));
        }
        let scene_records = if std::env::var_os("PVD_SCENE_RECORD_ACCOUNTING").is_some() {
            self.resources
                .as_ref()
                .map(|resources| resources.scene_record_accounting(&roots, limits, cancelled))
                .transpose()?
        } else {
            None
        };
        let resources = match &self.resources {
            Some(resources) => resources.physical_accounting_with_kinds(
                &roots,
                scene_records.as_ref().map(|report| &report.kinds),
                limits,
                cancelled,
            )?,
            None => PhysicalResources::default(),
        };
        let sum = add(segments.partition_sum()?, resources.partition_sum()?)?;
        check_replay_cancelled(cancelled)?;
        if before != self.bytes() || sum != before || self.snapshot().stats.disk_bytes != sum {
            return Err(error(
                "physical accounting does not reconcile with HistoryStats",
            ));
        }
        Ok(Report {
            schema: "pvd-history-physical-ledger-v2",
            history_stats_disk_bytes: before,
            segment: segments,
            resources,
            partition_sum: sum,
            logical_root_ids: roots.len(),
            resource_checkpoint_records,
            resource_checkpoint_logical_bytes,
            scene_records,
        })
    }
}
