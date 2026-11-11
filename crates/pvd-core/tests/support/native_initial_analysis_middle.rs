use super::*;
use pvd_core::{InitialUntimedError, MemoryHistoryScope, NativeMemoryTotals, SceneState};
use std::collections::BTreeSet;

#[path = "native_initial_analysis_prefix.rs"]
mod prefix_tests;

type Key = (u16, u32, u64);
type SpanRow = (u64, u16, u32, u64, u64, u64, usize);
type ValueRow = (u64, u16, u32, u64, u64, usize);

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn take(&mut self, count: usize) -> &'a [u8] {
        let end = self.pos.checked_add(count).unwrap();
        let result = &self.bytes[self.pos..end];
        self.pos = end;
        result
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take(4).try_into().unwrap())
    }
    fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.take(8).try_into().unwrap())
    }
    fn blob(&mut self) -> &'a [u8] {
        let count = self.u32() as usize;
        self.take(count)
    }
    fn end(&self) {
        assert_eq!(self.pos, self.bytes.len());
    }
}

struct Set<'a> {
    id: u32,
    property: u32,
    data: &'a [u8],
    undo: Vec<u32>,
}

fn set(bytes: &[u8]) -> Option<Set<'_>> {
    let mut reader = Reader::new(bytes);
    if reader.u32() != 3 {
        return None;
    }
    let id = reader.u32();
    let property = reader.u32();
    let data = reader.blob();
    let undo = (0..reader.u32()).map(|_| reader.u32()).collect();
    reader.end();
    Some(Set {
        id,
        property,
        data,
        undo,
    })
}

struct Raw<'a> {
    events: Vec<&'a [u8]>,
    initial: BTreeMap<u32, (u32, &'a [u8])>,
    frames: Vec<(usize, usize)>,
}

