use super::*;
use pvd_core::analysis::view::{AllocationView, MemoryEventView, MemorySummary};
use pvd_core::analysis::{MemoryChurn, MemorySample};
use std::{cell::Cell, collections::BTreeMap, ops::Range, sync::Arc};

#[derive(Clone, Default)]
struct Pages {
    rows: Arc<BTreeMap<usize, Box<(u64, String)>>>,
    frame: usize,
}

impl MemoryRows for Pages {
    fn live_count(&self) -> usize {
        self.rows.len()
    }
    fn live_bytes(&self) -> u64 {
        self.rows.len() as u64 * 16
    }
    fn allocation_at(&self, index: usize) -> Option<AllocationView<'_>> {
        let row = self.rows.get(&index)?;
        Some(AllocationView {
            address: row.0,
            size: 16,
            type_name: &row.1,
            file: "pages.cpp",
            line: 7,
            received_frame: self.frame,
        })
    }
}

struct Source {
    revision: u64,
    fail: bool,
    pages: Pages,
    calls: Cell<usize>,
}

impl MemorySource for Source {
    type Cursor = Pages;
    fn revision(&self) -> u64 {
        self.revision
    }
    fn first_retained_frame(&self) -> usize {
        0
    }
    fn summary(&self) -> MemorySummary {
        MemorySummary::default()
    }
    fn seek(&self, cursor: &mut Pages, frame: usize) -> Result<(), pvd_core::Error> {
        self.calls.set(self.calls.get() + 1);
        *cursor = self.pages.clone();
        cursor.frame = frame;
        if self.fail {
            return Err(std::io::Error::other("cursor budget exceeded").into());
        }
        Ok(())
    }
    fn event_count(&self) -> usize {
        0
    }
    fn event_at(&self, _: usize) -> Option<MemoryEventView<'_>> {
        None
    }
    fn frame_events(&self, _: usize) -> Range<usize> {
        0..0
    }
    fn churn(&self, _: usize) -> MemoryChurn {
        MemoryChurn::default()
    }
    fn sample_count(&self) -> usize {
        0
    }
    fn sample_at(&self, _: usize) -> Option<MemorySample> {
        None
    }
}

fn source(revision: u64, offset: u64) -> Source {
    Source {
        revision,
        fail: false,
        calls: Cell::new(0),
        pages: Pages {
            rows: Arc::new(
                (0..513)
                    .map(|index| {
                        (
                            index,
                            Box::new((offset + index as u64, format!("row-{index}"))),
                        )
                    })
                    .collect(),
            ),
            frame: 0,
        },
    }
}

#[test]
fn panel_reads_noncontiguous_rows_and_refreshes_changed_revision_without_events() {
    let first = source(1, 1000);
    let mut panel = PanelState::<Pages>::default();
    panel.update(&first, 7).unwrap();
    let before = Arc::strong_count(&first.pages.rows);
    for index in [0, 255, 256, 512] {
        let row = panel.cursor.allocation_at(index).unwrap();
        assert_eq!(row.address, 1000 + index as u64);
        assert_eq!(row.type_name, format!("row-{index}"));
        assert_eq!(row.received_frame, 7);
    }
    assert_eq!(Arc::strong_count(&first.pages.rows), before);
    assert!(panel.cursor.allocation_at(513).is_none());
    let second = source(2, 2000);
    panel.update(&second, 7).unwrap();
    assert_eq!(panel.cursor.allocation_at(512).unwrap().address, 2512);
    assert_eq!(panel.key, Some((2, 7)));
}

#[test]
fn failed_seek_preserves_the_published_cursor_and_allows_retry() {
    let mut memory = source(1, 1000);
    let mut panel = PanelState::<Pages>::default();
    panel.update(&memory, 7).unwrap();
    memory.fail = true;
    assert!(panel.update(&memory, 11).is_err());
    assert_eq!(panel.key, Some((1, 7)));
    assert_eq!(panel.cursor.allocation_at(512).unwrap().received_frame, 7);
    assert!(panel.update(&memory, 11).is_err());
    assert_eq!(memory.calls.get(), 2);
    memory.fail = false;
    assert!(panel.update(&memory, 11).is_err());
    assert_eq!(memory.calls.get(), 2);
    panel.retry();
    panel.update(&memory, 11).unwrap();
    assert_eq!(panel.key, Some((1, 11)));
    assert_eq!(panel.cursor.allocation_at(512).unwrap().received_frame, 11);
    assert_eq!(memory.calls.get(), 3);
}

