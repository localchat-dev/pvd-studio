use super::*;
use std::fs;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn store() -> Resources {
    store_with_budget(8 << 20)
}

fn store_with_budget(budget: u64) -> Resources {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/raw-write-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
    Resources::new(CacheDirectory::new(&root).unwrap(), budget)
}

fn block(contents: &[u8]) -> Block {
    Block {
        id: 1,
        len: contents.len(),
        hash: *blake3::hash(contents).as_bytes(),
        location: Mutex::new(BlockLocation::Pending(Arc::from(contents))),
    }
}

fn save(resources: &Resources, contents: &[u8]) -> ResourceHandle {
    let mut plan = ResourcePlan::default();
    let handle = resources.plan(contents, &[], &mut plan).unwrap();
    resources.commit(&plan).unwrap();
    handle
}

fn actual_bytes(resources: &Resources) -> u64 {
    resources
        .shared
        .state
        .lock()
        .unwrap()
        .chunks
        .values()
        .map(|chunk| fs::metadata(&chunk.path).unwrap().len())
        .sum()
}

fn chunk_id(handle: &ResourceHandle) -> u64 {
    assert_eq!(handle.0.blocks.len(), 1);
    match &*handle.0.blocks[0].location.lock().unwrap() {
        BlockLocation::Stored(placement) => placement.chunk.id,
        _ => panic!("uncommitted block"),
    }
}

#[test]
fn raw_record_bytes_and_legacy_reads_match_at_all_boundaries() {
    let resources = store();
    let mut state = resources.shared.state.lock().unwrap();
    prepare_write_scratch(&mut state, &resources.shared).unwrap();
    let pointer = state.write_scratch.as_ptr();
    assert_eq!(state.write_scratch.capacity(), WRITE_BUFFER_BYTES);
    let mut active = new_chunk(&resources.shared, &mut state).unwrap();
    let mut expected = MAGIC.to_vec();
    for len in [1, 35, 36, MIN_BLOCK, MAX_BLOCK] {
        let contents = (0..len).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        let block = block(&contents);
        let placement =
            write_block(&mut active, &block, &contents, &mut state.write_scratch).unwrap();
        expected.extend_from_slice(&(len as u32).to_le_bytes());
        expected.extend_from_slice(&block.hash);
        expected.extend_from_slice(&contents);
        assert!(state.write_scratch.is_empty());
        assert_eq!(state.write_scratch.as_ptr(), pointer);
        assert_eq!(
            active.chunk.bytes.load(Ordering::Relaxed),
            expected.len() as u64
        );
        assert_eq!(
            resources
                .shared
                .read_placement(&block, &placement, &mut || false)
                .unwrap(),
            contents
        );
    }
    active.file.flush().unwrap();
    assert_eq!(fs::read(&active.chunk.path).unwrap(), expected);
}

#[test]
fn invalid_block_or_unprepared_scratch_cannot_write_a_prefix() {
    let resources = store();
    let mut state = resources.shared.state.lock().unwrap();
    prepare_write_scratch(&mut state, &resources.shared).unwrap();
    let mut active = new_chunk(&resources.shared, &mut state).unwrap();
    for contents in [vec![], vec![5; MAX_BLOCK + 1]] {
        assert!(
            write_block(
                &mut active,
                &block(&contents),
                &contents,
                &mut state.write_scratch
            )
            .is_err()
        );
    }
    let contents = [6; 100];
    assert!(
        write_block(
            &mut active,
            &block(&contents),
            &contents[..99],
            &mut state.write_scratch
        )
        .is_err()
    );
    assert!(write_block(&mut active, &block(&contents), &contents, &mut Vec::new()).is_err());
    assert_eq!(active.chunk.bytes.load(Ordering::Relaxed), FILE_HEADER);
    assert_eq!(fs::read(&active.chunk.path).unwrap(), MAGIC);
}

#[test]
fn scratch_is_lazy_bounded_and_stays_accounted_after_pruning() {
    let resources = store();
    let mut state = resources.shared.state.lock().unwrap();
    assert_eq!(state.write_scratch.capacity(), 0);
    let before = metadata_bytes(&state);
    prepare_write_scratch(&mut state, &resources.shared).unwrap();
    let pointer = state.write_scratch.as_ptr();
    assert_eq!(metadata_bytes(&state), before + WRITE_BUFFER_BYTES);
    for _ in 0..10 {
        prepare_write_scratch(&mut state, &resources.shared).unwrap();
        prune(&mut state, &resources.shared).unwrap();
        assert_eq!(state.write_scratch.as_ptr(), pointer);
        assert_eq!(state.write_scratch.capacity(), WRITE_BUFFER_BYTES);
        assert_eq!(metadata_bytes(&state), before + WRITE_BUFFER_BYTES);
    }
}