impl<'a> Raw<'a> {
    fn parse(bytes: &'a [u8]) -> Self {
        let mut reader = Reader::new(bytes);
        assert_eq!(reader.u32(), 0x36101bb0);
        assert_eq!(reader.u32(), 1);
        let objects = reader.u32() as usize;
        let communication = reader.u32() as usize;
        reader.take(objects + communication);
        reader.pos = (reader.pos / 4096 + 1) * 4096;
        let count = reader.u64() as usize;
        let pages = (0..reader.u32())
            .map(|_| {
                (
                    reader.u64() as usize,
                    reader.u64() as usize,
                    reader.u32() as usize,
                    reader.u32() as usize,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(reader.u32(), 1);
        assert_eq!(reader.u64(), 0);
        let snapshot_position = reader.u64() as usize;
        let snapshot_length = reader.u32() as usize;
        assert_eq!(reader.u32(), 492);
        let deleted = reader.u32() as usize;
        reader.take(deleted * 16);
        let base = (reader.pos / 4096 + 1) * 4096;
        let mut events = Vec::new();
        for (first, position, count, length) in pages {
            assert_eq!(first, events.len());
            let mut page = Reader::new(&bytes[base + position..base + position + length]);
            assert_eq!(page.u32() as usize, count);
            let offsets = (0..count).map(|_| page.u32() as usize).collect::<Vec<_>>();
            let payload = page.take(page.bytes.len() - page.pos);
            for (index, &start) in offsets.iter().enumerate() {
                events.push(
                    &payload[start..offsets.get(index + 1).copied().unwrap_or(payload.len())],
                );
            }
        }
        assert_eq!(events.len(), count);
        let mut directory = Reader::new(
            &bytes[base + snapshot_position..base + snapshot_position + snapshot_length],
        );
        let mut initial = BTreeMap::new();
        for _ in 0..directory.u32() {
            let id = directory.u32();
            let position = directory.u64() as usize;
            let mut saved = Reader::new(&bytes[base + position..]);
            let class = saved.u32();
            assert_eq!(saved.u32(), id);
            let body = saved.blob();
            assert!(initial.insert(id, (class, body)).is_none());
        }
        directory.end();
        let mut frames = Vec::new();
        let mut begin = None;
        for (ordinal, event) in events.iter().enumerate() {
            let mut event = Reader::new(event);
            if event.u32() != 8 {
                continue;
            }
            event.u32();
            let mut section = Reader::new(event.blob());
            section.u64();
            section.u64();
            section.u32();
            let start = section.take(1)[0];
            section.take(3);
            section.end();
            event.end();
            if start == 1 {
                assert!(begin.replace(ordinal).is_none());
            } else {
                assert_eq!(start, 0);
                frames.push((begin.take().unwrap(), ordinal));
            }
        }
        assert!(begin.is_none());
        Self {
            events,
            initial,
            frames,
        }
    }

    fn array(&self, id: u32) -> (&'a [u8], &'a [u8]) {
        let mut reader = Reader::new(self.initial[&id].1);
        let fixed = reader.blob();
        let array = reader.blob();
        reader.end();
        (fixed, array)
    }

    fn latest(&self, id: u32, property: u32, cut: usize) -> &'a [u8] {
        self.events[..cut]
            .iter()
            .filter_map(|&event| set(event))
            .filter(|event| event.id == id && event.property == property)
            .map(|event| event.data)
            .next_back()
            .unwrap_or_else(|| self.array(id).1)
    }

    fn alive(&self, cut: usize) -> BTreeSet<u32> {
        let mut result = self.initial.keys().copied().collect::<BTreeSet<_>>();
        for event in &self.events[..cut] {
            let mut event = Reader::new(event);
            match event.u32() {
                1 => {
                    assert!(result.insert(event.u32()));
                }
                2 => {
                    assert!(result.remove(&event.u32()));
                }
                _ => {}
            }
        }
        result
    }

    fn profile(&self, cut: usize) -> Profile {
        assert!(self.array(2).1.is_empty());
        let mut result = Profile::default();
        for event in &self.events[..cut] {
            if let Some(event) = set(event)
                && event.id == 2
                && event.property == 50
            {
                result.ingest(event.data);
            }
        }
        result
    }
}

#[derive(Clone)]
struct Start {
    tick: u64,
    priority: u8,
    cpu: u8,
}

#[derive(Default)]
struct Profile {
    stacks: BTreeMap<Key, Vec<Start>>,
    unmatched: BTreeMap<Key, u64>,
}

impl Profile {
    fn ingest(&mut self, bytes: &[u8]) {
        let mut reader = Reader::new(bytes);
        let mut context = None;
        let mut tick = 0u64;
        while reader.pos < bytes.len() {
            let kind = reader.take(1)[0];
            let options = reader.take(1)[0];
            let event = u16::from_le_bytes(reader.take(2).try_into().unwrap());
            let time_width = 1 << (options & 3);
            let context_width = 1 << ((options >> 2) & 3);
            let integer = |reader: &mut Reader<'_>, width| {
                let mut value = [0u8; 8];
                value[..width].copy_from_slice(reader.take(width));
                u64::from_le_bytes(value)
            };
            match kind {
                1..=4 => {
                    if kind <= 2 {
                        context = Some((
                            reader.u32(),
                            integer(&mut reader, context_width),
                            reader.take(1)[0],
                            reader.take(1)[0],
                        ));
                    }
                    let (thread, context, priority, cpu) = context.unwrap();
                    let value = integer(&mut reader, time_width);
                    tick = if time_width == 8 {
                        value
                    } else {
                        tick.checked_add(value).unwrap()
                    };
                    let key = (event, thread, context);
                    if kind == 1 || kind == 3 {
                        self.stacks.entry(key).or_default().push(Start {
                            tick,
                            priority,
                            cpu,
                        });
                    } else if let Some(starts) = self.stacks.get_mut(&key) {
                        assert!(starts.pop().unwrap().tick <= tick);
                        if starts.is_empty() {
                            self.stacks.remove(&key);
                        }
                    } else {
                        *self.unmatched.entry(key).or_default() += 1;
                    }
                }
                5 => {
                    reader.take(time_width + context_width + 4);
                }
                6 => {
                    reader.take(12);
                    let count = reader.u32() as usize;
                    reader.take(count + 8);
                }
                _ => panic!("unexpected source profile opcode {kind}"),
            }
        }
    }

    fn seed(&self) -> Vec<u8> {
        let mut output = Vec::new();
        for (&(event, thread, context), starts) in &self.stacks {
            for start in starts {
                output.extend([1, 15]);
                output.extend(event.to_le_bytes());
                output.extend(thread.to_le_bytes());
                output.extend(context.to_le_bytes());
                output.extend([start.priority, start.cpu]);
                output.extend(start.tick.to_le_bytes());
            }
        }
        output
    }

    fn ticks(&self, zone: u64) -> BTreeMap<(u64, u16, u32, u64), Vec<u64>> {
        self.stacks
            .iter()
            .map(|(&(event, thread, context), rows)| {
                (
                    (zone, event, thread, context),
                    rows.iter().map(|row| row.tick).collect(),
                )
            })
            .collect()
    }
}

fn spans(
    recording: &Recording,
    first: usize,
    last: usize,
    ids: Option<&BTreeMap<u64, u64>>,
) -> Vec<SpanRow> {
    analysis(recording)
        .spans
        .iter()
        .filter(|span| (first..=last).contains(&span.received_frame))
        .map(|span| {
            (
                ids.map_or(span.zone, |ids| ids[&span.zone]),
                span.event,
                span.thread,
                span.context,
                span.start,
                span.end,
                span.received_frame - first,
            )
        })
        .collect()
}

fn values(
    recording: &Recording,
    first: usize,
    last: usize,
    ids: Option<&BTreeMap<u64, u64>>,
) -> Vec<ValueRow> {
    analysis(recording)
        .values
        .iter()
        .filter(|value| (first..=last).contains(&value.received_frame))
        .map(|value| {
            (
                ids.map_or(value.zone, |ids| ids[&value.zone]),
                value.event,
                value.thread,
                value.context,
                value.raw_value,
                value.received_frame - first,
            )
        })
        .collect()
}

fn pending(
    recording: &Recording,
    ids: Option<&BTreeMap<u64, u64>>,
) -> BTreeMap<(u64, u16, u32, u64), Vec<u64>> {
    analysis(recording)
        .open_event_starts()
        .map(|(&(zone, event, thread, context), ticks)| {
            (
                (ids.map_or(zone, |ids| ids[&zone]), event, thread, context),
                ticks.to_vec(),
            )
        })
        .collect()
}

fn totals_match(actual: &NativeMemoryTotals, expected: &NativeMemoryTotals) {
    assert_eq!(actual.len(), expected.len());
    assert_eq!(actual.nonnegative_bytes(), expected.nonnegative_bytes());
    for index in 0..expected.len() {
        assert_eq!(actual.row(index), expected.row(index));
    }
}

fn initial_errors(recording: &Recording) -> BTreeMap<(u64, u32), Vec<InitialUntimedError>> {
    let mut result: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for row in recording.initial_untimed_errors() {
        result
            .entry((row.container_instance, row.container_generation))
            .or_default()
            .push(row.clone());
    }
    result
}

fn verify_errors(output: &Recording, source: &Recording, raw: &Raw<'_>, first: usize, last: usize) {
    let (cut, end) = (raw.frames[first].0, raw.frames[last].1);
    let alive = raw.alive(cut);
    let mut expected = initial_errors(source);
    expected.retain(|(id, _), _| alive.contains(&(*id as u32)));
    for row in source.untimed_error_events().iter().filter(|row| {
        row.history_event < cut as u64 && alive.contains(&(row.container_instance as u32))
    }) {
        expected
            .entry((row.container_instance, row.container_generation))
            .or_default()
            .push(InitialUntimedError {
                code: row.code,
                message: row.message.clone(),
                file: row.file.clone(),
                line: row.line,
                index: row.index,
                container_instance: row.container_instance,
                container_generation: row.container_generation,
            });
    }
    assert_eq!(initial_errors(output), expected);
    let expected: Vec<_> = source
        .untimed_error_events()
        .iter()
        .filter(|row| (cut as u64..=end as u64).contains(&row.history_event))
        .map(|row| {
            let mut row = row.clone();
            row.history_event = row.history_event - cut as u64 + 2;
            row.received_frame -= first;
            row
        })
        .collect();
    assert_eq!(output.untimed_error_events(), expected);
}

fn verify_frame(actual: &SceneState, expected: &SceneState) {
    assert_eq!(actual.objects, expected.objects);
    assert_eq!(actual.cameras, expected.cameras);
    assert_eq!(actual.errors, expected.errors);
    assert_eq!(actual.debug_draw, expected.debug_draw);
    assert_eq!(actual.contacts, expected.contacts);
    let drawing: Vec<_> = actual
        .debug_draw
        .values()
        .flat_map(|batch| batch.iter())
        .collect();
    assert!(matches!(
        drawing.as_slice(),
        [Primitive::JointFrames { instance: 20, .. }]
    ));
    for (id, class) in [
        (1, "_debugger_::PxProfileMemoryEventBuffer"),
        (2, "_debugger_::PxProfileZone"),
        (3, "_debugger::MemoryEventTotals"),
        (5, "_debugger_::PvdUserRenderer"),
    ] {
        assert_eq!(actual.objects[&id].type_name, class);
    }
}

struct Case<'a> {
    source: &'a [u8],
    output: &'a [u8],
    original: &'a Recording,
    original_bytes: &'a [u8],
    sdk: &'a Recording,
    sdk_bytes: &'a [u8],
    local_first: usize,
    local_last: usize,
    absolute_first: usize,
    seed_starts: usize,
    initial_total_rows: usize,
    initial_total_bytes: u64,
}

