#[path = "support/analysis.rs"]
mod analysis_support;
use analysis_support::{edit_legacy_fixture, legacy_fixture};
use pvd_core::analysis::view::ProfileSource;
use pvd_core::{
    Recording,
    analysis_export::{
        AnalysisCsvKind, AnalysisExportPhase, AnalysisExportSummary, export_analysis_csv_streaming,
        write_analysis_csv,
    },
};
use std::{
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

fn expected(recording: &Recording, kind: AnalysisCsvKind) -> Vec<u8> {
    let mut output = Vec::new();
    let quote = |text: &str| format!("\"{}\"", text.replace('"', "\"\""));
    match kind {
        AnalysisCsvKind::Profile => {
            writeln!(
                output,
                "zone,event,thread,context,start_ticks,end_ticks,duration_ms,received_frame"
            )
            .unwrap();
            let ratio = recording.header.timestamp_numerator as f64
                / recording.header.timestamp_denominator as f64
                / 1_000_000.0;
            for span in &legacy_fixture(recording).spans {
                let zone = legacy_fixture(recording).zones.get(&span.zone);
                let event = zone
                    .and_then(|zone| zone.events.get(&span.event))
                    .cloned()
                    .unwrap_or_else(|| span.event.to_string());
                writeln!(
                    output,
                    "{},{},{},{},{},{},{:.9},{}",
                    quote(zone.map_or("", |zone| &zone.name)),
                    quote(&event),
                    span.thread,
                    span.context,
                    span.start,
                    span.end,
                    (span.end - span.start) as f64 * ratio,
                    span.received_frame
                )
                .unwrap();
            }
        }
        AnalysisCsvKind::Memory => {
            writeln!(
                output,
                "received_frame,live_bytes,live_allocations,total_allocations,total_frees"
            )
            .unwrap();
            for sample in &legacy_fixture(recording).memory.samples {
                writeln!(
                    output,
                    "{},{},{},{},{}",
                    sample.received_frame,
                    sample.live_bytes,
                    sample.live_allocations,
                    sample.allocations,
                    sample.frees
                )
                .unwrap();
            }
        }
    }
    output
}

fn verify(bytes: &[u8]) {
    let original = Recording::from_bytes(bytes).unwrap();
    for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
        let mut output = Vec::new();
        let mut phases = Vec::new();
        let report =
            export_analysis_csv_streaming(&mut Cursor::new(bytes), &mut output, kind, |progress| {
                phases.push(progress.phase);
                true
            })
            .unwrap();
        assert_eq!(output, expected(&original, kind));
        let mut loaded_output = Vec::new();
        let mut written = 0;
        let loaded = write_analysis_csv(&original, &mut loaded_output, kind, |progress| {
            assert!(
                progress.rows_written >= written && progress.rows_written <= progress.total_rows
            );
            written = progress.rows_written;
            true
        })
        .unwrap();
        assert_eq!(loaded_output, output);
        assert_eq!(loaded.exported_rows, report.exported_rows);
        assert_summary_eq(&loaded.summary, &report.summary);
        assert_eq!(written, report.exported_rows);
        assert_eq!(report.source_blake3, *blake3::hash(bytes).as_bytes());
        assert_eq!(report.source_bytes, bytes.len() as u64);
        assert_eq!(report.source_frames, original.frame_count());
        assert_eq!(
            report.summary.profile_spans,
            legacy_fixture(&original).profile_spans().len() as u64
        );
        assert_eq!(
            report.summary.profile_values,
            legacy_fixture(&original).values.len() as u64
        );
        assert_eq!(
            report.summary.open_spans,
            legacy_fixture(&original).open_spans()
        );
        assert_eq!(
            report.summary.unmatched_stop_events,
            legacy_fixture(&original).unmatched_stop_events
        );
        assert_eq!(
            report.summary.open_events,
            legacy_fixture(&original)
                .open_events()
                .map(|(key, count)| (*key, count))
                .collect()
        );
        assert_eq!(
            report.summary.memory_allocations,
            legacy_fixture(&original).memory.allocations
        );
        assert_eq!(
            report.summary.memory_frees,
            legacy_fixture(&original).memory.frees
        );
        assert_eq!(
            report.summary.unmatched_frees,
            legacy_fixture(&original).memory.unmatched_frees
        );
        assert_eq!(
            report.summary.replaced_addresses,
            legacy_fixture(&original).memory.replaced_addresses
        );
        assert_eq!(
            report.summary.live_bytes,
            legacy_fixture(&original).memory.live_bytes
        );
        assert_eq!(
            report.summary.live_allocations,
            legacy_fixture(&original).memory.live.len()
        );
        assert_eq!(phases[0], AnalysisExportPhase::Inspect);
        assert_eq!(*phases.last().unwrap(), AnalysisExportPhase::Export);
    }
}