#[test]
fn a_changed_revision_or_frame_releases_the_failed_request() {
    let mut memory = source(1, 1000);
    let mut panel = PanelState::<Pages>::default();
    memory.fail = true;
    assert!(panel.update(&memory, 7).is_err());
    memory.fail = false;
    panel.update(&memory, 8).unwrap();
    memory.fail = true;
    assert!(panel.update(&memory, 11).is_err());
    memory.fail = false;
    memory.revision = 2;
    panel.update(&memory, 11).unwrap();
    assert_eq!(panel.key, Some((2, 11)));
    assert!(panel.failed.is_none());
    assert_eq!(memory.calls.get(), 4);
}

#[test]
fn source_reset_releases_owned_pages_and_failed_request_but_preserves_display_options() {
    let mut memory = source(1, 1000);
    let weak = Arc::downgrade(&memory.pages.rows);
    let mut panel = PanelState::<Pages> {
        events: true,
        at_end: true,
        ..Default::default()
    };
    panel.update(&memory, 7).unwrap();
    memory.fail = true;
    assert!(panel.update(&memory, 11).is_err());
    drop(memory);
    assert!(weak.upgrade().is_some());
    panel.reset_source();
    assert!(weak.upgrade().is_none());
    assert!(panel.key.is_none() && panel.failed.is_none());
    assert!(panel.events && panel.at_end);
    assert_eq!(panel.cursor.live_count(), 0);
}

#[test]
fn prepare_releases_hidden_memory_allocations_when_the_source_epoch_changes() {
    use bevy::prelude::*;
    use bevy_egui::{EguiContext, EguiUserTextures, PrimaryEguiContext};
    use pvd_core::analysis::Analysis;
    use pvd_protocol::{Endian, Header};

    let mut bytes = Vec::new();
    for (handle, text) in [(1u32, "Shape"), (2, "source.cpp")] {
        bytes.extend(1u16.to_le_bytes());
        bytes.extend((text.len() as u32 + 1).to_le_bytes());
        bytes.extend(text.as_bytes());
        bytes.push(0);
        bytes.extend(handle.to_le_bytes());
    }
    bytes.extend([2, 0, 64, 1, 2, 20, 88]);
    let mut analysis = Analysis::default();
    analysis
        .ingest(7, "PxProfileMemoryEventBuffer", &bytes, Endian::Little, 0)
        .unwrap();
    let weak = Arc::downgrade(analysis.memory.live.get(&(7, 88)).unwrap());
    let mut recording = pvd_core::RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    })
    .finish()
    .unwrap();
    recording.analysis = analysis.into();
    let recording = Arc::new(recording);
    let context = egui::Context::default();
    let mut studio = Studio::default();
    studio.persist_layout = false;
    studio.recording = Some(recording.clone());
    let mut app = App::new();
    app.insert_resource(studio)
        .init_resource::<EguiUserTextures>()
        .add_systems(Update, crate::ui::prepare);
    let mut entity = app.world_mut().spawn(PrimaryEguiContext);
    *entity.get_mut::<EguiContext>().unwrap().get_mut() = context;
    app.update();
    {
        let mut studio = app.world_mut().resource_mut::<Studio>();
        studio
            .memory_panel
            .update(&recording.analysis.memory_snapshot(), 0)
            .unwrap();
        studio.memory_panel.events = true;
        studio.memory_panel.at_end = true;
        assert_eq!(studio.memory_panel.cursor.live_count(), 1);
        studio.recording = None;
        studio.profile_view_epoch += 1;
    }
    drop(recording);
    assert!(weak.upgrade().is_some());
    app.update();
    assert!(weak.upgrade().is_none());
    let panel = &app.world().resource::<Studio>().memory_panel;
    assert!(panel.events && panel.at_end);
    assert!(panel.key.is_none() && panel.failed.is_none());
    assert_eq!(panel.cursor.live_count(), 0);
}