#[derive(Clone, Copy)]
enum Window {
    Middle,
    Prefix,
}

fn verify(case: Case<'_>, initial: &native_initial::NativeInitialValues) {
    verify_window(case, initial, Window::Middle);
}

fn verify_window(case: Case<'_>, initial: &native_initial::NativeInitialValues, window: Window) {
    let source_raw = Raw::parse(case.source);
    let raw = Raw::parse(case.output);
    let source = Recording::from_bytes(case.source).unwrap();
    let (output, info) = native::read_with_progress(case.output, |_, _| true).unwrap();
    let count = case.local_last - case.local_first + 1;
    let absolute_last = case.absolute_first + count - 1;
    assert_eq!(output.frame_count(), count);
    assert_eq!(
        output.memory_history_scope(),
        MemoryHistoryScope::AfterInitialSnapshot
    );
    let cut = source_raw.frames[case.local_first].0;
    let end = source_raw.frames[case.local_last].1;
    let start_profile = source_raw.profile(cut);
    let expected_seed = match window {
        Window::Middle => start_profile.seed(),
        Window::Prefix => set(source_raw.events[1]).unwrap().data.to_vec(),
    };
    assert_eq!(expected_seed.len(), case.seed_starts * 26);
    let seed = set(raw.events[1]).unwrap();
    assert_eq!((seed.id, seed.property), (2, 50));
    assert_eq!(seed.undo, [1]);
    assert_eq!(seed.data, expected_seed);
    assert_eq!(raw.frames[0].0, 2);
    assert_eq!(raw.events.len(), end - cut + 3);
    match window {
        Window::Middle => {
            for (id, property) in [(1, 51), (2, 50), (3, 78), (5, 52)] {
                assert_eq!(raw.initial[&id].0, source_raw.initial[&id].0);
                assert_eq!(raw.array(id).0, source_raw.array(id).0);
                assert_eq!(raw.array(id).1, source_raw.latest(id, property, cut));
            }
        }
        Window::Prefix => {
            assert_eq!(case.local_first, 0);
            assert_eq!(raw.initial, source_raw.initial);
            assert_eq!(raw.events.as_slice(), &source_raw.events[..=end]);
        }
    }
    assert!(raw.array(2).1.is_empty());
    assert_eq!(raw.array(5).1.len(), 66);
    let captured = output.native_initial_memory_totals().unwrap().unwrap();
    assert_eq!(
        (captured.len(), captured.nonnegative_bytes()),
        (case.initial_total_rows, Some(case.initial_total_bytes))
    );
    assert_eq!(
        captured.nonnegative_bytes(),
        Some(
            analysis(case.sdk)
                .memory
                .state_at_frame(case.absolute_first - 1)
                .live_bytes
        )
    );
    match window {
        Window::Middle => verify_errors(
            &output,
            &source,
            &source_raw,
            case.local_first,
            case.local_last,
        ),
        Window::Prefix => {
            assert_eq!(initial_errors(&output), initial_errors(&source));
            let expected: Vec<_> = source
                .untimed_error_events()
                .iter()
                .filter(|row| row.history_event <= end as u64)
                .cloned()
                .collect();
            assert_eq!(output.untimed_error_events(), expected);
        }
    }

    let mut direct = Vec::new();
    trim_scene_frames(
        case.original_bytes,
        case.absolute_first,
        absolute_last,
        &mut direct,
        || true,
    )
    .unwrap();
    let direct_raw = Raw::parse(&direct);
    assert_eq!(raw.initial, direct_raw.initial);
    assert_eq!(raw.events, direct_raw.events);
    let direct = Recording::from_bytes(&direct).unwrap();
    assert_eq!(initial_errors(&output), initial_errors(&direct));
    let interval =
        pvd_core::clip::trim_scene_frames(case.sdk_bytes, case.absolute_first, absolute_last)
            .unwrap();
    let interval = Recording::from_bytes(&interval).unwrap();
    for frame in (0..count)
        .chain((0..count).rev())
        .chain([count - 1, 0, count / 2, count - 1, 0])
    {
        let actual = output.state_at(frame).unwrap();
        verify_frame(&actual, &source.state_at(case.local_first + frame).unwrap());
        verify_frame(
            &actual,
            &case.original.state_at(case.absolute_first + frame).unwrap(),
        );
        verify_frame(&actual, &direct.state_at(frame).unwrap());
        assert_eq!(
            output.frames[frame].timestamp,
            case.original.frames[case.absolute_first + frame].timestamp
        );
        assert_eq!(
            output.frames[frame].scene_id,
            case.original.frames[case.absolute_first + frame].scene_id
        );
        assert!(check_frame_state(&output, &interval, frame, &info.source_ids, initial) > 100);
        totals_match(
            &output.native_memory_totals_at(&actual).unwrap().unwrap(),
            &case
                .original
                .native_memory_totals_at(
                    &case.original.state_at(case.absolute_first + frame).unwrap(),
                )
                .unwrap()
                .unwrap(),
        );
    }
    native_memory_interval::verify_partial_interval(
        &output,
        case.sdk,
        case.absolute_first,
        absolute_last,
        &info.source_ids,
    );
    let captured_end = output.native_end_memory_totals().unwrap().unwrap();
    totals_match(
        &captured_end,
        &case
            .original
            .native_memory_totals_at(&case.original.state_at(absolute_last).unwrap())
            .unwrap()
            .unwrap(),
    );
    assert_eq!(
        captured_end.nonnegative_bytes(),
        Some(
            analysis(case.sdk)
                .memory
                .state_at_frame(absolute_last)
                .live_bytes
        )
    );
    assert_eq!(
        spans(&output, 0, count - 1, None),
        spans(&source, case.local_first, case.local_last, None)
    );
    assert_eq!(
        spans(&output, 0, count - 1, None),
        spans(case.original, case.absolute_first, absolute_last, None)
    );
    assert_eq!(
        values(&output, 0, count - 1, None),
        values(&source, case.local_first, case.local_last, None)
    );
    let sdk_prefix = pvd_core::clip::trim_scene_frames(case.sdk_bytes, 0, absolute_last).unwrap();
    let sdk_prefix = Recording::from_bytes(&sdk_prefix).unwrap();
    assert_eq!(
        spans(&output, 0, count - 1, Some(&info.source_ids)),
        spans(&sdk_prefix, case.absolute_first, absolute_last, None)
    );
    assert_eq!(
        values(&output, 0, count - 1, Some(&info.source_ids)),
        values(&sdk_prefix, case.absolute_first, absolute_last, None)
    );
    let end_profile = source_raw.profile(end + 1);
    assert_eq!(pending(&output, None), end_profile.ticks(2));
    assert_eq!(
        pending(&output, Some(&info.source_ids)),
        pending(&sdk_prefix, None)
    );
    let unmatched: BTreeMap<_, _> = end_profile
        .unmatched
        .iter()
        .filter_map(|(&key, &count)| {
            let delta = count - start_profile.unmatched.get(&key).copied().unwrap_or(0);
            (delta != 0).then_some(((2, key.0, key.1, key.2), delta))
        })
        .collect();
    assert_eq!(analysis(&output).unmatched_stop_events, unmatched);
    assert_eq!(
        analysis(&output).unmatched_stops,
        unmatched.values().sum::<u64>()
    );
    for (&id, zone) in &analysis(&source).zones {
        assert_eq!(analysis(&output).zones[&id].name, zone.name);
        assert_eq!(analysis(&output).zones[&id].events, zone.events);
    }
    let mut csv = Vec::new();
    assert!(
        pvd_core::analysis_export::write_analysis_csv(
            &output,
            &mut csv,
            pvd_core::analysis_export::AnalysisCsvKind::Memory,
            |_| true
        )
        .is_err()
    );
    assert!(csv.is_empty());
    output.verify_replay().unwrap();
    let mut rewritten = Vec::new();
    rewrite(case.output, &mut rewritten, || true).unwrap();
    assert_eq!(rewritten.as_slice(), case.output);
}