#[test]
fn insufficient_scratch_budget_rejects_commit_before_creating_files() {
    let resources = store();
    let mut plan = ResourcePlan::default();
    let handle = resources.plan(b"bounded writer", &[], &mut plan).unwrap();
    {
        let mut state = resources.shared.state.lock().unwrap();
        prune(&mut state, &resources.shared).unwrap();
        state.metadata_limit = metadata_bytes(&state) + WRITE_BUFFER_BYTES - 1;
    }
    assert!(
        resources
            .commit(&plan)
            .unwrap_err()
            .to_string()
            .contains("metadata")
    );
    assert_eq!(resources.physical_bytes(), 0);
    assert!(!handle.0.ready.load(Ordering::Acquire));
    {
        let mut state = resources.shared.state.lock().unwrap();
        assert!(state.chunks.is_empty());
        assert_eq!(state.write_scratch.capacity(), 0);
        assert!(matches!(
            *handle.0.blocks[0].location.lock().unwrap(),
            BlockLocation::Pending(_)
        ));
        state.metadata_limit = MAX_METADATA_BYTES;
    }
    resources.commit(&plan).unwrap();
    assert_eq!(
        resources.reader().read(handle.id(), &mut || false).unwrap(),
        b"bounded writer"
    );
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
}

#[test]
fn compaction_reuses_scratch_preserves_old_lease_and_releases_files() {
    let resources = store();
    let first = save(&resources, &[11; 5000]);
    let second = save(&resources, &[22; 7000]);
    let pointer = resources
        .shared
        .state
        .lock()
        .unwrap()
        .write_scratch
        .as_ptr();
    let block = second.0.blocks[0].clone();
    let old = match &*block.location.lock().unwrap() {
        BlockLocation::Stored(placement) => placement.clone(),
        _ => panic!("pending block"),
    };
    drop(first);
    resources.collect().unwrap();
    assert!(resources.compact_one().unwrap());
    assert_eq!(
        resources
            .shared
            .state
            .lock()
            .unwrap()
            .write_scratch
            .as_ptr(),
        pointer
    );
    assert_eq!(
        resources
            .shared
            .read_placement(&block, &old, &mut || false)
            .unwrap(),
        vec![22; 7000]
    );
    assert_eq!(
        resources.reader().read(second.id(), &mut || false).unwrap(),
        vec![22; 7000]
    );
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    drop((second, block, old));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    let state = resources.shared.state.lock().unwrap();
    assert_eq!(
        state.metadata,
        recalculate_metadata(&state, &resources.shared.blobs.lock().unwrap())
    );
    assert_eq!(state.write_scratch.capacity(), WRITE_BUFFER_BYTES);
}

