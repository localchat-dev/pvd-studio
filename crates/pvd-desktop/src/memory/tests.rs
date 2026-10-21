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