fn assert_summary_eq(actual: &AnalysisExportSummary, expected: &AnalysisExportSummary) {
    assert_eq!(actual.profile_spans, expected.profile_spans);
    assert_eq!(actual.profile_values, expected.profile_values);
    assert_eq!(actual.legacy_cuda_buffers, expected.legacy_cuda_buffers);
    assert_eq!(actual.unmatched_stops, expected.unmatched_stops);
    assert_eq!(actual.open_spans, expected.open_spans);
    assert_eq!(actual.memory_allocations, expected.memory_allocations);
    assert_eq!(actual.memory_frees, expected.memory_frees);
    assert_eq!(actual.unmatched_frees, expected.unmatched_frees);
    assert_eq!(actual.replaced_addresses, expected.replaced_addresses);
    assert_eq!(actual.live_bytes, expected.live_bytes);
    assert_eq!(actual.live_allocations, expected.live_allocations);
    assert_eq!(actual.zones.len(), expected.zones.len());
    for (id, zone) in &expected.zones {
        let actual = &actual.zones[id];
        assert_eq!(actual.name, zone.name);
        assert_eq!(actual.events, zone.events);
    }
    assert_eq!(actual.unmatched_stop_events, expected.unmatched_stop_events);
    assert_eq!(actual.open_events, expected.open_events);
    assert_eq!(actual.ambiguous_zones, expected.ambiguous_zones);
    assert_eq!(actual.ambiguous_events, expected.ambiguous_events);
    assert_eq!(
        actual.duplicate_profile_names.len(),
        expected.duplicate_profile_names.len()
    );
    for (actual, expected) in actual
        .duplicate_profile_names
        .iter()
        .zip(&expected.duplicate_profile_names)
    {
        assert_eq!(actual.zone_name, expected.zone_name);
        assert_eq!(actual.event_name, expected.event_name);
        assert_eq!(actual.keys, expected.keys);
    }
}

fn group(events: &[Vec<u8>]) -> Vec<u8> {
    let data: Vec<_> = events.iter().flatten().copied().collect();
    let mut bytes = Vec::new();
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend((events.len() as u32).to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(100u64.to_le_bytes());
    bytes.extend(data);
    bytes
}
fn text(text: &str) -> Vec<u8> {
    let mut bytes = (text.len() as u32 + 1).to_le_bytes().to_vec();
    bytes.extend(text.as_bytes());
    bytes.push(0);
    bytes
}
fn string(id: u32, value: &str) -> Vec<u8> {
    let mut bytes = vec![1];
    bytes.extend(text(value));
    bytes.extend(id.to_le_bytes());
    bytes
}
fn instance(class: u32, id: u64) -> Vec<u8> {
    let mut bytes = vec![6];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(class.to_le_bytes());
    bytes.extend(id.to_le_bytes());
    bytes
}
fn frame(index: u64) -> Vec<u8> {
    let mut bytes = vec![19];
    bytes.extend(9u64.to_le_bytes());
    bytes.extend(6u32.to_le_bytes());
    bytes.extend(index.to_le_bytes());
    bytes
}
fn payload(id: u64, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![7];
    bytes.extend(id.to_le_bytes());
    for value in [4, 1, 5, data.len() as u32, data.len() as u32] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend(data);
    bytes
}
fn metadata(name: &str) -> Vec<Vec<u8>> {
    let mut zone = vec![24];
    zone.extend(10u64.to_le_bytes());
    zone.extend(text("zone,\"name"));
    let mut event = vec![25];
    event.extend(10u64.to_le_bytes());
    event.extend(text(name));
    event.extend(1u16.to_le_bytes());
    event.push(1);
    vec![zone, event]
}
fn prefix() -> Vec<u8> {
    let mut bytes = pvd_protocol::STREAM_MAGIC.to_le_bytes().to_vec();
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u64.to_le_bytes());
    bytes.extend(1u32.to_le_bytes());
    bytes.extend(group(&[
        string(1, "sdk"),
        string(2, "PxProfileZone"),
        string(3, "PxProfileMemoryEventBuffer"),
        string(4, "events"),
        string(5, "PvdU8"),
        string(6, "frame"),
        instance(2, 10),
        instance(3, 11),
    ]));
    bytes
}
fn profile(kind: u8, time: u64) -> Vec<u8> {
    let mut bytes = vec![kind, 3, 1, 0];
    if kind <= 2 {
        bytes.extend(7u32.to_le_bytes());
        bytes.extend([9, 0, 0]);
    }
    bytes.extend(time.to_le_bytes());
    bytes
}
fn memory_string(handle: u32, value: &str) -> Vec<u8> {
    let mut bytes = 1u16.to_le_bytes().to_vec();
    bytes.extend(text(value));
    bytes.extend(handle.to_le_bytes());
    bytes
}
fn memory_allocate(size: u8, address: u8) -> Vec<u8> {
    vec![2, 0, size, 1, 2, 13, address]
}