#[test]
fn compaction_selects_the_most_reclaimable_bytes_and_charges_pinned_old_placements() {
    let resources = store_with_budget(12 << 10);
    let path = resources.shared.owner.as_ref().unwrap().path.clone();
    let older_dead = save(&resources, &[1; 50]);
    let older_live = save(&resources, &[2; 200]);
    let older_id = chunk_id(&older_live);
    assert_eq!(chunk_id(&older_dead), older_id);
    resources.collect().unwrap();
    let newer_dead = save(&resources, &[3; 200]);
    let newer_live = save(&resources, &[4; 50]);
    let newer_id = chunk_id(&newer_live);
    assert_eq!(chunk_id(&newer_dead), newer_id);
    assert!(older_id < newer_id);
    let copied_block = newer_live.0.blocks[0].clone();
    let inflight = match &*copied_block.location.lock().unwrap() {
        BlockLocation::Stored(placement) => placement.clone(),
        _ => panic!("uncommitted block"),
    };
    let old_path = inflight.chunk.path.clone();
    let before = resources.physical_bytes();
    assert_eq!(before, 2 * (FILE_HEADER + 2 * BLOCK_HEADER + 250));
    drop((older_dead, newer_dead));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), before);
    assert!(resources.compact_one().unwrap());
    assert_eq!(chunk_id(&older_live), older_id);
    assert!(chunk_id(&newer_live) > newer_id);
    let replacement = FILE_HEADER + BLOCK_HEADER + 50;
    assert_eq!(resources.physical_bytes(), before + replacement);
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    assert!(old_path.exists());
    assert_eq!(
        resources
            .reader()
            .read(older_live.id(), &mut || false)
            .unwrap(),
        [2; 200]
    );
    assert_eq!(
        resources
            .reader()
            .read(newer_live.id(), &mut || false)
            .unwrap(),
        [4; 50]
    );
    assert_eq!(
        resources
            .shared
            .read_placement(&copied_block, &inflight, &mut || false)
            .unwrap(),
        [4; 50]
    );
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), before + replacement);
    assert!(old_path.exists());
    drop(inflight);
    resources.collect().unwrap();
    assert!(!old_path.exists());
    assert_eq!(resources.physical_bytes(), before - (BLOCK_HEADER + 200));
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    assert!(resources.compact_one().unwrap());
    assert_ne!(chunk_id(&older_live), older_id);
    assert_eq!(
        resources.physical_bytes(),
        2 * FILE_HEADER + 2 * BLOCK_HEADER + 250
    );
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    assert!(!resources.compact_one().unwrap());
    assert_eq!(
        resources
            .reader()
            .read(older_live.id(), &mut || false)
            .unwrap(),
        [2; 200]
    );
    assert_eq!(
        resources
            .reader()
            .read(newer_live.id(), &mut || false)
            .unwrap(),
        [4; 50]
    );
    drop((older_live, newer_live, copied_block));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert_eq!(actual_bytes(&resources), 0);
    drop(resources);
    assert!(!path.exists());
}

#[test]
fn equal_compaction_gains_choose_the_lower_chunk_id() {
    let resources = store_with_budget(12 << 10);
    let path = resources.shared.owner.as_ref().unwrap().path.clone();
    let older_dead = save(&resources, &[11; 50]);
    let older_live = save(&resources, &[12; 100]);
    let older_id = chunk_id(&older_live);
    assert_eq!(chunk_id(&older_dead), older_id);
    resources.collect().unwrap();
    let newer_dead = save(&resources, &[13; 50]);
    let newer_live = save(&resources, &[14; 200]);
    let newer_id = chunk_id(&newer_live);
    assert_eq!(chunk_id(&newer_dead), newer_id);
    assert!(older_id < newer_id);
    drop((older_dead, newer_dead));
    resources.collect().unwrap();
    let before = resources.physical_bytes();
    assert!(resources.compact_one().unwrap());
    assert!(chunk_id(&older_live) > newer_id);
    assert_eq!(chunk_id(&newer_live), newer_id);
    assert_eq!(resources.physical_bytes(), before - (BLOCK_HEADER + 50));
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    assert_eq!(
        resources
            .reader()
            .read(older_live.id(), &mut || false)
            .unwrap(),
        [12; 100]
    );
    assert_eq!(
        resources
            .reader()
            .read(newer_live.id(), &mut || false)
            .unwrap(),
        [14; 200]
    );
    drop((older_live, newer_live));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert_eq!(actual_bytes(&resources), 0);
    drop(resources);
    assert!(!path.exists());
}

#[cfg(feature = "resource-test-write-faults")]
#[derive(Clone, Copy)]
enum Fault {
    Write { skip: usize, prefix: usize },
    Flush,
}

#[cfg(feature = "resource-test-write-faults")]
thread_local! {
    static FAULT: std::cell::RefCell<Option<Fault>> = const { std::cell::RefCell::new(None) };
}

#[cfg(feature = "resource-test-write-faults")]
struct FaultGuard;

#[cfg(feature = "resource-test-write-faults")]
impl FaultGuard {
    fn install(fault: Fault) -> Self {
        FAULT.with(|slot| assert!(slot.replace(Some(fault)).is_none()));
        Self
    }
}

#[cfg(feature = "resource-test-write-faults")]
impl Drop for FaultGuard {
    fn drop(&mut self) {
        FAULT.with(|slot| {
            slot.take();
        });
    }
}

#[cfg(feature = "resource-test-write-faults")]
pub(super) fn write_all(file: &mut File, bytes: &[u8]) -> std::io::Result<()> {
    let prefix = FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        match slot.as_mut() {
            Some(Fault::Write { skip, .. }) if *skip != 0 => {
                *skip -= 1;
                None
            }
            Some(Fault::Write { prefix, .. }) => {
                let prefix = *prefix;
                slot.take();
                Some(prefix)
            }
            _ => None,
        }
    });
    if let Some(prefix) = prefix {
        file.write_all(&bytes[..prefix.min(bytes.len())])?;
        return Err(std::io::Error::other("injected partial write"));
    }
    file.write_all(bytes)
}

