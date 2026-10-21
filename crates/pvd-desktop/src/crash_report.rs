use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(directory) = std::env::var_os("LOCALAPPDATA") {
            let directory = PathBuf::from(directory)
                .join("pvd-studio")
                .join("diagnostics");
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = directory.join(format!("panic-{timestamp}-{}.log", std::process::id()));
            if std::fs::create_dir_all(&directory).is_ok()
                && let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(path)
            {
                let _ = writeln!(file, "pvd-studio {}", env!("CARGO_PKG_VERSION"));
                let _ = writeln!(file, "{info}");
                let _ = writeln!(file, "{}", std::backtrace::Backtrace::force_capture());
                let _ = file.sync_all();
            }
        }
        previous(info);
    }));
}
