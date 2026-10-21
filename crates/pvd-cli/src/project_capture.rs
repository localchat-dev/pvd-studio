use crate::{Recording, Result};
use std::{fs::File, io::Read};

pub fn is_capture(path: &str) -> Result<bool> {
    let mut signature = [0; 8];
    let mut file = File::open(path)?;
    match file.read_exact(&mut signature) {
        Ok(()) => Ok(pvd_core::capture::Format::from_magic(&signature)?.is_some()),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub fn require_full_source(path: &str) -> Result<()> {
    if is_capture(path)? {
        return Err("project captures contain only a retained interval, not the complete raw SDK source; full-source analysis, CSV and raw trimming are unavailable".into());
    }
    Ok(())
}

pub fn sample_frames(recording: &Recording) -> Result<[usize; 3]> {
    let first = recording.first_frame();
    let end = recording.frame_count();
    if first >= end {
        return Err("no retained completed scene frames".into());
    }
    Ok([first, first + (end - first) / 2, end - 1])
}

pub fn print_range(recording: &Recording) {
    if recording.source == pvd_core::RecordingSource::ProjectCapture {
        println!(
            "format: pvd-studio project capture ({})\nfirst_visible_frame: {}\nlast_visible_frame: {}\nvisible_frames: {}\noriginal_pvd3_compatible: no\ncomplete_raw_source: unavailable",
            recording
                .capture_format()
                .expect("project capture format")
                .name(),
            recording.first_frame(),
            recording.frame_count().saturating_sub(1),
            recording
                .frame_count()
                .saturating_sub(recording.first_frame())
        );
    }
}

pub fn inspect(path: &str) -> Result<()> {
    let started = std::time::Instant::now();
    let recording = Recording::from_path(path)?;
    let [first, _, last] = sample_frames(&recording)?;
    let state = recording.state_at(last)?;
    println!("file: {path}\nbytes: {}", std::fs::metadata(path)?.len());
    print_range(&recording);
    println!(
        "last_frame_objects: {}\nfirst_frame_seekable: {}\nparse_seconds: {:.6}",
        state.objects.len(),
        recording.is_frame_retained(first),
        started.elapsed().as_secs_f64()
    );
    for diagnostic in &recording.diagnostics {
        println!("diagnostic: {diagnostic}");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