#[cfg(feature = "resource-test-write-faults")]
pub(super) fn check_flush_failure() -> std::io::Result<()> {
    let failed = FAULT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if matches!(*slot, Some(Fault::Flush)) {
            slot.take();
            true
        } else {
            false
        }
    });
    if failed {
        Err(std::io::Error::other("injected flush failure"))
    } else {
        Ok(())
    }
}

#[cfg(feature = "resource-test-write-faults")]
#[test]
fn actual_partial_write_does_not_publish_any_new_plan_location() {
    let resources = store();
    let old = save(&resources, b"old committed content");
    let before = resources.physical_bytes();
    let mut plan = ResourcePlan::default();
    let first = resources.plan(&[1; 100], &[], &mut plan).unwrap();
    let second = resources.plan(&[2; 2000], &[], &mut plan).unwrap();
    let _fault = FaultGuard::install(Fault::Write {
        skip: 1,
        prefix: 42,
    });
    assert!(
        resources
            .commit(&plan)
            .unwrap_err()
            .to_string()
            .contains("partial write")
    );
    assert_eq!(resources.physical_bytes(), before + BLOCK_HEADER + 100 + 42);
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    for handle in [&first, &second] {
        assert!(!handle.0.ready.load(Ordering::Acquire));
        assert!(matches!(
            *handle.0.blocks[0].location.lock().unwrap(),
            BlockLocation::Pending(_)
        ));
        assert!(resources.reader().read(handle.id(), &mut || false).is_err());
    }
    assert!(
        resources
            .shared
            .state
            .lock()
            .unwrap()
            .write_scratch
            .is_empty()
    );
    assert_eq!(
        resources.reader().read(old.id(), &mut || false).unwrap(),
        b"old committed content"
    );
    resources.commit(&plan).unwrap();
    assert_eq!(
        resources.reader().read(first.id(), &mut || false).unwrap(),
        vec![1; 100]
    );
    assert_eq!(
        resources.reader().read(second.id(), &mut || false).unwrap(),
        vec![2; 2000]
    );
    drop((old, first, second, plan));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
}

#[cfg(feature = "resource-test-write-faults")]
#[test]
fn final_flush_failure_preserves_pending_retries_and_counts_actual_bytes() {
    let resources = store();
    let mut plan = ResourcePlan::default();
    let handle = resources.plan(&[9; 150], &[], &mut plan).unwrap();
    let _fault = FaultGuard::install(Fault::Flush);
    assert!(
        resources
            .commit(&plan)
            .unwrap_err()
            .to_string()
            .contains("flush failure")
    );
    assert_eq!(resources.physical_bytes(), FILE_HEADER + BLOCK_HEADER + 150);
    assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
    assert!(!handle.0.ready.load(Ordering::Acquire));
    assert!(matches!(
        *handle.0.blocks[0].location.lock().unwrap(),
        BlockLocation::Pending(_)
    ));
    resources.commit(&plan).unwrap();
    assert_eq!(
        resources.reader().read(handle.id(), &mut || false).unwrap(),
        vec![9; 150]
    );
    drop((handle, plan));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
}

#[cfg(feature = "resource-test-write-faults")]
#[test]
fn compaction_write_and_flush_failures_leave_old_locations_readable() {
    for fault in [
        Fault::Write {
            skip: 1,
            prefix: 41,
        },
        Fault::Flush,
    ] {
        let resources = store();
        let first = save(&resources, &[13; 5000]);
        let second = save(&resources, &[23; 7000]);
        let block = second.0.blocks[0].clone();
        let old = match &*block.location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => panic!("pending block"),
        };
        drop(first);
        resources.collect().unwrap();
        let before = resources.physical_bytes();
        let _fault = FaultGuard::install(fault);
        assert!(resources.compact_one().is_err());
        let expected = match fault {
            Fault::Write { prefix, .. } => FILE_HEADER + prefix as u64,
            Fault::Flush => FILE_HEADER + BLOCK_HEADER + block.len as u64,
        };
        assert_eq!(resources.physical_bytes(), before + expected);
        assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
        assert!(
            matches!(&*block.location.lock().unwrap(), BlockLocation::Stored(placement) if Arc::ptr_eq(&placement.chunk, &old.chunk) && placement.offset == old.offset)
        );
        assert_eq!(
            resources.reader().read(second.id(), &mut || false).unwrap(),
            vec![23; 7000]
        );
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), before);
        assert!(resources.compact_one().unwrap());
        assert_eq!(
            resources
                .shared
                .read_placement(&block, &old, &mut || false)
                .unwrap(),
            vec![23; 7000]
        );
        assert_eq!(
            resources.reader().read(second.id(), &mut || false).unwrap(),
            vec![23; 7000]
        );
        drop((second, block, old));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }
}

