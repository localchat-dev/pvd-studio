use std::io::{self, Write};

use pvd_core::{
    MemoryHistoryScope, NativeMemoryTotal, NativeMemoryTotals, Recording, RecordingSource,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Position {
    Initial,
    End,
    Frame(usize),
}

impl Position {
    fn parse(value: Option<&str>) -> crate::Result<Self> {
        match value {
            None | Some("end") => Ok(Self::End),
            Some("initial") => Ok(Self::Initial),
            Some(value) => Ok(Self::Frame(value.parse()?)),
        }
    }
}

pub(crate) fn run(path: &str, position: Option<&str>) -> crate::Result<()> {
    let position = Position::parse(position)?;
    let cancel = crate::cancellation::Cancellation::install()?;
    let recording = Recording::from_path_with_progress(path, |_, _| !cancel.requested())?;
    if recording.source != RecordingSource::Pvd3Database {
        return Err("memory-totals requires a PVD3 database capture".into());
    }
    let totals = match position {
        Position::Initial => recording.native_initial_memory_totals()?,
        Position::End => recording.native_end_memory_totals()?,
        Position::Frame(frame) => {
            let state = recording.state_at_with_cancel(frame, || cancel.requested())?;
            recording.native_memory_totals_at_with_cancel(&state, || cancel.requested())?
        }
    };
    write_totals(
        &mut io::stdout().lock(),
        position,
        recording.memory_history_scope(),
        totals.as_ref(),
        || !cancel.requested(),
    )?;
    crate::report_diagnostics(&recording);
    Ok(())
}

fn write_totals(
    output: &mut impl Write,
    position: Position,
    scope: MemoryHistoryScope,
    totals: Option<&NativeMemoryTotals>,
    mut proceed: impl FnMut() -> bool,
) -> io::Result<()> {
    if !proceed() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "memory totals cancelled",
        ));
    }
    writeln!(
        output,
        "position: {}",
        match position {
            Position::Initial => "initial-snapshot",
            Position::End => "capture-end",
            Position::Frame(_) => "frame-state",
        }
    )?;
    if let Position::Frame(frame) = position {
        writeln!(output, "frame: {frame}")?;
    }
    writeln!(
        output,
        "event_scope: {}",
        match scope {
            MemoryHistoryScope::CapturedEvents => "captured-events",
            MemoryHistoryScope::AfterInitialSnapshot => "after-initial-snapshot",
        }
    )?;
    let Some(totals) = totals else {
        return writeln!(output, "captured: false");
    };
    writeln!(output, "captured: true\nsource_rows: {}", totals.len())?;
    match totals.nonnegative_bytes() {
        Some(bytes) => writeln!(output, "captured_nonnegative_bytes: {bytes}")?,
        None => writeln!(output, "captured_nonnegative_bytes: unavailable")?,
    }
    for index in 0..totals.len() {
        if index.is_multiple_of(256) && !proceed() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "memory totals cancelled",
            ));
        }
        let row = totals
            .row(index)
            .ok_or_else(|| io::Error::other("validated memory source row is unavailable"))?;
        write_row(output, row)?;
    }
    Ok(())
}

fn write_row(output: &mut impl Write, row: NativeMemoryTotal<'_>) -> io::Result<()> {
    writeln!(
        output,
        "source:\n  index: {}\n  type: {:?}\n  file: {:?}\n  line: {}\n  captured_bytes: {}",
        row.index, row.type_name, row.file, row.line, row.bytes
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_bytes_keep_signed_bounds_and_escape_captured_paths() {
        let mut output = Vec::new();
        write_row(
            &mut output,
            NativeMemoryTotal {
                index: 7,
                type_name: "Block\nPool",
                file: "C:\\capture\\source.cpp",
                line: 42,
                bytes: i32::MIN,
            },
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("index: 7\n"));
        assert!(output.contains("type: \"Block\\nPool\""));
        assert!(output.contains("file: \"C:\\\\capture\\\\source.cpp\""));
        assert!(output.contains("captured_bytes: -2147483648"));
        assert!(!output.contains("live_allocations") && !output.contains("received_frame"));
    }

    #[test]
    fn snapshot_and_end_reports_do_not_add_frame_or_timestamp_positions() {
        for position in [Position::Initial, Position::End] {
            let mut output = Vec::new();
            write_totals(
                &mut output,
                position,
                MemoryHistoryScope::AfterInitialSnapshot,
                None,
                || true,
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert!(output.contains("event_scope: after-initial-snapshot"));
            assert!(output.contains("captured: false"));
            assert!(!output.contains("frame:") && !output.contains("timestamp"));
        }
        assert_eq!(Position::parse(None).unwrap(), Position::End);
        assert_eq!(Position::parse(Some("12")).unwrap(), Position::Frame(12));
        assert!(Position::parse(Some("-1")).is_err());
    }

    #[test]
    fn cancelled_source_report_writes_no_header() {
        let mut output = Vec::new();
        let failure = write_totals(
            &mut output,
            Position::Initial,
            MemoryHistoryScope::CapturedEvents,
            None,
            || false,
        )
        .unwrap_err();
        assert_eq!(failure.kind(), io::ErrorKind::Interrupted);
        assert!(output.is_empty());
    }

    #[test]
    #[ignore = "requires PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY constructed fixtures"]
    fn constructed_native_memory_totals_reports_keep_initial_frame_and_end_values_separate() {
        let directory = std::path::PathBuf::from(
            std::env::var_os("PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY")
                .expect("constructed memory fixture directory"),
        );
        let recording = Recording::from_bytes(
            &std::fs::read(directory.join("memory-initial-frames.pxd2")).unwrap(),
        )
        .unwrap();
        for (position, expected) in [
            (Position::Initial, vec![64, 16]),
            (Position::Frame(0), vec![32, 16]),
            (Position::Frame(1), vec![20, 8, 4]),
            (Position::End, vec![5, 6, 7]),
        ] {
            let totals = match position {
                Position::Initial => recording.native_initial_memory_totals().unwrap(),
                Position::End => recording.native_end_memory_totals().unwrap(),
                Position::Frame(frame) => recording
                    .native_memory_totals_at(&recording.state_at(frame).unwrap())
                    .unwrap(),
            };
            let mut output = Vec::new();
            write_totals(
                &mut output,
                position,
                recording.memory_history_scope(),
                totals.as_ref(),
                || true,
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            let bytes: Vec<i32> = output
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("  captured_bytes: ")
                        .map(|value| value.parse().unwrap())
                })
                .collect();
            assert_eq!(bytes, expected, "{output}");
            assert!(output.contains("event_scope: after-initial-snapshot"));
            assert_eq!(
                output.contains("\nframe: "),
                matches!(position, Position::Frame(_))
            );
            assert!(
                !output.contains("address")
                    && !output.contains("received")
                    && !output.contains("timestamp")
            );
        }
        let zero = Recording::from_bytes(
            &std::fs::read(directory.join("memory-initial-zero.pxd2")).unwrap(),
        )
        .unwrap();
        assert_eq!(zero.frame_count(), 0);
        let mut output = Vec::new();
        write_totals(
            &mut output,
            Position::Initial,
            zero.memory_history_scope(),
            zero.native_initial_memory_totals().unwrap().as_ref(),
            || true,
        )
        .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("captured_nonnegative_bytes: 80")
        );
    }
}