fn fixture() -> Vec<u8> {
    let mut bytes = prefix();
    let mut memory = memory_string(1, "type A");
    memory.extend(memory_string(2, "source.cpp"));
    memory.extend(memory_allocate(20, 88));
    bytes.extend(group(&[
        payload(10, &profile(1, 10)),
        payload(11, &memory),
        frame(0),
    ]));
    let mut nested = profile(1, 20);
    nested.extend(profile(2, 30));
    bytes.extend(group(&[
        payload(10, &nested),
        payload(11, &memory_allocate(30, 88)),
        frame(1),
    ]));
    let mut stops = profile(2, 40);
    stops.extend(profile(2, 50));
    stops.extend(profile(1, 60));
    let mut memory = vec![3, 0, 88, 3, 0, 99];
    memory.extend(memory_string(1, "type B"));
    memory.extend(memory_allocate(17, 88));
    bytes.extend(group(&[
        payload(10, &stops),
        payload(11, &memory),
        frame(2),
    ]));
    bytes.extend(group(&metadata("late,\"event\"\nname")));
    bytes
}

#[test]
fn nested_cross_buffer_spans_late_names_and_memory_reuse_match_full_analysis() {
    verify(&fixture());
}

#[test]
fn loaded_summary_keeps_empty_zones_and_final_metadata_without_event_rows() {
    let mut bytes = prefix();
    let mut zone = vec![24];
    zone.extend(55u64.to_le_bytes());
    zone.extend(text("empty zone"));
    let mut renamed = vec![24];
    renamed.extend(55u64.to_le_bytes());
    renamed.extend(text("empty renamed zone"));
    bytes.extend(group(&[zone, renamed]));
    verify(&bytes);
    let recording = Recording::from_bytes(&bytes).unwrap();
    let mut output = Vec::new();
    let report =
        write_analysis_csv(&recording, &mut output, AnalysisCsvKind::Profile, |_| true).unwrap();
    assert_eq!(report.exported_rows, 0);
    assert_eq!(report.summary.ambiguous_zones, [55]);
    assert_eq!(report.summary.zones[&55].name, "empty renamed zone");
    assert!(report.summary.zones[&55].events.is_empty());
    assert_eq!(output.iter().filter(|&&byte| byte == b'\n').count(), 1);
}

#[test]
fn loaded_writer_honors_each_progress_cancellation_boundary() {
    let mut bytes = fixture();
    for index in 0..300 {
        let mut span = profile(1, 100 + index * 2);
        span.extend(profile(2, 101 + index * 2));
        bytes.extend(group(&[
            payload(10, &span),
            payload(11, &memory_allocate(1, 88)),
            frame(index + 3),
        ]));
    }
    let recording = Recording::from_bytes(&bytes).unwrap();
    for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
        let mut statuses = Vec::new();
        write_analysis_csv(&recording, &mut io::sink(), kind, |progress| {
            statuses.push(progress);
            true
        })
        .unwrap();
        assert_eq!(statuses.len(), 5);
        assert_eq!(statuses[3].rows_written, 256);
        assert_eq!(statuses[4].rows_written, statuses[4].total_rows);
        for rejected in 0..statuses.len() {
            let mut calls = 0;
            let mut output = Vec::new();
            let error = write_analysis_csv(&recording, &mut output, kind, |_| {
                let accepted = calls != rejected;
                calls += 1;
                accepted
            })
            .unwrap_err();
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::Interrupted
            );
            assert_eq!(calls, rejected + 1);
            if rejected < 2 {
                assert!(output.is_empty());
            }
        }
    }
}

