use super::*;
use crate::profile::analysis_probe;
use pvd_core::{
    analysis::ProfileSpan,
    analysis_export::AnalysisCsvKind,
    profile_curves::{CatalogEntry, ExactKey},
    profile_timeline::{GlobalPairingStats, ThreadEntry, TickRange},
};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Entry {
    zone: u64,
    event: u16,
    thread: u32,
    context: u64,
    zone_name: String,
    event_name: String,
    first_start: Option<u64>,
    complete_spans: usize,
    open_spans: usize,
    unmatched_stops: u64,
}

impl Entry {
    fn key(&self) -> ExactKey {
        ExactKey {
            zone: self.zone,
            event: self.event,
            thread: self.thread,
            context: self.context,
        }
    }

    fn expected(&self) -> CatalogEntry {
        CatalogEntry {
            key: self.key(),
            first_start: self.first_start,
            complete_spans: self.complete_spans,
            open_spans: self.open_spans,
            unmatched_stops: self.unmatched_stops,
            metadata_ambiguous: false,
        }
    }
}

#[derive(Deserialize)]
struct Section {
    kind: String,
    name: String,
    id: u64,
    tick: u64,
    frame_index: Option<usize>,
    source_generation: u32,
}

fn json<T: serde::de::DeserializeOwned>(directory: &Path, name: &str) -> T {
    serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap()
}

fn wait(studio: &mut Studio, context: &egui::Context, done: impl Fn(&Studio) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        studio.poll(context);
        if done(studio) {
            return;
        }
        assert!(Instant::now() < deadline, "{:?}", studio.notices);
        std::thread::park_timeout(Duration::from_millis(1));
    }
}

fn incomplete_memory(studio: &Studio) {
    let recording = studio.recording.as_ref().unwrap();
    assert!(recording.diagnostics.iter().any(|text| {
        text.starts_with("Analysis decoding stopped at frame")
            && text.contains("undefined memory type handle 13")
    }));
    assert_incomplete_analysis_is_not_exported(recording);
}

fn span_key(span: &ProfileSpan) -> ExactKey {
    ExactKey {
        zone: span.zone,
        event: span.event,
        thread: span.thread,
        context: span.context,
    }
}

fn span_fields(span: &ProfileSpan) -> (u64, u16, u32, u64, u64, u64, usize) {
    (
        span.zone,
        span.event,
        span.thread,
        span.context,
        span.start,
        span.end,
        span.received_frame,
    )
}

fn expected_threads(entries: &[Entry], spans: &[ProfileSpan]) -> Vec<ThreadEntry> {
    let mut threads = BTreeMap::new();
    for entry in entries {
        let thread = threads.entry(entry.thread).or_insert(ThreadEntry {
            thread: entry.thread,
            completed_tick_range: None,
            complete_spans: 0,
            open_starts: 0,
            unmatched_stops: 0,
        });
        thread.complete_spans += entry.complete_spans as u64;
        thread.open_starts += entry.open_spans as u64;
        thread.unmatched_stops += entry.unmatched_stops;
    }
    for span in spans {
        let thread = threads.get_mut(&span.thread).unwrap();
        thread.completed_tick_range = Some(match thread.completed_tick_range {
            Some(range) => TickRange {
                start: range.start.min(span.start),
                end: range.end.max(span.end),
            },
            None => TickRange {
                start: span.start,
                end: span.end,
            },
        });
    }
    threads.into_values().collect()
}

fn empty_directory(path: &Path) {
    if path.exists() {
        assert_eq!(std::fs::read_dir(path).unwrap().count(), 0, "{path:?}");
    }
}

