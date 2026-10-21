use super::*;
use std::sync::mpsc;

fn interrupted(failure: &Error) {
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn baseline_budget_retry_observes_cancellation_before_reclaiming_the_reader() {
    let mut fixture = Fixture::new(0, 2, false);
    fixture.history.append(
        &FrameInfo {
            index: 0,
            timestamp: 0,
            scene_id: 1,
        },
        &SceneState::default(),
        &SceneUpdate::default(),
        &Analysis::default(),
        Vec::new(),
    );
    assert!(fixture.history.warning.is_none());
    fixture.history.close_segment();
    fixture.recording.disk = Some(fixture.history.snapshot());
    let segment = fixture
        .recording
        .disk
        .as_ref()
        .unwrap()
        .segments
        .front()
        .unwrap()
        .clone();
    let mut cursor = Cursor::open(&segment).unwrap();
    let bytes = read_record_bytes_with_cancel(
        &mut cursor.reader,
        segment.analysis_base,
        segment.committed_end,
        &mut || false,
    )
    .unwrap();
    let root = match cursor.reader.decode(&bytes, &mut || false).unwrap() {
        Record::AnalysisPages(root) => root,
        _ => panic!("expected a paged baseline"),
    };
    let mut reader = analysis_pages::AnalysisPageReader::for_capture(&segment.resources).unwrap();
    let warmed: SharedAnalysis = reader
        .read(root, &segment.resources, &mut || false)
        .unwrap()
        .into();
    let domain = warmed.resident().unwrap().domain().clone();
    let wire = postcard::to_stdvec(&warmed).unwrap();
    assert_eq!(wire, postcard::to_stdvec(&Analysis::default()).unwrap());
    drop(warmed);
    let weak_bytes = domain.statistics().logical_weak_fixed_bytes;
    assert!(weak_bytes > 0);

    let disk = fixture.recording.disk.as_mut().unwrap();
    disk.analysis_pages = Some(reader);
    disk.analysis_cache = AnalysisCache::default();
    let marker: SharedAnalysis = ResidentAnalysis::new(Domain::new(1 << 20)).unwrap().into();
    disk.analysis_cache.insert(42, marker, &segment.lease);
    assert_eq!(disk.analysis_cache.stats().0, 1);
    let stats = domain.statistics();
    let pressure = domain.reserve(stats.limit - stats.logical_used).unwrap();
    let mut cancelled_after_eviction = false;
    let mut profile = SeekProfile::default();
    let failure = cursor
        .baseline(
            disk,
            &segment,
            &mut || {
                let cancelled = disk.analysis_cache.stats().0 == 0;
                cancelled_after_eviction |= cancelled;
                cancelled
            },
            &mut profile,
        )
        .err()
        .expect("the cancelled baseline must not be published");
    interrupted(&failure);
    assert!(cancelled_after_eviction);
    assert_eq!(profile.metrics.baseline_loads, 1);
    assert_eq!(disk.analysis_cache.stats().0, 0);
    assert_eq!(domain.statistics().logical_weak_fixed_bytes, weak_bytes);

    drop(pressure);
    let checkpoint = cursor
        .baseline(disk, &segment, &mut || false, &mut SeekProfile::default())
        .unwrap();
    assert_eq!(postcard::to_stdvec(&checkpoint.data).unwrap(), wire);
    drop((checkpoint, cursor, segment, fixture));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn frame_budget_retry_cancels_while_the_shared_reader_registry_is_busy() {
    let fixture = Fixture::new(0, 2, false);
    let disk = fixture.recording.disk.as_ref().unwrap();
    let domain = Domain::new(8 << 20);
    let before: SharedAnalysis = ResidentAnalysis::new(domain.clone()).unwrap().into();
    let wire = postcard::to_stdvec(&before).unwrap();
    let mut cached = before.clone();
    cached.add_zone(7, "x".repeat(1 << 20)).unwrap();
    let lease = ResourceLease::new(Vec::new());
    disk.analysis_cache.insert(4, cached, &lease);
    assert_eq!(disk.analysis_cache.stats().0, 1);
    let stats = domain.statistics();
    let pressure = domain.reserve(stats.limit - stats.logical_used).unwrap();
    let inputs = [AnalysisInput::Zone(
        9,
        "published after cancellation".into(),
    )];
    let reader = disk.analysis_pages.as_ref().unwrap().clone();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (finished_tx, finished_rx) = mpsc::channel();

    let (finished_before_unlock, result, cancelled_after_eviction) = std::thread::scope(|scope| {
        let holder = scope.spawn(move || {
            reader.with_registry_locked_for_test(|| {
                held_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(15)).unwrap();
            })
        });
        held_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let worker = scope.spawn(|| {
            let mut cancelled_after_eviction = false;
            let result = apply_analysis_frame(disk, &before, &inputs, &mut || {
                let cancelled = disk.analysis_cache.stats().0 == 0;
                cancelled_after_eviction |= cancelled;
                cancelled
            });
            let _ = finished_tx.send(());
            (result, cancelled_after_eviction)
        });
        let finished_before_unlock = finished_rx.recv_timeout(Duration::from_secs(5)).is_ok();
        let _ = release_tx.send(());
        holder.join().unwrap().unwrap();
        let (result, cancelled_after_eviction) = worker.join().unwrap();
        (finished_before_unlock, result, cancelled_after_eviction)
    });

    interrupted(&result.unwrap_err());
    assert!(cancelled_after_eviction);
    assert_eq!(disk.analysis_cache.stats().0, 0);
    assert_eq!(postcard::to_stdvec(&before).unwrap(), wire);
    drop(pressure);
    let after = apply_analysis_frame(disk, &before, &inputs, &mut || false).unwrap();
    let mut expected = Analysis::default();
    inputs[0].apply(&mut expected).unwrap();
    assert_eq!(
        postcard::to_stdvec(&after).unwrap(),
        postcard::to_stdvec(&expected).unwrap()
    );
    drop((after, before, lease, fixture));
    assert_eq!(domain.statistics().logical_used, 0);
    assert!(
        finished_before_unlock,
        "the cancelled budget retry waited for the reader registry lock"
    );
}