#[test]
fn loaded_writer_rejects_incomplete_analysis_and_preserves_import_diagnostics() {
    let original = Recording::from_bytes(&fixture()).unwrap();
    for detail in [
        "Analysis decoding stopped at frame 2: unsupported event",
        "PVD3 initial snapshot restored 2 saved instances; analysis contains only subsequent history events.",
    ] {
        let mut recording = original.clone();
        recording.diagnostics.push(detail.into());
        let mut output = Vec::new();
        assert!(
            write_analysis_csv(&recording, &mut output, AnalysisCsvKind::Profile, |_| true)
                .is_err()
        );
        assert!(output.is_empty());
    }
    let mut recording = original.clone();
    edit_legacy_fixture(&mut recording, |analysis| analysis.first_retained_frame = 1);
    assert!(
        write_analysis_csv(&recording, &mut Vec::new(), AnalysisCsvKind::Memory, |_| {
            true
        })
        .is_err()
    );
    let mut recording = original;
    recording
        .diagnostics
        .push("PVD3 compatibility limitation: object IDs are container-local".into());
    let mut output = Vec::new();
    write_analysis_csv(&recording, &mut output, AnalysisCsvKind::Profile, |_| true).unwrap();
    assert_eq!(output, expected(&recording, AnalysisCsvKind::Profile));
}

#[test]
fn loaded_writer_cancellation_and_write_errors_propagate() {
    let recording = Recording::from_bytes(&fixture()).unwrap();
    for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
        let mut output = Vec::new();
        let error = write_analysis_csv(&recording, &mut output, kind, |progress| {
            progress.rows_written == 0
        })
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!output.is_empty());
        let error = write_analysis_csv(
            &recording,
            &mut io::Cursor::new(&mut [0u8; 8][..]),
            kind,
            |_| true,
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::WriteZero
        );
    }
}

#[test]
#[ignore = "requires original PVD3 baseline and rich containers"]
fn loaded_native_csv_matches_existing_complete_exports() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures/native");
    for name in ["baseline-paired-native.pxd2", "rich-paired-native.pxd2"] {
        let recording = Recording::from_path(root.join(name)).unwrap();
        for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
            let mut output = Vec::new();
            let report = write_analysis_csv(&recording, &mut output, kind, |_| true).unwrap();
            assert_eq!(output, expected(&recording, kind));
            eprintln!(
                "native {name} {kind:?}: rows={} bytes={}",
                report.exported_rows,
                output.len()
            );
        }
    }
}

#[test]
fn ambiguous_names_and_unsupported_buffers_are_explicit() {
    let mut bytes = fixture();
    bytes.extend(group(&metadata("renamed event")));
    let mut zone = vec![24];
    zone.extend(10u64.to_le_bytes());
    zone.extend(text("renamed zone"));
    bytes.extend(group(&[zone]));
    verify(&bytes);
    let report = export_analysis_csv_streaming(
        &mut Cursor::new(bytes),
        &mut io::sink(),
        AnalysisCsvKind::Profile,
        |_| true,
    )
    .unwrap();
    assert_eq!(report.summary.ambiguous_zones, [10]);
    assert_eq!(report.summary.ambiguous_events, [(10, 1)]);
    for (id, data, diagnostic) in [
        (10, vec![255, 0, 1, 0], "unknown profile event"),
        (11, vec![4, 0], "unsupported memory event"),
    ] {
        let mut bytes = fixture();
        bytes.extend(group(&[payload(id, &data)]));
        let mut output = Vec::new();
        let error = export_analysis_csv_streaming(
            &mut Cursor::new(bytes),
            &mut output,
            AnalysisCsvKind::Profile,
            |_| true,
        )
        .unwrap_err();
        assert!(error.to_string().contains(diagnostic), "{error}");
        assert!(output.is_empty());
    }
}