fn persist(name: &str, bytes: &[u8]) {
    if let Some(directory) = std::env::var_os("PVD_NATIVE_INITIAL_ANALYSIS_MIDDLE_DIRECTORY") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(name), bytes).unwrap();
    }
}

#[test]
#[ignore = "requires the preserved original rich native container and its same-run SDK stream"]
fn actual_rich_initial_analysis_middle_preserves_source_sdk_pending_and_partial_memory() {
    let path = pair("rich", "-native.pxd2");
    let original_bytes = std::fs::read(&path).unwrap();
    assert_eq!(original_bytes.len(), 1_034_096);
    let original = Recording::from_bytes(&original_bytes).unwrap();
    let sdk_bytes = std::fs::read(pair("rich", ".pvdstream")).unwrap();
    let sdk = Recording::from_bytes(&sdk_bytes).unwrap();
    assert_eq!((original.frame_count(), sdk.frame_count()), (120, 120));
    let initial = native_initial::NativeInitialValues::parse(&original_bytes);
    for (first, starts, total_rows, total_bytes) in [
        (1, 3, 252, 1_196_122),
        (30, 32, 253, 1_196_138),
        (60, 62, 269, 1_461_161),
    ] {
        let mut source = Vec::new();
        trim_scene_frames(&original_bytes, first, first + 1, &mut source, || true).unwrap();
        let mut output = Vec::new();
        trim_scene_frames(&source, 1, 1, &mut output, || true).unwrap();
        verify(
            Case {
                source: &source,
                output: &output,
                original: &original,
                original_bytes: &original_bytes,
                sdk: &sdk,
                sdk_bytes: &sdk_bytes,
                local_first: 1,
                local_last: 1,
                absolute_first: first + 1,
                seed_starts: starts,
                initial_total_rows: total_rows,
                initial_total_bytes: total_bytes,
            },
            &initial,
        );
        assert_eq!(Raw::parse(&output).events.len(), 26);
        persist(&format!("rich-{first}-{}-source.pxd2", first + 1), &source);
        persist(&format!("rich-{}-retrim.pxd2", first + 1), &output);
    }
    let mut source = Vec::new();
    trim_scene_frames(&original_bytes, 30, 34, &mut source, || true).unwrap();
    let mut middle = Vec::new();
    trim_scene_frames(&source, 1, 3, &mut middle, || true).unwrap();
    verify(
        Case {
            source: &source,
            output: &middle,
            original: &original,
            original_bytes: &original_bytes,
            sdk: &sdk,
            sdk_bytes: &sdk_bytes,
            local_first: 1,
            local_last: 3,
            absolute_first: 31,
            seed_starts: 32,
            initial_total_rows: 253,
            initial_total_bytes: 1_196_138,
        },
        &initial,
    );
    let mut repeated = Vec::new();
    trim_scene_frames(&middle, 1, 1, &mut repeated, || true).unwrap();
    verify(
        Case {
            source: &middle,
            output: &repeated,
            original: &original,
            original_bytes: &original_bytes,
            sdk: &sdk,
            sdk_bytes: &sdk_bytes,
            local_first: 1,
            local_last: 1,
            absolute_first: 32,
            seed_starts: 33,
            initial_total_rows: 253,
            initial_total_bytes: 1_195_778,
        },
        &initial,
    );
    persist("rich-30-34-source.pxd2", &source);
    persist("rich-31-33-retrim.pxd2", &middle);
    persist("rich-32-repeated.pxd2", &repeated);
    assert_eq!(std::fs::read(path).unwrap(), original_bytes);
}