#[test]
fn panel_distinguishes_frame_survivors_from_net_change_and_capture_end() {
    use crate::i18n::Language;
    use pvd_core::analysis::Analysis;
    use pvd_protocol::{Endian, Header};

    let mut initial = Vec::new();
    for (handle, text) in [(1u32, "Block"), (2, "source.cpp")] {
        initial.extend(1u16.to_le_bytes());
        initial.extend((text.len() as u32 + 1).to_le_bytes());
        initial.extend(text.bytes());
        initial.push(0);
        initial.extend(handle.to_le_bytes());
    }
    initial.extend([2, 0, 64, 1, 2, 17, 10]);
    let mut analysis = Analysis::default();
    for (frame, bytes) in [
        (0, initial),
        (
            1,
            vec![
                3, 0, 10, 2, 0, 96, 1, 2, 17, 10, 2, 0, 8, 1, 2, 17, 11, 3, 0, 11,
            ],
        ),
        (4, vec![3, 0, 10]),
    ] {
        analysis
            .ingest(
                7,
                "PxProfileMemoryEventBuffer",
                &bytes,
                Endian::Little,
                frame,
            )
            .unwrap();
    }
    let mut recording = pvd_core::RecordingBuilder::new(Header {
        version: 1,
        timestamp_numerator: 1,
        timestamp_denominator: 1,
        flags: 1,
        endian: Endian::Little,
    })
    .finish()
    .unwrap();
    recording.analysis = analysis.into();
    let recording = Arc::new(recording);
    for language in [Language::English, Language::Chinese] {
        let mut studio = Studio::default();
        studio.persist_layout = false;
        studio.prefs.language = language;
        studio.recording = Some(recording.clone());
        studio.frame = 1;
        let context = egui::Context::default();
        context.style_mut(|style| style.animation_time = 0.0);
        let details = language.text("Activity and trend", "活动与趋势");
        let mut draw = |at_end, events| {
            studio.memory_panel.at_end = at_end;
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(700.0, 450.0),
                    )),
                    events,
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| show(ui, &mut studio));
                },
            );
            let mut toggle = None;
            let labels = output
                .shapes
                .into_iter()
                .filter_map(|shape| match shape.shape {
                    egui::epaint::Shape::Text(text) => {
                        if text.galley.text() == details {
                            toggle = Some(text.pos + text.galley.size() * 0.5);
                        }
                        Some(text.galley.text().to_owned())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            (labels, toggle)
        };
        let (labels, toggle) = draw(false, Vec::new());
        assert!(
            labels
                .iter()
                .any(|label| label.contains(language.text("Churn: 96 B", "本帧新增存活: 96 B"))),
            "{labels:?}"
        );
        assert!(labels.iter().any(|label| label == "0x000000000000000a"));
        assert!(!labels.iter().any(|label| label.contains("+32 B")));
        let position = toggle.expect("activity details remain accessible when collapsed");
        draw(
            false,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let (labels, _) = draw(false, Vec::new());
        assert!(
            labels
                .iter()
                .any(|label| label.contains(language.text("net +32 B", "净变化 +32 B"))),
            "{labels:?}"
        );
        let (labels, _) = draw(true, Vec::new());
        assert!(
            labels
                .iter()
                .any(|label| label.contains(language.text("Churn: 0 B", "本帧新增存活: 0 B"))),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|label| label.contains(language.text("Flush frame 4", "刷新帧 4"))),
            "{labels:?}"
        );
    }
}

struct Counted<'a, S> {
    source: &'a S,
    reads: Cell<usize>,
}

impl<S: MemorySource> MemorySource for Counted<'_, S> {
    type Cursor = S::Cursor;
    fn revision(&self) -> u64 {
        self.source.revision()
    }
    fn first_retained_frame(&self) -> usize {
        self.source.first_retained_frame()
    }
    fn summary(&self) -> MemorySummary {
        self.source.summary()
    }
    fn seek(&self, cursor: &mut Self::Cursor, frame: usize) -> Result<(), pvd_core::Error> {
        self.source.seek(cursor, frame)
    }
    fn event_count(&self) -> usize {
        self.source.event_count()
    }
    fn event_at(&self, index: usize) -> Option<MemoryEventView<'_>> {
        self.reads.set(self.reads.get() + 1);
        self.source.event_at(index)
    }
    fn frame_events(&self, frame: usize) -> Range<usize> {
        self.source.frame_events(frame)
    }
    fn churn(&self, frame: usize) -> MemoryChurn {
        self.source.churn(frame)
    }
    fn sample_count(&self) -> usize {
        self.source.sample_count()
    }
    fn sample_at(&self, index: usize) -> Option<MemorySample> {
        self.source.sample_at(index)
    }
}

fn group_analysis() -> pvd_core::analysis::SharedAnalysis {
    use pvd_protocol::Endian;
    fn strings() -> Vec<u8> {
        let mut bytes = Vec::new();
        for (handle, text) in [
            (1u32, "Block"),
            (2, "/a/source.cpp"),
            (3, "C:\\b\\source.cpp"),
            (4, "other.cpp"),
        ] {
            bytes.extend(1u16.to_le_bytes());
            bytes.extend((text.len() as u32 + 1).to_le_bytes());
            bytes.extend(text.bytes());
            bytes.push(0);
            bytes.extend(handle.to_le_bytes());
        }
        bytes
    }
    let mut initial = strings();
    initial.extend([2, 0, 64, 1, 2, 17, 10]);
    let mut analysis = pvd_core::analysis::Analysis::default();
    analysis
        .ingest(7, "PxProfileMemoryEventBuffer", &initial, Endian::Little, 0)
        .unwrap();
    analysis
        .ingest(
            7,
            "PxProfileMemoryEventBuffer",
            &[
                3, 0, 10, 2, 0, 96, 1, 2, 17, 10, 2, 0, 8, 1, 3, 17, 11, 3, 0, 11, 2, 0, 4, 1, 2,
                17, 12, 2, 0, 16, 1, 4, 20, 12, 3, 0, 99,
            ],
            Endian::Little,
            1,
        )
        .unwrap();
    let mut other = strings();
    other.extend([2, 0, 5, 1, 2, 17, 10]);
    analysis
        .ingest(8, "PxProfileMemoryEventBuffer", &other, Endian::Little, 1)
        .unwrap();
    analysis.into()
}