#[test]
fn duplicate_profile_names_retain_exact_numeric_keys() {
    let mut bytes = fixture();
    let name = "late,\"event\"\nname";
    let mut alias = metadata(name).pop().unwrap();
    let length = alias.len();
    alias[length - 3..length - 1].copy_from_slice(&2u16.to_le_bytes());
    bytes.extend(group(&[alias]));
    let mut other_zone = metadata(name);
    for event in &mut other_zone {
        event[1..9].copy_from_slice(&12u64.to_le_bytes());
    }
    bytes.extend(group(&other_zone));
    verify(&bytes);
    let report = export_analysis_csv_streaming(
        &mut Cursor::new(bytes),
        &mut io::sink(),
        AnalysisCsvKind::Profile,
        |_| true,
    )
    .unwrap();
    assert!(report.summary.ambiguous_zones.is_empty());
    assert!(report.summary.ambiguous_events.is_empty());
    let collisions = report.summary.duplicate_profile_names;
    assert_eq!(collisions.len(), 1);
    assert_eq!(collisions[0].zone_name.as_ref(), "zone,\"name");
    assert_eq!(collisions[0].event_name.as_ref(), name);
    assert_eq!(collisions[0].keys, [(10, 1), (10, 2), (12, 1)]);
}

#[test]
fn values_and_legacy_cuda_buffers_are_counted_without_fabricating_span_rows() {
    let mut bytes = fixture();
    let mut data = vec![5, 0, 2, 0, 42, 9, 7, 0, 0, 0];
    data.extend([6, 0, 3, 0]);
    data.extend([0; 12]);
    data.extend(3u32.to_le_bytes());
    data.extend([1, 2, 3]);
    data.extend([0; 8]);
    bytes.extend(group(&[payload(10, &data)]));
    verify(&bytes);
    let report = export_analysis_csv_streaming(
        &mut Cursor::new(bytes),
        &mut io::sink(),
        AnalysisCsvKind::Profile,
        |_| true,
    )
    .unwrap();
    assert_eq!(report.summary.profile_values, 1);
    assert_eq!(report.summary.legacy_cuda_buffers, 1);
    assert_eq!(report.exported_rows, 2);
}

#[test]
fn transaction_and_property_message_buffers_keep_commit_order() {
    use pvd_protocol::{Decoder, Endian, Event, rewrite::map_group_events};
    let source = fixture();
    for mode in 0..3 {
        let mut output = source[..28].to_vec();
        output.extend(group(&[string(100, "analysis_message")]));
        let mut decoder = Decoder::default();
        for decoded in decoder.feed(&source).unwrap() {
            let offset = decoded.offset as usize;
            let length =
                u32::from_le_bytes(source[offset..offset + 4].try_into().unwrap()) as usize;
            map_group_events(
                &source[offset..offset + 24 + length],
                Endian::Little,
                |event, raw| {
                    if let Event::SetPropertyValue {
                        id,
                        property,
                        datatype,
                        data,
                        ..
                    } = event
                    {
                        if mode == 0 {
                            let mut begin = vec![8];
                            begin.extend(id.to_le_bytes());
                            for value in [*property, datatype.namespace, datatype.name] {
                                begin.extend(value.to_le_bytes())
                            }
                            output.extend(group(&[begin]));
                            for chunk in data.chunks(7) {
                                let mut append = vec![9];
                                append.extend((chunk.len() as u32).to_le_bytes());
                                append.extend(chunk);
                                append.extend((chunk.len() as u32).to_le_bytes());
                                output.extend(group(&[append]));
                            }
                            output.extend(group(&[vec![10]]));
                        } else {
                            let mut definition = vec![5];
                            for value in [
                                1u32,
                                if *id == 10 { 2 } else { 3 },
                                1,
                                100,
                                1,
                                *property,
                                datatype.namespace,
                                datatype.name,
                                0,
                                data.len() as u32,
                                data.len() as u32,
                            ] {
                                definition.extend(value.to_le_bytes())
                            }
                            output.extend(group(&[definition]));
                            if mode == 1 {
                                let mut message = vec![11];
                                message.extend(id.to_le_bytes());
                                message.extend(1u32.to_le_bytes());
                                message.extend(100u32.to_le_bytes());
                                message.extend((data.len() as u32).to_le_bytes());
                                message.extend(data.as_ref());
                                output.extend(group(&[message]));
                            } else {
                                let mut begin = vec![12];
                                begin.extend(1u32.to_le_bytes());
                                begin.extend(100u32.to_le_bytes());
                                let mut message = vec![13];
                                message.extend(id.to_le_bytes());
                                message.extend((data.len() as u32).to_le_bytes());
                                message.extend(data.as_ref());
                                output.extend(group(&[begin, message, vec![14]]));
                            }
                        }
                    } else {
                        output.extend(group(&[raw.to_vec()]));
                    }
                    Ok(None)
                },
            )
            .unwrap();
        }
        verify(&output);
    }
    let mut bytes = prefix();
    let mut begin = vec![8];
    begin.extend(10u64.to_le_bytes());
    for value in [4u32, 1, 5] {
        begin.extend(value.to_le_bytes())
    }
    let data = profile(1, 10);
    let mut append = vec![9];
    append.extend((data.len() as u32).to_le_bytes());
    append.extend(&data);
    append.extend((data.len() as u32).to_le_bytes());
    bytes.extend(group(&[
        begin,
        append,
        frame(0),
        vec![10],
        payload(10, &profile(2, 40)),
        frame(1),
    ]));
    verify(&bytes);
}

