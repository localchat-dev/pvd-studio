use super::*;
use crate::live::disk_accounting::{Limits, add, limited, scene_records};

pub(in crate::live) const SCENE: u8 = 1;
pub(in crate::live) const ANALYSIS: u8 = 2;
const UNKNOWN: u8 = 4;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(in crate::live) struct Bucket {
    pub blocks: u64,
    pub payload_bytes: u64,
}

#[derive(Debug, Default, serde::Serialize)]
pub(in crate::live) struct PhysicalResources {
    pub scene_only: Bucket,
    pub analysis_only: Bucket,
    pub shared_scene_analysis: Bucket,
    pub unattributed_live: Bucket,
    pub file_header_bytes: u64,
    pub current_block_header_bytes: u64,
    pub residual_physical_bytes: u64,
    pub total_bytes: u64,
    pub files: u64,
    pub unique_current_blocks: u64,
    pub live_blobs: u64,
    pub missing_semantic_roots: u64,
    pub pending_blocks: u64,
    pub pending_logical_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_kind_partition: Option<BTreeMap<u16, Bucket>>,
}

impl PhysicalResources {
    pub fn partition_sum(&self) -> Result<u64, Error> {
        [
            self.scene_only.payload_bytes,
            self.analysis_only.payload_bytes,
            self.shared_scene_analysis.payload_bytes,
            self.unattributed_live.payload_bytes,
            self.file_header_bytes,
            self.current_block_header_bytes,
            self.residual_physical_bytes,
        ]
        .into_iter()
        .try_fold(0, add)
    }

    fn bucket(&mut self, mask: u8) -> &mut Bucket {
        if mask & UNKNOWN != 0 {
            &mut self.unattributed_live
        } else {
            match mask {
                SCENE => &mut self.scene_only,
                ANALYSIS => &mut self.analysis_only,
                3 => &mut self.shared_scene_analysis,
                _ => &mut self.unattributed_live,
            }
        }
    }
}

fn mark(
    start: u64,
    mask: u8,
    blobs: &BTreeMap<u64, Arc<Blob>>,
    labels: &mut BTreeMap<u64, u8>,
    limits: Limits,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), Error> {
    let mut pending = vec![start];
    while let Some(id) = pending.pop() {
        check_replay_cancelled(cancelled)?;
        let previous = labels.get(&id).copied().unwrap_or_default();
        if previous & mask == mask {
            continue;
        }
        let blob = blobs
            .get(&id)
            .ok_or_else(|| error("accounting dependency is absent from the live blob directory"))?;
        labels.insert(id, previous | mask);
        limited(labels.len(), limits.items)?;
        for dependency in &blob.dependencies {
            if labels.get(&dependency.id()).copied().unwrap_or_default() & mask != mask {
                limited(pending.len().saturating_add(1), limits.items)?;
                pending.push(dependency.id());
            }
        }
    }
    Ok(())
}

