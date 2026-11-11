use std::{path::Path, time::Instant};

use pvd_core::native::export::{self, Selection};

pub fn run(source: &str, destination: &str, selection: Selection) -> crate::Result<()> {
    let destination = Path::new(destination);
    if !destination
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pxd2"))
    {
        return Err("PVD3 container export requires a .pxd2 output".into());
    }
    if let Selection::Frames { first, last } = selection
        && first > last
    {
        return Err("PVD3 clip start must not exceed its end".into());
    }
    let cancel = crate::cancellation::Cancellation::install()?;
    let mut source = crate::input::Input::open_locked(source, |_, _| !cancel.requested())?;
    if !source.is_native()? {
        return Err("native export requires a PVD3 database; SDK streams use trim".into());
    }
    let mut updated = Instant::now();
    let report = crate::atomic_output::write_new_cancellable(destination, &cancel, |output| {
        export::write_reader(&mut source, selection, output, |progress| {
            if updated.elapsed().as_millis() >= 250 {
                eprintln!(
                    "native {:?}: {}/{} bytes",
                    progress.phase, progress.completed, progress.total
                );
                updated = Instant::now();
            }
            !cancel.requested()
        })
    })?;
    println!(
        "source_bytes: {}\nsource_blake3: {}\nsource_frames: {}\noutput_bytes: {}\nframes: {}\nevents: {}\nsaved_instances: {}",
        report.source_bytes,
        report.source_blake3,
        report.source_frames,
        report.output_bytes,
        report.frames,
        report.events,
        report.saved_instances
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_format_and_reversed_interval_are_rejected_before_io() {
        assert!(
            run("missing.pxd2", "output.pvdstream", Selection::Whole)
                .unwrap_err()
                .to_string()
                .contains(".pxd2")
        );
        assert!(
            run(
                "missing.pxd2",
                "output.pxd2",
                Selection::Frames { first: 3, last: 1 },
            )
            .unwrap_err()
            .to_string()
            .contains("start must not exceed")
        );
    }
}