struct Repeated {
    prefix: Vec<u8>,
    block: Vec<u8>,
    repeats: u64,
    position: u64,
    max_read: usize,
}
impl Repeated {
    fn length(&self) -> u64 {
        self.prefix.len() as u64 + self.block.len() as u64 * self.repeats
    }
}
impl Read for Repeated {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.max_read = self.max_read.max(output.len());
        assert!(output.len() <= 64 << 10);
        if self.position == self.length() {
            return Ok(0);
        }
        let bytes = if self.position < self.prefix.len() as u64 {
            &self.prefix[self.position as usize..]
        } else {
            let index = (self.position - self.prefix.len() as u64) % self.block.len() as u64;
            &self.block[index as usize..]
        };
        let count = bytes.len().min(output.len());
        output[..count].copy_from_slice(&bytes[..count]);
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for Repeated {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        let next = match p {
            SeekFrom::Start(v) => i128::from(v),
            SeekFrom::End(v) => i128::from(self.length()) + i128::from(v),
            SeekFrom::Current(v) => i128::from(self.position) + i128::from(v),
        };
        if !(0..=i128::from(self.length())).contains(&next) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        self.position = next as u64;
        Ok(self.position)
    }
}

#[test]
#[ignore = "explicit synthetic input exceeds 512 MiB without materializing the capture"]
fn input_over_512_mib_exports_every_sample_and_span_with_bounded_reads() {
    let mut prefix = prefix();
    prefix.extend(group(&metadata("streamed event")));
    prefix.extend(group(&[
        string(7, "OpaqueOwner"),
        string(8, "blob"),
        string(9, "OpaqueData"),
        instance(7, 12),
    ]));
    let mut opaque = vec![7];
    opaque.extend(12u64.to_le_bytes());
    for value in [8u32, 1, 9, 1, 1 << 20] {
        opaque.extend(value.to_le_bytes())
    }
    opaque.resize(opaque.len() + (1 << 20), 42);
    let mut span = profile(1, 10);
    span.extend(profile(2, 20));
    let block = group(&[
        opaque,
        payload(10, &span),
        payload(11, &[3, 0, 99]),
        frame(0),
    ]);
    let mut source = Repeated {
        prefix,
        block,
        repeats: 513,
        position: 0,
        max_read: 0,
    };
    assert!(source.length() > 512 << 20);
    for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
        let report =
            export_analysis_csv_streaming(&mut source, &mut io::sink(), kind, |_| true).unwrap();
        assert_eq!(report.source_frames, 513);
        assert_eq!(report.exported_rows, 513);
        assert_eq!(report.summary.profile_spans, 513);
        assert_eq!(report.summary.memory_frees, 513);
        assert_eq!(source.max_read, 64 << 10);
        assert!(source.prefix.len() + source.block.len() < 2 << 20);
        eprintln!(
            "{kind:?}: source_bytes={} rows={} input_template={} max_read={}",
            report.source_bytes,
            report.exported_rows,
            source.prefix.len() + source.block.len(),
            source.max_read
        );
    }
}