mod integration_oracle {
    use super::super::*;
    use crate::analysis::view::{MemorySource, MemoryViewCursor};
    use crate::live::{
        AnalysisCache, DiskRecording, HistoryConfig, Record, ReplayCursor, decode_record,
        read_record_bytes_with_cancel,
    };
    use serde::Serialize;
    use std::{path::Path, time::Instant};

    struct DigestWriter(blake3::Hasher);

    impl Write for DigestWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn digest(value: &impl Serialize) -> String {
        postcard::to_io(value, DigestWriter(blake3::Hasher::new()))
            .unwrap()
            .0
            .finalize()
            .to_hex()
            .to_string()
    }

    fn load(input: &Path, root: &Path) -> (crate::Recording, usize, f64) {
        let started = Instant::now();
        let mut live = crate::LiveRecording::with_legacy_history(HistoryConfig {
            directory: root.into(),
            min_free_disk_bytes: 0,
            ..Default::default()
        })
        .unwrap();
        let mut file = File::open(input).unwrap();
        let mut buffer = [0; 65536];
        let mut fed = 0;
        while live.frame_count() < 1201 {
            let count = file.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            fed += count;
            assert!(fed <= 128 << 20);
            live.feed(&buffer[..count]).unwrap();
        }
        let recording = live.snapshot().unwrap();
        drop(live);
        let elapsed = started.elapsed().as_secs_f64();
        (recording, fed, elapsed)
    }

