#![allow(dead_code)]

use pvd_core::{
    Recording,
    analysis::{
        Analysis, SharedAnalysis,
        view::{MemorySource, MemoryViewCursor},
    },
};

pub fn legacy_fixture(recording: &Recording) -> &Analysis {
    recording
        .analysis
        .legacy()
        .expect("in-memory fixture uses Legacy analysis")
}

pub fn edit_legacy_fixture(recording: &mut Recording, edit: impl FnOnce(&mut Analysis)) {
    let mut analysis = legacy_fixture(recording).clone();
    edit(&mut analysis);
    recording.analysis = analysis.into();
}

pub fn memory_at(analysis: &SharedAnalysis, frame: usize) -> Vec<u8> {
    let mut cursor = MemoryViewCursor::default();
    analysis.memory_snapshot().seek(&mut cursor, frame).unwrap();
    postcard::to_stdvec(&cursor).unwrap()
}
