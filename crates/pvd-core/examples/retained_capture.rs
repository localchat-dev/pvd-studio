use pvd_core::{
    LiveRecording, Recording,
    capture::{self, CaptureLimits, CaptureRange},
    live::HistoryConfig,
};
use std::{
    fs::{self, File},
    io::Read,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn main() -> Result<(), pvd_core::Error> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: retained_capture INPUT_RAW OUTPUT_CAPTURE FIRST LAST".into());
    }
    let range = CaptureRange {
        first: args[2].to_str().ok_or("invalid first frame")?.parse()?,
        last: args[3].to_str().ok_or("invalid last frame")?.parse()?,
    };
    let cache = std::env::current_dir()?.join("pvd-cache").join(format!(
        "capture-example-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir_all(cache.parent().unwrap())?;
    fs::create_dir(&cache)?;
    let cleanup = EmptyDirectory(cache.clone());
    let mut live = LiveRecording::with_history(HistoryConfig {
        directory: cache,
        ..Default::default()
    })?;
    let mut input = File::open(&args[0])?;
    let mut buffer = [0; 64 << 10];
    loop {
        let length = input.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        live.feed(&buffer[..length])?;
    }
    let source = live.finish()?;
    let output = PathBuf::from(&args[1]);
    let report = capture::write_retained_new(
        &source,
        &output,
        range,
        &CaptureLimits::default(),
        512 << 20,
        |_| true,
    )?;
    drop(source);
    drop(cleanup);
    let reopened = Recording::from_path(output)?;
    println!(
        "project_capture first={} last={} frames={} bytes={} resources={} blocks={} reopened_frames={}",
        range.first,
        range.last,
        report.frames,
        report.bytes,
        report.resource_count,
        report.block_count,
        reopened.frames.len()
    );
    Ok(())
}

struct EmptyDirectory(PathBuf);
impl Drop for EmptyDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}
