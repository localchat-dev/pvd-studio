#[path = "../../pvd-desktop/src/scene/source_index.rs"]
mod source_index;

use pvd_core::{LiveRecording, SceneState, live::HistoryConfig};
use source_index::SourceIndex;
use std::{collections::HashSet, fs::File, hint::black_box, io::Read, time::Instant};

fn collect(state: &SceneState, index: Option<&SourceIndex>) -> serde_json::Value {
    let started = Instant::now();
    let cloth = black_box(match index {
        Some(index) => pvd_core::cloth::collect_from_ids(black_box(state), &index.cloths),
        None => pvd_core::cloth::collect(black_box(state)),
    });
    let cloth_done = Instant::now();
    let particles = black_box(match index {
        Some(index) => pvd_core::particles::collect_from_ids(black_box(state), &index.particles),
        None => pvd_core::particles::collect(black_box(state)),
    });
    let particles_done = Instant::now();
    let queries = black_box(match index {
        Some(index) => pvd_core::queries::collect_from_ids(black_box(state), &index.scenes),
        None => pvd_core::queries::collect(black_box(state)),
    });
    let queries_done = Instant::now();
    let output_counts = serde_json::json!({
        "cloths":cloth.cloths.len(), "cloth_particles":cloth.particle_count,
        "cloth_constraints":cloth.constraint_count, "cloth_diagnostics":cloth.diagnostics.len(),
        "particle_systems":particles.systems.len(),
        "particles":particles.systems.iter().map(|system|system.particles.len()).sum::<usize>(),
        "particle_diagnostics":particles.diagnostics.len(), "raycasts":queries.raycasts.len(),
        "sweeps":queries.sweeps.len(), "overlaps":queries.overlaps.len(),
        "query_diagnostics":queries.diagnostics.len()
    });
    let dropping = Instant::now();
    drop((cloth, particles, queries));
    let drop_done = Instant::now();
    let ms = |duration: std::time::Duration| duration.as_secs_f64() * 1000.0;
    let cloth_ms = ms(cloth_done - started);
    let particles_ms = ms(particles_done - cloth_done);
    let queries_ms = ms(queries_done - particles_done);
    let drop_ms = ms(drop_done - dropping);
    serde_json::json!({"cloth_ms":cloth_ms,"particles_ms":particles_ms,
        "queries_ms":queries_ms,"collect_total_ms":ms(queries_done-started),
        "drop_ms":drop_ms,"collect_and_drop_ms":cloth_ms+particles_ms+queries_ms+drop_ms,
        "result_counts":output_counts})
}

fn main() -> Result<(), pvd_core::Error> {
    let args: Vec<_> = std::env::args_os().collect();
    let input_path = args
        .get(1)
        .ok_or("usage: source_index_scans INPUT HISTORY_DIR")?;
    let directory = args.get(2).ok_or("missing history directory")?;
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: directory.into(),
        min_free_disk_bytes: 0,
        segment_frames: 40,
        disk_budget_bytes: 8 << 30,
        ..Default::default()
    })?;
    let load_started = Instant::now();
    let mut input = File::open(input_path)?;
    let mut buffer = [0; 65536];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        live.feed(&buffer[..read])?;
    }
    let recording = live.finish()?;
    eprintln!("load_seconds={:.6}", load_started.elapsed().as_secs_f64());
    for frame in [150, 229, 800] {
        let previous = recording.state_at(frame - 1)?;
        let state = recording.state_at(frame)?;
        let changed: HashSet<_> = previous
            .objects
            .keys()
            .chain(state.objects.keys())
            .copied()
            .filter(|id| previous.objects.get(id) != state.objects.get(id))
            .collect();
        let mut index = SourceIndex::default();
        index.update(&state, None);
        assert!(index.complete);
        assert_eq!(
            pvd_core::cloth::collect(&state),
            pvd_core::cloth::collect_from_ids(&state, &index.cloths)
        );
        assert_eq!(
            pvd_core::particles::collect(&state),
            pvd_core::particles::collect_from_ids(&state, &index.particles)
        );
        assert_eq!(
            pvd_core::queries::collect(&state),
            pvd_core::queries::collect_from_ids(&state, &index.scenes)
        );
        println!(
            "{}",
            serde_json::json!({"phase":"state", "frame":frame,
            "counts":{"objects":state.objects.len(),"cloth_objects":index.cloths.len(),
                "particle_objects":index.particles.len(),"scene_objects":index.scenes.len(),
                "changed_objects":changed.len()},"full_output_equal":true})
        );
        let expected = collect(&state, None)["result_counts"].clone();
        for _ in 0..5 {
            assert_eq!(collect(&state, None)["result_counts"], expected);
            assert_eq!(collect(&state, Some(&index))["result_counts"], expected);
        }
        for sample in 0..64 {
            let (full, indexed) = if sample % 2 == 0 {
                (collect(&state, None), collect(&state, Some(&index)))
            } else {
                let indexed = collect(&state, Some(&index));
                (collect(&state, None), indexed)
            };
            assert_eq!(full["result_counts"], expected);
            assert_eq!(indexed["result_counts"], expected);
            let mut delta = SourceIndex::default();
            delta.update(&previous, None);
            let start = Instant::now();
            delta.update(black_box(&state), black_box(Some(&changed)));
            black_box(&delta);
            let delta_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert!(delta.complete);
            assert_eq!(delta.scenes, index.scenes);
            assert_eq!(delta.cloths, index.cloths);
            assert_eq!(delta.particles, index.particles);
            let start = Instant::now();
            delta.update(black_box(&state), None);
            black_box(&delta);
            let rebuild_ms = start.elapsed().as_secs_f64() * 1000.0;
            println!(
                "{}",
                serde_json::json!({"phase":"sample","frame":frame,"sample":sample,
                "full_first":sample%2==0,"full":full,"indexed":indexed,
                "delta_ms":delta_ms,"rebuild_ms":rebuild_ms})
            );
        }
    }
    Ok(())
}
