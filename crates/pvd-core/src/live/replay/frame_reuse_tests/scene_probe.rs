use super::*;
use crate::analysis::view::{MemorySource, MemoryViewCursor};
use serde::Serialize;

mod cold_phases;

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

fn scene_hash(scene: &SceneState) -> String {
    digest(&(
        &scene.objects,
        &scene.cameras,
        &scene.errors,
        &scene.debug_draw,
        &scene.contacts,
        &scene.reference_lifetimes,
        &scene.generations,
    ))
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
struct Reference {
    phase: &'static str,
    frame: usize,
    scene: String,
    analysis: String,
    memory: String,
    churn: String,
}

fn reference(
    phase: &'static str,
    frame: usize,
    scene: &SceneState,
    analysis: &SharedAnalysis,
) -> Reference {
    let memory = analysis.memory_snapshot();
    let mut state = MemoryViewCursor::default();
    memory.seek(&mut state, frame).unwrap();
    Reference {
        phase,
        frame,
        scene: scene_hash(scene),
        analysis: digest(analysis),
        memory: digest(&state),
        churn: digest(&memory.churn(frame)),
    }
}

fn reset(recording: &mut crate::Recording) {
    recording.disk.as_mut().unwrap().analysis_cache = AnalysisCache::default();
}

fn sequence(first: usize, last: usize) -> Vec<(&'static str, usize)> {
    let cold = (first + 3).min(last);
    let end = (cold + 5).min(last);
    let mut result = vec![("logical_cold", cold)];
    result.extend((cold + 1..=end).map(|frame| ("adjacent_forward", frame)));
    result.extend((cold..end).rev().map(|frame| ("adjacent_reverse", frame)));
    result
}

fn references(
    recording: &mut crate::Recording,
    sequence: &[(&'static str, usize)],
) -> Vec<Reference> {
    reset(recording);
    let mut cursor = ReplayCursor::default();
    let mut scene = SceneState::default();
    sequence
        .iter()
        .map(|&(phase, frame)| {
            let replay = cursor.seek(recording, frame, || false).unwrap();
            replay.update.apply(&mut scene);
            reference(phase, frame, &scene, &replay.analysis)
        })
        .collect()
}

fn run_arm(
    recording: &mut crate::Recording,
    expected: &[Reference],
    full: bool,
) -> Vec<serde_json::Value> {
    reset(recording);
    let mut cursor = ReplayCursor::default();
    let mut scene = SceneState::default();
    let mut displayed_analysis = None;
    let mut rows = Vec::new();
    for row in expected {
        let elapsed;
        if full {
            let started = Instant::now();
            let replay = cursor.seek(recording, row.frame, || false).unwrap();
            elapsed = started.elapsed();
            replay.update.apply(&mut scene);
            displayed_analysis = Some(replay.analysis);
            let actual = reference(
                row.phase,
                row.frame,
                &scene,
                displayed_analysis.as_ref().unwrap(),
            );
            assert_eq!(actual, *row);
            assert_eq!(
                cursor
                    .current
                    .as_ref()
                    .unwrap()
                    .analysis
                    .as_ref()
                    .unwrap()
                    .frame,
                row.frame
            );
        } else {
            let started = Instant::now();
            let replay = cursor.seek_scene(recording, row.frame, || false).unwrap();
            elapsed = started.elapsed();
            assert_eq!(replay.frame, row.frame);
            replay.update.apply(&mut scene);
            assert_eq!(scene_hash(&scene), row.scene);
            assert!(cursor.current.as_ref().unwrap().analysis.is_none());
        }
        let (cache_entries, cache_bytes) = recording.disk.as_ref().unwrap().analysis_cache.stats();
        assert!(cache_bytes <= 192 << 20);
        rows.push(serde_json::json!({"phase":row.phase,"frame":row.frame,"mode":if full {"full"} else {"scene_only"},"seek_ns":elapsed.as_nanos() as u64,"scene_hash":row.scene,"analysis_hash":full.then_some(&row.analysis),"memory_hash":full.then_some(&row.memory),"churn_hash":full.then_some(&row.churn),"cache_entries":cache_entries,"cache_bytes":cache_bytes}));
    }
    drop(displayed_analysis);
    rows
}

fn verify_demands(recording: &mut crate::Recording, expected: &[Reference]) {
    reset(recording);
    let mut cursor = ReplayCursor::default();
    let mut scene = SceneState::default();
    for row in expected {
        let replay = cursor.seek_scene(recording, row.frame, || false).unwrap();
        assert_eq!(replay.frame, row.frame);
        replay.update.apply(&mut scene);
        assert_eq!(scene_hash(&scene), row.scene);
        let replay = cursor.seek(recording, row.frame, || false).unwrap();
        assert!(replay.update.objects.is_empty());
        replay.update.apply(&mut scene);
        assert_eq!(
            reference(row.phase, row.frame, &scene, &replay.analysis),
            *row
        );
        assert_eq!(
            cursor
                .current
                .as_ref()
                .unwrap()
                .analysis
                .as_ref()
                .unwrap()
                .frame,
            row.frame
        );
    }
}

#[test]
fn mode_comparison_and_deferred_analysis_oracle_match_the_fixture() {
    let mut fixture = Fixture::new(14, 4, true, false);
    let expected = references(&mut fixture.recording, &sequence(0, 13));
    for row in &expected {
        assert_eq!(row.scene, scene_hash(&fixture.scenes[row.frame]));
        assert_eq!(
            row.analysis,
            blake3::hash(&fixture.analyses[row.frame])
                .to_hex()
                .to_string()
        );
    }
    let full = run_arm(&mut fixture.recording, &expected, true);
    let scene = run_arm(&mut fixture.recording, &expected, false);
    assert_eq!(full.len(), 11);
    assert_eq!(scene.len(), 11);
    verify_demands(&mut fixture.recording, &expected);
}

#[test]
#[ignore = "requires full real SDK capture and a coordinated quiet window; same-build scene/full API comparison"]
fn full_capture_scene_and_analysis_demand_probe() {
    if cfg!(any(
        feature = "replay-test-read-counts",
        feature = "resource-test-write-faults"
    )) {
        panic!("This probe requires default test features without internal counters or faults");
    }
    let input = PathBuf::from(std::env::var_os("PVD_SCENE_CAPTURE").expect("PVD_SCENE_CAPTURE"));
    let output = PathBuf::from(std::env::var_os("PVD_SCENE_OUTPUT").expect("PVD_SCENE_OUTPUT"));
    std::fs::create_dir_all(&output).unwrap();
    let history = output.join("history");
    assert!(!history.exists());
    assert!(!output.join("summary.json").exists());
    let mut live = crate::LiveRecording::with_history(HistoryConfig {
        directory: history.clone(),
        min_free_disk_bytes: 0,
        ..Default::default()
    })
    .unwrap();
    let mut file = File::open(&input).unwrap();
    let input_bytes = file.metadata().unwrap().len();
    assert!(input_bytes <= 512 << 20);
    let mut checksum = blake3::Hasher::new();
    let mut buffer = [0; 65536];
    let mut fed = 0usize;
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        checksum.update(&buffer[..count]);
        live.feed(&buffer[..count]).unwrap();
        fed += count;
    }
    assert_eq!(fed as u64, input_bytes);
    let mut recording = live.finish().unwrap();
    let disk = recording.disk.as_ref().unwrap();
    let ranges: Vec<_> = disk
        .segments
        .iter()
        .map(Arc::as_ref)
        .chain(&disk.active)
        .map(|segment| {
            (
                segment.frames.first().unwrap().info.index,
                segment.frames.last().unwrap().info.index,
            )
        })
        .collect();
    assert_eq!(
        ranges.len(),
        33,
        "This probe expects the fixed 600-second capture with default 600-frame segments"
    );
    let stats = serde_json::json!({"frames":recording.frame_count(),"segments":ranges.len(),"disk_bytes":disk.stats.disk_bytes});
    if let Some(reference) = std::env::var_os("PVD_SCENE_PHASE_REFERENCE") {
        assert!(std::env::var_os("PVD_SCENE_SELECTED_SEGMENTS").is_none());
        cold_phases::run(
            recording,
            &ranges,
            stats,
            fed,
            checksum.finalize().to_hex().to_string(),
            Path::new(&reference),
            &output,
        );
        return;
    }
    let selected = std::env::var_os("PVD_SCENE_SELECTED_SEGMENTS")
        .map(|value| {
            let value = value
                .into_string()
                .expect("segment selection must be valid Unicode");
            assert!(value.len() <= 128, "segment selection is too long");
            let indices: Vec<usize> = value
                .split(',')
                .map(|index| {
                    index
                        .parse()
                        .expect("segment selection must contain indices")
                })
                .collect();
            assert!(!indices.is_empty());
            assert!(indices.iter().all(|&index| index < ranges.len()));
            assert!(indices.windows(2).all(|pair| pair[0] < pair[1]));
            indices
        })
        .unwrap_or_else(|| (0..ranges.len()).collect());
    let mut groups = Vec::new();
    for &index in &selected {
        let (first, last) = ranges[index];
        let expected = references(&mut recording, &sequence(first, last));
        let order = if index % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        };
        let mut arms = Vec::new();
        for full in order {
            arms.push(run_arm(&mut recording, &expected, full));
        }
        verify_demands(&mut recording, &expected);
        groups.push(serde_json::json!({"first":first,"last":last,"order":order,"reference":expected,"arms":arms,"every_target_mixed_demand_verified":true}));
        eprintln!("Verified segment {} / {}", index + 1, ranges.len());
    }
    drop(recording);
    assert!(std::fs::read_dir(&history).unwrap().next().is_none());
    let summary = serde_json::json!({"scope":"same-build scene-only versus full API; full seek is the semantic reference, not an independent SDK oracle; cache resets define logical cold, filesystem cache is not flushed; scene-update application/hash/reference and mixed-demand checks are outside timed calls; full arm retains its displayed Analysis until next result; optional internal test counters are compiled out","input_bytes":fed,"input_blake3":checksum.finalize().to_hex().to_string(),"stats":stats,"selected_segment_indices":selected,"groups":groups,"closed_history_entries":0});
    std::fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .unwrap();
}