#[test]
fn source_groups_match_flush_events_without_merging_buffer_addresses_or_old_frees() {
    let analysis = group_analysis();
    let memory = analysis.memory_snapshot();
    let mut cache = groups::Cache::default();
    cache.advance(&memory, 1, groups::Limits::default(), usize::MAX, || true);
    assert!(cache.error.is_none());
    assert!(cache.progress().is_none());
    assert_eq!(cache.rows.len(), 2);
    let other = &cache.rows[0];
    assert_eq!(other.label, "other.cpp:20 (Block)");
    assert_eq!(
        (
            other.allocated_bytes,
            other.surviving_bytes,
            other.survivors
        ),
        (16, 16, 1)
    );
    let source = &cache.rows[1];
    assert_eq!(source.label, "source.cpp:17 (Block)");
    assert_eq!((source.allocated_bytes, source.allocations), (113, 4));
    assert_eq!((source.freed_bytes, source.frees), (8, 1));
    assert_eq!((source.surviving_bytes, source.survivors), (101, 2));
    assert_eq!(source.file, "/a/source.cpp");
    assert!(source.multiple_files);
    assert_eq!(memory.churn(1).freed_bytes, 72);
    assert_eq!(memory.churn(1).surviving_bytes, 117);
    cache.advance(&memory, 0, groups::Limits::default(), usize::MAX, || true);
    assert_eq!(cache.rows.len(), 1);
    assert_eq!(cache.rows[0].surviving_bytes, 64);
}

#[test]
fn source_group_work_is_bounded_cached_and_cancelled_until_the_key_changes() {
    let mut analysis = group_analysis();
    let mut cache = groups::Cache::default();
    {
        let memory = analysis.memory_snapshot();
        let source = Counted {
            source: &memory,
            reads: Cell::new(0),
        };
        cache.advance(&source, 1, groups::Limits::default(), 2, || true);
        assert_eq!(source.reads.get(), 2);
        assert_eq!(cache.progress(), Some((2, 8)));
        assert!(cache.rows.is_empty());
        cache.advance(&source, 1, groups::Limits::default(), 100, || false);
        assert_eq!(source.reads.get(), 2);
        cache.cancel();
        cache.advance(&source, 1, groups::Limits::default(), 100, || true);
        assert_eq!(source.reads.get(), 2);
        assert!(cache.cancelled);
        cache.advance(&source, 0, groups::Limits::default(), 100, || true);
        assert!(!cache.cancelled);
        assert_eq!(source.reads.get(), 3);
        cache.advance(&source, 0, groups::Limits::default(), 100, || true);
        assert_eq!(source.reads.get(), 3);
        cache.advance(&source, 1, groups::Limits::default(), 100, || true);
        assert_eq!(source.reads.get(), 11);
    }
    analysis
        .ingest(
            7,
            "PxProfileMemoryEventBuffer",
            &[2, 0, 7, 1, 2, 17, 14],
            pvd_protocol::Endian::Little,
            1,
        )
        .unwrap();
    cache.advance(
        &analysis.memory_snapshot(),
        1,
        groups::Limits::default(),
        100,
        || true,
    );
    assert_eq!(cache.rows[1].surviving_bytes, 108);
    cache.reset();
    assert!(cache.rows.is_empty() && cache.progress().is_none());
}

#[test]
fn source_group_limits_fail_without_publishing_partial_rows() {
    let analysis = group_analysis();
    for limits in [
        groups::Limits {
            rows: 0,
            ..Default::default()
        },
        groups::Limits {
            addresses: 0,
            ..Default::default()
        },
        groups::Limits {
            bytes: 0,
            ..Default::default()
        },
    ] {
        let memory = analysis.memory_snapshot();
        let source = Counted {
            source: &memory,
            reads: Cell::new(0),
        };
        let mut cache = groups::Cache::default();
        cache.advance(&source, 1, limits, 100, || true);
        assert!(cache.error.is_some());
        assert!(cache.rows.is_empty() && cache.progress().is_none());
        let reads = source.reads.get();
        cache.advance(&source, 1, limits, 100, || true);
        assert_eq!(source.reads.get(), reads);
    }
}