#[test]
fn cancellation_writer_failures_and_invalid_tail_propagate() {
    let bytes = fixture();
    for phase in [AnalysisExportPhase::Inspect, AnalysisExportPhase::Export] {
        let mut output = Vec::new();
        let error = export_analysis_csv_streaming(
            &mut Cursor::new(&bytes),
            &mut output,
            AnalysisCsvKind::Profile,
            |state| !(state.phase == phase && state.bytes_read > 0),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::Interrupted
        );
        if phase == AnalysisExportPhase::Inspect {
            assert!(output.is_empty());
        }
    }
    let mut output = Vec::new();
    assert!(
        export_analysis_csv_streaming(
            &mut Cursor::new(&bytes[..bytes.len() - 1]),
            &mut output,
            AnalysisCsvKind::Memory,
            |_| true
        )
        .is_err()
    );
    assert!(output.is_empty());
    struct Failed;
    impl Write for Failed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let error = export_analysis_csv_streaming(
        &mut Cursor::new(bytes),
        &mut Failed,
        AnalysisCsvKind::Memory,
        |_| true,
    )
    .unwrap_err();
    assert_eq!(
        error.downcast_ref::<io::Error>().unwrap().kind(),
        io::ErrorKind::StorageFull
    );
}

struct Changing {
    source: Cursor<Vec<u8>>,
    seeks: usize,
}
impl Read for Changing {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.source.read(b)
    }
}
impl Seek for Changing {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        if p == SeekFrom::Start(0) {
            self.seeks += 1;
            if self.seeks == 2 {
                let index = self
                    .source
                    .get_ref()
                    .windows(6)
                    .position(|bytes| bytes == b"type A")
                    .unwrap();
                self.source.get_mut()[index + 5] = b'X';
            }
        }
        self.source.seek(p)
    }
}
#[test]
fn source_identity_change_rejects_staged_csv() {
    let mut source = Changing {
        source: Cursor::new(fixture()),
        seeks: 0,
    };
    let error = export_analysis_csv_streaming(
        &mut source,
        &mut Vec::new(),
        AnalysisCsvKind::Memory,
        |_| true,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("changed between analysis export scans"),
        "{error}"
    );
}

#[test]
#[ignore = "requires official SDK 3.4 complete captures"]
fn real_sdk_full_csv_matches_existing_profile_and_memory_exports() {
    let root = std::env::var_os("PVD_ANALYSIS_EXPORT_CAPTURE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../captures"));
    for name in [
        "physx-3.4-all.pvdstream",
        "physx-3.4-profile-events.pvdstream",
        "physx-3.4-profile-metrics.pvdstream",
        "physx-3.4-rich.pvdstream",
    ] {
        let path = root.join(name);
        let bytes = std::fs::read(&path).unwrap();
        verify(&bytes);
        let mut source = File::open(path).unwrap();
        let report = export_analysis_csv_streaming(
            &mut source,
            &mut io::sink(),
            AnalysisCsvKind::Profile,
            |_| true,
        )
        .unwrap();
        eprintln!(
            "{name}: frames={} profile_spans={} profile_values={} memory_allocations={} memory_frees={}",
            report.source_frames,
            report.summary.profile_spans,
            report.summary.profile_values,
            report.summary.memory_allocations,
            report.summary.memory_frees
        );
    }
}

#[test]
#[ignore = "explicit synthetic resource test has over two million embedded profile and memory events in single buffers"]
fn single_buffers_over_two_million_records_export_without_window_truncation() {
    let spans = 2_000_001u64;
    let mut profile = vec![1, 0, 1, 0, 7, 0, 0, 0, 9, 0, 0, 0, 4, 0, 1, 0, 1];
    for _ in 1..spans {
        profile.extend([3, 0, 1, 0, 1, 4, 0, 1, 0, 1]);
    }
    let memory = [3, 0, 99].repeat(spans as usize);
    let mut bytes = prefix();
    bytes.extend(group(&[
        payload(10, &profile),
        payload(11, &memory),
        frame(0),
    ]));
    bytes.extend(group(&metadata("many spans")));
    for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
        let report =
            export_analysis_csv_streaming(&mut Cursor::new(&bytes), &mut io::sink(), kind, |_| {
                true
            })
            .unwrap();
        assert_eq!(report.summary.profile_spans, spans);
        assert_eq!(report.summary.memory_frees, spans);
        assert_eq!(report.summary.unmatched_frees, spans);
        assert_eq!(
            report.exported_rows,
            if kind == AnalysisCsvKind::Profile {
                spans
            } else {
                1
            }
        );
        eprintln!(
            "{kind:?}: rows={} spans={} frees={} source={}",
            report.exported_rows,
            report.summary.profile_spans,
            report.summary.memory_frees,
            bytes.len()
        );
    }
}