impl Resources {
    pub(in crate::live) fn physical_accounting(
        &self,
        roots: &BTreeMap<u64, u8>,
        limits: Limits,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<PhysicalResources, Error> {
        self.physical_accounting_with_kinds(roots, None, limits, cancelled)
    }

    pub(in crate::live) fn scene_record_accounting(
        &self,
        roots: &BTreeMap<u64, u8>,
        limits: Limits,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<scene_records::SceneRecords, Error> {
        check_replay_cancelled(cancelled)?;
        if self.shared.capture.is_some() {
            return Err(error("scene accounting requires a writable history store"));
        }
        let limits = Limits {
            items: limits.items.min(scene_records::MAX_ITEMS),
            ..limits
        };
        let state = self.shared.state.lock().map_err(|_| lock_error())?;
        let blobs = {
            let directory = self.shared.blobs.lock().map_err(|_| lock_error())?;
            limited(directory.len(), limits.items)?;
            let mut blobs = BTreeMap::new();
            for (&id, weak) in &*directory {
                check_replay_cancelled(cancelled)?;
                if let Some(blob) = weak.upgrade() {
                    blobs.insert(id, blob);
                }
            }
            blobs
        };
        let mut labels = BTreeMap::new();
        limited(roots.len(), limits.items)?;
        for (&id, &mask) in roots {
            check_replay_cancelled(cancelled)?;
            if mask == 0 || mask & !(SCENE | ANALYSIS) != 0 {
                return Err(error("invalid scene accounting root class"));
            }
            if mask & SCENE != 0
                && blobs
                    .get(&id)
                    .is_some_and(|blob| blob.ready.load(Ordering::Acquire))
            {
                mark(id, SCENE, &blobs, &mut labels, limits, cancelled)?;
            }
        }
        drop(state);
        // Blob owners keep the validated directory stable while record I/O runs outside the store lock.
        let reader = self.reader();
        let mut report = scene_records::SceneRecords::new(limits);
        for (&id, blob) in &blobs {
            check_replay_cancelled(cancelled)?;
            if labels.get(&id).copied().unwrap_or_default() & SCENE == 0 {
                continue;
            }
            limited(blob.len, report.per_record_wire_limit)?;
            let bytes = reader.read(id, cancelled)?;
            report.observe(id, &bytes, cancelled)?;
        }
        report.verify()?;
        Ok(report)
    }

    pub(in crate::live) fn physical_accounting_with_kinds(
        &self,
        roots: &BTreeMap<u64, u8>,
        kinds: Option<&BTreeMap<u64, u16>>,
        limits: Limits,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<PhysicalResources, Error> {
        check_replay_cancelled(cancelled)?;
        if self.shared.capture.is_some() {
            return Err(error(
                "physical accounting requires a writable history store",
            ));
        }
        let state = self.shared.state.lock().map_err(|_| lock_error())?;
        limited(state.blocks.len(), limits.items)?;
        limited(state.chunks.len(), limits.items)?;
        let blobs = {
            let directory = self.shared.blobs.lock().map_err(|_| lock_error())?;
            limited(directory.len(), limits.items)?;
            let mut blobs = BTreeMap::new();
            for (&id, weak) in &*directory {
                check_replay_cancelled(cancelled)?;
                if let Some(blob) = weak.upgrade() {
                    blobs.insert(id, blob);
                }
            }
            blobs
        };
        if let Some(kinds) = kinds {
            limited(kinds.len(), limits.items)?;
            for (&id, &kind) in kinds {
                if kind == 0
                    || !kind.is_power_of_two()
                    || kind & !scene_records::SCENE_KINDS != 0
                    || !blobs.contains_key(&id)
                {
                    return Err(error("scene accounting kind directory is invalid"));
                }
            }
        }
        let mut report = PhysicalResources {
            live_blobs: blobs.len() as u64,
            record_kind_partition: kinds.map(|_| BTreeMap::new()),
            ..Default::default()
        };
        let mut labels = BTreeMap::new();
        for (&id, &mask) in roots {
            check_replay_cancelled(cancelled)?;
            if mask == 0 || mask & !(SCENE | ANALYSIS) != 0 {
                return Err(error("invalid accounting root class"));
            }
            if blobs
                .get(&id)
                .is_some_and(|blob| blob.ready.load(Ordering::Acquire))
            {
                mark(id, mask, &blobs, &mut labels, limits, cancelled)?;
            } else {
                report.missing_semantic_roots = add(report.missing_semantic_roots, 1)?;
            }
        }
        for &id in blobs.keys() {
            if !labels.contains_key(&id) {
                mark(id, UNKNOWN, &blobs, &mut labels, limits, cancelled)?;
            }
        }
        let mut block_masks = BTreeMap::<u64, (u8, u16)>::new();
        for (&id, blob) in &blobs {
            check_replay_cancelled(cancelled)?;
            let mask = labels.get(&id).copied().unwrap_or(UNKNOWN);
            let mut kind = kinds
                .and_then(|kinds| kinds.get(&id))
                .copied()
                .unwrap_or_default();
            if mask & ANALYSIS != 0 {
                kind |= scene_records::ANALYSIS_KIND;
            }
            if mask & UNKNOWN != 0 || (mask & SCENE != 0 && kind & scene_records::SCENE_KINDS == 0)
            {
                kind |= scene_records::UNKNOWN_KIND;
            }
            for block in &blob.blocks {
                let entry = block_masks.entry(block.id).or_default();
                entry.0 |= mask;
                entry.1 |= kind;
                limited(block_masks.len(), limits.items)?;
            }
        }
        let mut placements = BTreeMap::<u64, Vec<(u64, u64, u8, u16)>>::new();
        for weak in state.blocks.values() {
            check_replay_cancelled(cancelled)?;
            let Some(block) = weak.upgrade() else {
                continue;
            };
            let location = block.location.lock().map_err(|_| lock_error())?;
            let placement = match &*location {
                BlockLocation::Pending(_) => {
                    report.pending_blocks = add(report.pending_blocks, 1)?;
                    report.pending_logical_bytes =
                        add(report.pending_logical_bytes, block.len as u64)?;
                    continue;
                }
                BlockLocation::Stored(placement) => placement.clone(),
            };
            drop(location);
            let chunk = state
                .chunks
                .get(&placement.chunk.id)
                .ok_or_else(|| error("accounting block references an absent physical file"))?;
            let end = add(add(placement.offset, BLOCK_HEADER)?, block.len as u64)?;
            if placement.offset < FILE_HEADER || end > chunk.bytes.load(Ordering::Relaxed) {
                return Err(error(
                    "accounting block placement exceeds its physical file",
                ));
            }
            let (mask, kind) = block_masks
                .get(&block.id)
                .copied()
                .unwrap_or((UNKNOWN, scene_records::UNKNOWN_KIND));
            let entries = placements.entry(chunk.id).or_default();
            limited(entries.len().saturating_add(1), limits.items)?;
            entries.push((placement.offset, end, mask, kind));
        }
        for chunk in state.chunks.values() {
            check_replay_cancelled(cancelled)?;
            let bytes = chunk.bytes.load(Ordering::Relaxed);
            if std::fs::metadata(&chunk.path)?.len() != bytes {
                return Err(error(
                    "accounting physical file length disagrees with history statistics",
                ));
            }
            report.files = add(report.files, 1)?;
            report.total_bytes = add(report.total_bytes, bytes)?;
            let header = bytes.min(FILE_HEADER);
            report.file_header_bytes = add(report.file_header_bytes, header)?;
            let mut end = header;
            if let Some(entries) = placements.get_mut(&chunk.id) {
                entries.sort_unstable_by_key(|entry| entry.0);
                for &(offset, next, mask, kind) in entries.iter() {
                    check_replay_cancelled(cancelled)?;
                    if offset < end {
                        return Err(error(
                            "accounting current physical block placements overlap",
                        ));
                    }
                    report.residual_physical_bytes =
                        add(report.residual_physical_bytes, offset - end)?;
                    report.current_block_header_bytes =
                        add(report.current_block_header_bytes, BLOCK_HEADER)?;
                    report.unique_current_blocks = add(report.unique_current_blocks, 1)?;
                    let bucket = report.bucket(mask);
                    bucket.blocks = add(bucket.blocks, 1)?;
                    bucket.payload_bytes = add(bucket.payload_bytes, next - offset - BLOCK_HEADER)?;
                    if let Some(partition) = &mut report.record_kind_partition {
                        let bucket = partition.entry(kind).or_default();
                        bucket.blocks = add(bucket.blocks, 1)?;
                        bucket.payload_bytes =
                            add(bucket.payload_bytes, next - offset - BLOCK_HEADER)?;
                    }
                    end = next;
                }
            }
            report.residual_physical_bytes = add(report.residual_physical_bytes, bytes - end)?;
        }
        if report.partition_sum()? != report.total_bytes {
            return Err(error("resource accounting partition does not reconcile"));
        }
        if let Some(partition) = &report.record_kind_partition {
            let (blocks, bytes) =
                partition
                    .values()
                    .try_fold((0, 0), |(blocks, bytes), bucket| {
                        Ok::<_, Error>((
                            add(blocks, bucket.blocks)?,
                            add(bytes, bucket.payload_bytes)?,
                        ))
                    })?;
            let physical_payload = [
                report.scene_only.payload_bytes,
                report.analysis_only.payload_bytes,
                report.shared_scene_analysis.payload_bytes,
                report.unattributed_live.payload_bytes,
            ]
            .into_iter()
            .try_fold(0, add)?;
            if blocks != report.unique_current_blocks || bytes != physical_payload {
                return Err(error("record-kind physical partition does not reconcile"));
            }
        }
        Ok(report)
    }
}
