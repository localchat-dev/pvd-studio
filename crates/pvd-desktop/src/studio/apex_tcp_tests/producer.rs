use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
};

pub(super) struct Process {
    child: Child,
}

impl Process {
    pub(super) fn spawn(command: &mut Command) -> Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        Self {
            child: command.spawn().expect("start validation process"),
        }
    }

    pub(super) fn status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().expect("poll validation process")
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

pub(super) struct Producer {
    process: Process,
    input: ChildStdin,
    pub(super) progress: Receiver<String>,
    reader: Option<JoinHandle<io::Result<()>>>,
}

impl Producer {
    pub(super) fn start(
        executable: &Path,
        asset: &Path,
        directory: &Path,
        port: u16,
        asynchronous: bool,
    ) -> Self {
        Self::start_cycles(executable, asset, directory, port, asynchronous, 1)
    }

    pub(super) fn start_cycles(
        executable: &Path,
        asset: &Path,
        directory: &Path,
        port: u16,
        asynchronous: bool,
        cycles: usize,
    ) -> Self {
        assert!((1..=32).contains(&cycles));
        let mut command = Command::new(executable);
        command
            .args(["--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .arg(directory.join("truth.json"))
            .arg(asset)
            .args(["--debug-only", "--step-control", "--cycles"])
            .arg(cycles.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(directory.join("sdk.stderr.log")).unwrap());
        if asynchronous {
            command.arg("--async-fetch");
        }
        let mut process = Process::spawn(&mut command);
        let input = process.child.stdin.take().unwrap();
        let output = process.child.stdout.take().unwrap();
        let mut log = File::create(directory.join("sdk.stdout.log")).unwrap();
        let (sender, progress) = mpsc::sync_channel(64);
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let line = line?;
                writeln!(log, "{line}")?;
                if line.starts_with("PVD_APEX_") {
                    if line.len() > 256 {
                        return Err(io::Error::other("oversized producer control message"));
                    }
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
            Ok(())
        });
        Self {
            process,
            input,
            progress,
            reader: Some(reader),
        }
    }

    pub(super) fn command(&mut self, command: &str) {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
    }

    pub(super) fn status(&mut self) -> Option<ExitStatus> {
        self.process.status()
    }

    pub(super) fn join_output(&mut self) {
        self.reader.take().unwrap().join().unwrap().unwrap();
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        if self.process.status().is_none() {
            let _ = self.process.child.kill();
        }
        let _ = self.process.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