    fn disk_summary(
        recording: &crate::Recording,
    ) -> (serde_json::Value, Vec<(usize, usize, String)>) {
        let disk = recording.disk.as_ref().unwrap();
        let segments: Vec<_> = disk
            .segments
            .iter()
            .map(Arc::as_ref)
            .chain(&disk.active)
            .collect();
        assert_eq!(segments.len(), 3);
        let resources = &segments[0].resources;
        let mut baselines = Vec::new();
        let mut rows = Vec::new();
        let mut frame_bytes = 0;
        for segment in segments {
            let first = segment.frames.first().unwrap().info.index;
            let last = segment.frames.last().unwrap().info.index;
            let mut reader = DiskRecording::open(segment).unwrap();
            let started = Instant::now();
            let bytes = read_record_bytes_with_cancel(
                &mut reader,
                segment.analysis_base,
                segment.bytes,
                &mut || false,
            )
            .unwrap();
            let read_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = Instant::now();
            let Record::Analysis(analysis) = decode_record(&bytes, &mut || false).unwrap() else {
                panic!("analysis baseline")
            };
            let decode_ms = started.elapsed().as_secs_f64() * 1000.0;
            let hash = blake3::hash(&bytes).to_hex().to_string();
            baselines.push((first, last, hash.clone()));
            rows.push(serde_json::json!({"first":first,"last":last,"record_bytes":bytes.len(),"record_hash":hash,
                "analysis_hash":digest(&analysis),"resource_read_and_decode_ms":read_ms,"record_serde_ms":decode_ms}));
            frame_bytes += segment.bytes;
        }
        let mut counts = [0usize; 2];
        let mut unique_raw_bytes = 0;
        let mut original_blocks = Vec::new();
        let blocks: Vec<_> = resources
            .shared
            .state
            .lock()
            .unwrap()
            .blocks
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        for block in blocks {
            let location = block.location.lock().unwrap();
            let BlockLocation::Stored(placement) = &*location else {
                panic!("pending block")
            };
            let placement = placement.clone();
            drop(location);
            let bytes = resources
                .shared
                .read_placement(&block, &placement, &mut || false)
                .unwrap();
            assert_eq!(bytes.len(), block.len);
            assert_eq!(blake3::hash(&bytes).as_bytes(), &block.hash);
            counts[0] += 1;
            unique_raw_bytes += block.len;
            original_blocks.push((block.len, block.hash));
        }
        original_blocks.sort_unstable();
        let resource_bytes = resources.physical_bytes();
        assert_eq!(frame_bytes + resource_bytes, disk.stats.disk_bytes);
        let actual_bytes: u64 = std::fs::read_dir(&resources.shared.owner.as_ref().unwrap().path)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "pvdhistory")
            })
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert_eq!(actual_bytes, disk.stats.disk_bytes);
        (
            serde_json::json!({"frames":recording.frame_count(),"segments":3,"disk_bytes":actual_bytes,
            "frame_file_bytes":frame_bytes,"resource_bytes":resource_bytes,"resource_metadata_bytes":resources.metadata_bytes(),
            "unique_blocks":original_blocks.len(),"unique_raw_bytes":unique_raw_bytes,"block_identity_hash":digest(&original_blocks),
            "raw_blocks":counts[0],"compressed_blocks":counts[1],"baselines":rows}),
            baselines,
        )
    }

    fn trace(
        recording: &mut crate::Recording,
        ranges: &[(usize, usize, String)],
        diagnostic: bool,
    ) -> Vec<serde_json::Value> {
        let mut rows = Vec::new();
        let targets: Vec<_> = ranges
            .iter()
            .flat_map(|(first, last, _)| [*first, (*first + 1).min(*last), *last])
            .collect();
        let mut sequence = Vec::new();
        for &(first, last, _) in ranges {
            let cold = (first + 3).min(last);
            sequence.push(("cold", cold));
            sequence.push(("warm_forward", (cold + 1).min(last)));
            sequence.push(("warm_forward", (cold + 2).min(last)));
            sequence.push(("warm_reverse", (cold + 1).min(last)));
            sequence.push(("warm_reverse", cold));
        }
        sequence.extend(targets.iter().map(|&frame| ("forward", frame)));
        sequence.extend(targets.iter().rev().map(|&frame| ("reverse", frame)));
        let mut cursor = ReplayCursor::default();
        let mut scene = crate::SceneState::default();
        for (phase, frame) in sequence {
            if phase == "cold" {
                recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
                cursor = ReplayCursor::default();
                scene = crate::SceneState::default();
            }
            let (replay, metrics, elapsed_ms) = if diagnostic {
                let (replay, metrics) = cursor.seek_profiled(recording, frame, || false).unwrap();
                (replay, Some(metrics), None)
            } else {
                let started = Instant::now();
                let replay = cursor.seek(recording, frame, || false).unwrap();
                let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
                (replay, None, Some(elapsed_ms))
            };
            replay.update.apply(&mut scene);
            if let Some(metrics) = &metrics {
                if phase == "cold" {
                    assert_eq!(metrics.baseline_loads, 1);
                }
                if phase.starts_with("warm_") {
                    assert_eq!(metrics.baseline_loads, 0);
                }
            }
            let (cache_entries, cache_bytes) =
                recording.disk.as_ref().unwrap().analysis_cache.stats();
            assert!(cache_bytes <= 192 << 20);
            let memory = replay.analysis.memory_snapshot();
            let mut memory_state = MemoryViewCursor::default();
            memory.seek(&mut memory_state, frame).unwrap();
            rows.push(serde_json::json!({"phase":phase,"frame":frame,
                    "scene_hash":digest(&(&scene.objects,&scene.cameras,&scene.errors,&scene.debug_draw,&scene.contacts,&scene.reference_lifetimes,&scene.generations)),
                    "analysis_hash":digest(&replay.analysis),"memory_hash":digest(&memory_state),
                    "churn_hash":digest(&memory.churn(frame)),
                    "total_seek_ms":elapsed_ms,"cache_entries":cache_entries,"cache_bytes":cache_bytes,
                    "diagnostic_profile":metrics.map(|metrics| serde_json::json!({
                        "baseline_loads":metrics.baseline_loads,"baseline_bytes":metrics.baseline_bytes,
                        "total_ms":metrics.total.as_secs_f64()*1000.0,
                        "baseline_io_ms":metrics.baseline_io.as_secs_f64()*1000.0,
                        "baseline_serde_ms":metrics.baseline_decode.as_secs_f64()*1000.0,
                        "baseline_pages_ms":metrics.baseline_pages.as_secs_f64()*1000.0,
                "resident_domain_bytes": metrics.resident_domain_bytes,
                "analysis_root_hits": metrics.analysis_root_hits,
                        "baseline_sharing_ms":metrics.baseline_sharing.as_secs_f64()*1000.0
                    }))}));
        }
        rows
    }

    #[test]
    #[ignore = "requires a real SDK capture; verifies three segments and all retained resource bytes"]
    fn three_real_segments_current_integration() {
        let input =
            PathBuf::from(std::env::var_os("PVD_RESOURCE_CAPTURE").expect("PVD_RESOURCE_CAPTURE"));
        let output =
            PathBuf::from(std::env::var_os("PVD_RESOURCE_OUTPUT").expect("PVD_RESOURCE_OUTPUT"));
        let label = "legacy-record-coalesced";
        std::fs::create_dir_all(&output).unwrap();
        let history = output.join("history");
        let (mut recording, fed, seconds) = load(&input, &history);
        let (disk, ranges) = disk_summary(&recording);
        let rows = trace(&mut recording, &ranges, false);
        let diagnostic_rows = trace(&mut recording, &ranges, true);
        let semantic_rows = |rows: &[serde_json::Value]| {
            rows.iter()
                .map(|row| {
                    serde_json::json!([
                        row["phase"],
                        row["frame"],
                        row["scene_hash"],
                        row["analysis_hash"],
                        row["memory_hash"],
                        row["churn_hash"]
                    ])
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(semantic_rows(&rows), semantic_rows(&diagnostic_rows));
        assert_eq!(rows.len(), 33);
        let result = serde_json::json!({"label":label,"input_bytes_fed":fed,"load_seconds":seconds,"disk":disk,"trace":rows,"diagnostic_trace":diagnostic_rows});
        drop(recording);
        assert!(std::fs::read_dir(&history).unwrap().next().is_none());
        std::fs::write(output.join("summary.json"),serde_json::to_vec_pretty(&serde_json::json!({
            "scope":"Legacy Record::Analysis layout with coalesced raw writes; functional three-segment byte oracle; not Resident history validation; timing is diagnostic only",
            "closed_history_entries":0,"result":result
        })).unwrap()).unwrap();
    }
}

#[test]
fn small_store_scratch_matches_block_limit_through_write_and_compaction() {
    for budget in [8 << 10, 12 << 10, 8 << 20] {
        let resources = store_with_budget(budget);
        let capacity =
            (resources.shared.target as usize - FILE_HEADER as usize).min(WRITE_BUFFER_BYTES);
        let discarded = save(&resources, &[11; 90]);
        let retained = save(&resources, &[22; 50]);
        let block = retained.0.blocks[0].clone();
        let old = match &*block.location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => panic!("pending block"),
        };
        let pointer = {
            let state = resources.shared.state.lock().unwrap();
            assert_eq!(state.write_scratch.capacity(), capacity);
            state.write_scratch.as_ptr()
        };
        drop(discarded);
        resources.collect().unwrap();
        assert!(resources.compact_one().unwrap());
        assert_eq!(
            resources
                .shared
                .read_placement(&block, &old, &mut || false)
                .unwrap(),
            vec![22; 50]
        );
        let contents = vec![33; capacity - BLOCK_HEADER as usize];
        let maximum = save(&resources, &contents);
        assert_eq!(
            resources
                .reader()
                .read(maximum.id(), &mut || false)
                .unwrap(),
            contents
        );
        assert_eq!(
            resources
                .reader()
                .read(retained.id(), &mut || false)
                .unwrap(),
            vec![22; 50]
        );
        assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
        drop((maximum, retained, block, old));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
        let state = resources.shared.state.lock().unwrap();
        assert_eq!(state.write_scratch.capacity(), capacity);
        assert_eq!(state.write_scratch.as_ptr(), pointer);
        assert_eq!(
            state.metadata,
            recalculate_metadata(&state, &resources.shared.blobs.lock().unwrap())
        );
    }
}

#[test]
fn small_store_scratch_reservation_fails_before_creating_a_file() {
    let resources = store_with_budget(12 << 10);
    let capacity = resources.shared.target as usize - FILE_HEADER as usize;
    let mut plan = ResourcePlan::default();
    let handle = resources
        .plan(b"small bounded writer", &[], &mut plan)
        .unwrap();
    {
        let mut state = resources.shared.state.lock().unwrap();
        prune(&mut state, &resources.shared).unwrap();
        state.metadata_limit = metadata_bytes(&state) + capacity - 1;
    }
    assert!(
        resources
            .commit(&plan)
            .unwrap_err()
            .to_string()
            .contains("metadata")
    );
    assert_eq!(resources.physical_bytes(), 0);
    assert!(!handle.0.ready.load(Ordering::Acquire));
    {
        let mut state = resources.shared.state.lock().unwrap();
        assert!(state.chunks.is_empty());
        assert_eq!(state.write_scratch.capacity(), 0);
        assert!(matches!(
            *handle.0.blocks[0].location.lock().unwrap(),
            BlockLocation::Pending(_)
        ));
        state.metadata_limit = MAX_METADATA_BYTES;
    }
    resources.commit(&plan).unwrap();
    assert_eq!(
        resources.reader().read(handle.id(), &mut || false).unwrap(),
        b"small bounded writer"
    );
    drop((handle, plan));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
}

#[cfg(feature = "resource-test-write-faults")]
#[test]
fn small_store_failed_writes_keep_pending_records_and_exact_prefix_accounting() {
    for fault in [
        Fault::Write {
            skip: 0,
            prefix: 41,
        },
        Fault::Flush,
    ] {
        let resources = store_with_budget(12 << 10);
        let old = save(&resources, b"old");
        let before = resources.physical_bytes();
        let mut plan = ResourcePlan::default();
        let handle = resources.plan(&[7; 100], &[], &mut plan).unwrap();
        let guard = FaultGuard::install(fault);
        assert!(resources.commit(&plan).is_err());
        let added = match fault {
            Fault::Write { prefix, .. } => prefix as u64,
            Fault::Flush => BLOCK_HEADER + 100,
        };
        assert_eq!(resources.physical_bytes(), before + added);
        assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
        assert!(!handle.0.ready.load(Ordering::Acquire));
        assert!(matches!(
            *handle.0.blocks[0].location.lock().unwrap(),
            BlockLocation::Pending(_)
        ));
        assert_eq!(
            resources.reader().read(old.id(), &mut || false).unwrap(),
            b"old"
        );
        assert!(
            resources
                .shared
                .state
                .lock()
                .unwrap()
                .write_scratch
                .is_empty()
        );
        drop(guard);
        resources.commit(&plan).unwrap();
        assert_eq!(
            resources.reader().read(handle.id(), &mut || false).unwrap(),
            vec![7; 100]
        );
        drop((handle, plan, old));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
    }
}

#[cfg(feature = "resource-test-write-faults")]
#[test]
fn small_store_failed_compaction_preserves_the_original_placement() {
    for fault in [
        Fault::Write {
            skip: 1,
            prefix: 41,
        },
        Fault::Flush,
    ] {
        let resources = store_with_budget(12 << 10);
        let discarded = save(&resources, &[13; 90]);
        let retained = save(&resources, &[23; 50]);
        let block = retained.0.blocks[0].clone();
        let old = match &*block.location.lock().unwrap() {
            BlockLocation::Stored(placement) => placement.clone(),
            _ => panic!("pending block"),
        };
        drop(discarded);
        resources.collect().unwrap();
        let before = resources.physical_bytes();
        let guard = FaultGuard::install(fault);
        assert!(resources.compact_one().is_err());
        let added = match fault {
            Fault::Write { prefix, .. } => FILE_HEADER + prefix as u64,
            Fault::Flush => FILE_HEADER + BLOCK_HEADER + 50,
        };
        assert_eq!(resources.physical_bytes(), before + added);
        assert_eq!(resources.physical_bytes(), actual_bytes(&resources));
        assert!(
            matches!(&*block.location.lock().unwrap(), BlockLocation::Stored(placement) if Arc::ptr_eq(&placement.chunk, &old.chunk) && placement.offset == old.offset)
        );
        assert_eq!(
            resources
                .reader()
                .read(retained.id(), &mut || false)
                .unwrap(),
            vec![23; 50]
        );
        drop(guard);
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), before);
        assert!(resources.compact_one().unwrap());
        assert_eq!(
            resources
                .shared
                .read_placement(&block, &old, &mut || false)
                .unwrap(),
            vec![23; 50]
        );
        drop((retained, block, old));
        resources.collect().unwrap();
        assert_eq!(resources.physical_bytes(), 0);
        assert_eq!(
            resources
                .shared
                .state
                .lock()
                .unwrap()
                .write_scratch
                .capacity(),
            376
        );
    }
}