#[test]
#[ignore = "requires PVD_APEX_ALL_CAPTURE and its independent PVD_APEX_PROFILE_ORACLE"]
fn apex_all_profile_workers_remain_available_after_memory_dictionary_failure() {
    let input = PathBuf::from(std::env::var_os("PVD_APEX_ALL_CAPTURE").unwrap());
    let oracle = PathBuf::from(std::env::var_os("PVD_APEX_PROFILE_ORACLE").unwrap());
    let summary: serde_json::Value = json(&oracle, "summary.json");
    let entries: Vec<Entry> = json(&oracle, "catalog.json");
    let spans: Vec<ProfileSpan> = json(&oracle, "spans.json");
    let sections: Vec<Section> = json(&oracle, "sections.json");
    let bytes = std::fs::read(&input).unwrap();
    assert_eq!(
        bytes.len() as u64,
        summary["capture_bytes"].as_u64().unwrap()
    );
    assert_eq!(summary["source_frames"], 72);
    assert_eq!(entries.len(), 138);
    assert_eq!(spans.len(), 6264);
    assert_eq!(
        Recording::from_bytes(&bytes).err().unwrap().to_string(),
        "undefined memory type handle 13"
    );

    let fixture = Fixture::new(&bytes);
    let context = egui::Context::default();
    crate::ui::configure(&context);
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.prefs.language = Language::English;
    studio.prefs.history_directory = fixture.history.directory.to_string_lossy().into_owned();
    studio.open(fixture.path.clone(), context.clone());
    wait(&mut studio, &context, |studio| {
        studio.job.is_none() && studio.seeking.is_none()
    });
    let recording = studio.recording.as_ref().unwrap();
    assert!(recording.is_disk_backed());
    assert_eq!(recording.frame_count(), 72);
    assert!(studio.complete_profile_source().is_some());
    assert!(studio.complete_timeline_source().is_some());
    incomplete_memory(&studio);

    let index_root = fixture.directory.join("profile-index");
    studio.profile_cpu_index.close();
    studio.profile_cpu_index = Arc::new(crate::full_profile::IndexSlot::for_tests(
        index_root.clone(),
        &studio.background_jobs,
    ));
    assert!(
        studio
            .complete_profile_source()
            .unwrap()
            .can_rebuild_cpu_index()
    );

    for first_start in [false, true] {
        let catalog = analysis_probe::catalog(&mut studio, &context, first_start);
        let mut expected: Vec<_> = entries.iter().map(Entry::expected).collect();
        if first_start {
            expected.sort_unstable_by_key(|entry| {
                (entry.first_start.is_none(), entry.first_start, entry.key)
            });
        } else {
            expected.sort_unstable_by_key(|entry| entry.key);
        }
        assert_eq!(catalog.entries, expected);
        for entry in &entries {
            let names = &catalog.names[&(entry.zone, entry.event)];
            assert_eq!(names.zone.as_deref(), Some(entry.zone_name.as_str()));
            assert_eq!(names.event.as_deref(), Some(entry.event_name.as_str()));
        }
        incomplete_memory(&studio);
    }

    let stats = GlobalPairingStats {
        complete_spans: summary["completed_spans"].as_u64().unwrap(),
        open_starts: summary["open_starts"].as_u64().unwrap(),
        unmatched_stops: summary["unmatched_stops"].as_u64().unwrap(),
    };
    let range = TickRange {
        start: summary["completed_tick_range"]["start"].as_u64().unwrap(),
        end: summary["completed_tick_range"]["end"].as_u64().unwrap(),
    };
    let threads = expected_threads(&entries, &spans);
    for streaming in [true, false] {
        let window = analysis_probe::thread_window(&mut studio, &context, streaming);
        assert_eq!(window.catalog.global_pairing, stats);
        assert_eq!(window.stats, stats);
        assert_eq!(
            window.catalog.clock.numerator,
            summary["clock"]["numerator"].as_u64().unwrap()
        );
        assert_eq!(
            window.catalog.clock.denominator,
            summary["clock"]["denominator"].as_u64().unwrap()
        );
        assert_eq!(window.catalog.completed_tick_range, Some(range));
        assert_eq!(window.catalog.threads, threads);
        assert_eq!(window.request.ticks, range);
        assert_eq!(window.request.thread, None);
        assert_eq!(window.request.context, None);
        assert!(window.index_bytes > 0);
        assert_eq!(
            window.spans.iter().map(span_fields).collect::<Vec<_>>(),
            spans.iter().map(span_fields).collect::<Vec<_>>()
        );
        incomplete_memory(&studio);
    }

    let reference: Vec<_> = sections
        .iter()
        .filter(|section| section.kind == "end" && section.name == "frame")
        .collect();
    assert_eq!(reference.len(), 36);
    let candidates: Vec<_> = entries
        .iter()
        .filter(|entry| entry.event_name == "ClothingScene::tickRenderProxies")
        .collect();
    assert_eq!(candidates.len(), 1);
    let event = candidates[0];
    assert_eq!(
        (
            event.complete_spans,
            event.open_spans,
            event.unmatched_stops
        ),
        (36, 0, 0)
    );
    studio.seek(reference[0].frame_index.unwrap());
    wait(&mut studio, &context, |studio| studio.seeking.is_none());
    let scene = studio.state.objects.get(&reference[0].id).unwrap();
    assert_eq!(scene.type_name, "physx3::PxScene");
    assert_eq!(scene.generation, reference[0].source_generation);
    studio.plot_profile_event(event.key(), context.clone());
    wait(&mut studio, &context, |studio| {
        studio.curve_job.is_none() && studio.curves.curves.iter().all(|curve| !curve.pending)
    });
    assert_eq!(studio.curves.curves.len(), 1);
    let curve = &studio.curves.curves[0];
    assert!(curve.error.is_none(), "{:?}", curve.error);
    let data = curve.data.as_ref().unwrap();
    assert_eq!(data.stride, 1);
    assert_eq!(data.source_frames, 72);
    assert_eq!(data.reference_frame_count, Some(reference.len()));
    assert_eq!(data.samples.len(), reference.len());
    assert_eq!(data.samples[0], (reference[0].frame_index.unwrap(), None));
    assert_eq!(data.statuses[0], "no_previous_frame");
    assert_eq!(data.occurrence_counts[0], 0);
    let scale = summary["clock"]["numerator"].as_u64().unwrap() as f64
        / summary["clock"]["denominator"].as_u64().unwrap() as f64
        / 1e6;
    for (point, interval) in reference.windows(2).enumerate() {
        let point = point + 1;
        let [lower, upper] = [interval[0].tick, interval[1].tick];
        let hits: Vec<_> = spans
            .iter()
            .filter(|span| {
                span_key(span) == event.key()
                    && span.end > lower
                    && span.end <= upper
                    && span.start >= lower
            })
            .collect();
        assert_eq!(hits.len(), 1);
        let expected_ms = (hits[0].end - hits[0].start) as f64 * scale;
        assert_eq!(data.samples[point].0, interval[1].frame_index.unwrap());
        assert!((data.samples[point].1.unwrap() - expected_ms).abs() <= 1e-12);
        assert_eq!(data.statuses[point], "recorded");
        assert_eq!(data.occurrence_counts[point], hits.len());
    }
    incomplete_memory(&studio);

    for (kind, name) in [
        (AnalysisCsvKind::Profile, "profile.csv"),
        (AnalysisCsvKind::Memory, "memory.csv"),
    ] {
        let output = fixture.directory.join(name);
        studio.analysis_export_path = output.to_string_lossy().into_owned();
        assert!(studio.can_export_analysis());
        studio.export_analysis(kind, context.clone());
        wait(&mut studio, &context, |studio| {
            studio.recording_export.is_none()
        });
        assert!(!output.exists());
        assert!(studio.notices.iter().any(|message| {
            message
                .text(Language::English)
                .contains("undefined memory type handle 13")
        }));
        incomplete_memory(&studio);
    }

    studio.close();
    crate::profile::sync_session(&context, studio.profile_view_epoch);
    let deadline = Instant::now() + Duration::from_secs(120);
    while !studio.poll_shutdown() {
        assert!(Instant::now() < deadline);
        std::thread::park_timeout(Duration::from_millis(1));
    }
    empty_directory(&fixture.history.directory);
    let index_entries: Vec<_> = std::fs::read_dir(&index_root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(index_entries, [index_root.join(".pvd-studio-profile-lock")]);
    assert_eq!(
        std::fs::read(&index_entries[0]).unwrap(),
        b"pvd-studio derived CPU profile root v1\n"
    );
    println!(
        "APEX ALL desktop profile workers: {} keys, {} spans, {} threads, {} curve samples; memory diagnostic retained; history and index released",
        entries.len(),
        spans.len(),
        threads.len(),
        reference.len()
    );
}
