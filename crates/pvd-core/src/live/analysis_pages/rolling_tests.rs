use super::*;

#[test]
#[ignore = "requires the fixed real SDK 970-frame capture"]
fn actual_64_mib_history_expires_disk_leases_without_invalidating_displayed_analysis() {
    let bytes =
        std::fs::read(std::env::var_os("PVD_SHARED_HISTORY_RAW").expect("fixed real capture path"))
            .unwrap();
    assert_eq!(bytes.len(), 35_817_266);
    assert_eq!(
        blake3::hash(&bytes).to_hex().as_str(),
        "ede45af3f4efb98bae28c4510f4df2aace6d668dac03fbf4cc4a3d2baa807d60"
    );
    let mut full_config = config(8 << 30, 40);
    full_config.min_free_disk_bytes = 512 << 20;
    let mut rolling_config = config(64 << 20, 40);
    rolling_config.min_free_disk_bytes = 512 << 20;
    let mut full = crate::LiveRecording::with_legacy_history(full_config).unwrap();
    let mut rolling = crate::LiveRecording::with_paged_history(rolling_config).unwrap();
    let mut paused = None;
    let mut first_metadata = None;
    let mut paused_hash = None;
    let mut peak_disk = 0;
    let mut inspected = None;
    let mut displayed = None;
    for chunk in bytes.chunks(64 << 10) {
        full.feed(chunk).unwrap();
        let frames = rolling.feed(chunk).unwrap();
        if frames == 0 {
            continue;
        }
        if paused.is_none() {
            let snapshot = rolling.snapshot().unwrap();
            let disk = snapshot.disk.as_ref().unwrap();
            let root = disk
                .resident_analysis_at_with_cancel(0, &mut || false)
                .unwrap();
            paused_hash = Some(blake3::hash(&postcard::to_stdvec(&root).unwrap()));
            paused = Some(root);
            first_metadata = Some(snapshot);
        }
        let history = &rolling
            .builder
            .as_ref()
            .unwrap()
            .streaming
            .as_ref()
            .unwrap()
            .history;
        let resources = history.resources.as_ref().unwrap();
        let physical = history.bytes();
        peak_disk = peak_disk.max(physical);
        assert!(
            physical.saturating_add(resources.reserve_bytes()) <= 64 << 20,
            "disk={physical} reserve={}",
            resources.reserve_bytes()
        );
        assert!(history.warning.is_none(), "{:?}", history.warning);
        let frame = frames - 1;
        if inspected != Some(frame / 80) {
            let snapshot = rolling.snapshot().unwrap();
            let disk = snapshot.disk.as_ref().unwrap();
            let root = disk
                .resident_analysis_at_with_cancel(frame, &mut || false)
                .unwrap();
            displayed = Some(root);
            inspected = Some(frame / 80);
        }
    }
    let full = full.finish().unwrap();
    let rolling = rolling.finish().unwrap();
    let disk = rolling.disk.as_ref().unwrap();
    assert_eq!(rolling.frame_count(), 970);
    assert!(disk.stats.warning.is_none(), "{:?}", disk.stats.warning);
    assert!(disk.stats.first_frame > 0);
    assert!(!first_metadata.as_ref().unwrap().is_frame_retained(0));
    assert!(
        first_metadata
            .as_ref()
            .unwrap()
            .disk
            .as_ref()
            .unwrap()
            .resident_analysis_at_with_cancel(0, &mut || false)
            .is_err()
    );
    for frame in [
        disk.stats.first_frame,
        (disk.stats.first_frame + 969) / 2,
        969,
    ] {
        let statistics = paused.as_ref().unwrap().domain().statistics();
        println!(
            "actual64_history before frame={frame} first={} used={} peak={} strong_layout={} weak_fixed={} other={} cells={}",
            disk.stats.first_frame,
            statistics.logical_used,
            statistics.logical_peak,
            statistics.live_strong_allocation_bytes,
            statistics.logical_weak_fixed_bytes,
            statistics
                .logical_used
                .saturating_sub(statistics.live_strong_allocation_bytes)
                .saturating_sub(statistics.logical_weak_fixed_bytes),
            statistics.live_strong_cells
        );
        let actual = disk
            .resident_analysis_at_with_cancel(frame, &mut || false)
            .unwrap();
        assert!(
            postcard::to_stdvec(&actual).unwrap()
                == postcard::to_stdvec(&full.analysis_at(frame).unwrap()).unwrap()
        );
        assert!(rolling.state_at(frame).unwrap() == full.state_at(frame).unwrap());
        displayed = Some(actual);
    }
    assert_eq!(
        blake3::hash(&postcard::to_stdvec(paused.as_ref().unwrap()).unwrap()),
        paused_hash.unwrap()
    );
    let resources = disk
        .segments
        .front()
        .map(|segment| segment.resources.clone())
        .or_else(|| {
            disk.active
                .as_ref()
                .map(|segment| segment.resources.clone())
        })
        .unwrap();
    let domain = paused.as_ref().unwrap().domain().clone();
    println!(
        "actual64_history first={} last=969 retained={} peak_disk={} final_disk={} domain_used={} domain_peak={} reserve_min=536870912",
        disk.stats.first_frame,
        disk.stats.retained_frames,
        peak_disk,
        disk.stats.disk_bytes,
        domain.statistics().logical_used,
        domain.statistics().logical_peak
    );
    drop((displayed, first_metadata, rolling, full));
    resources.collect().unwrap();
    assert_eq!(resources.physical_bytes(), 0);
    assert!(domain.statistics().logical_used > 0);
    drop(paused);
    assert_eq!(domain.statistics().logical_used, 0);
}
