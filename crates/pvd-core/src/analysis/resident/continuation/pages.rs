use super::buffer::pages::{self as sequences, BufferMemos, BufferRef, BufferRegistry};
use super::tree::pages::{self as metadata, TreeMemos, TreeRef, TreeRegistry, ValueWire};
use super::*;
use crate::analysis::resident::write_maps::{ResidentBuild, memo::ResidentMemos};

mod context;
mod memos;
mod read;
mod root;
#[cfg(test)]
mod tests;
mod write;
use context::{ReadContext, WriteContext};
use memos::{FullMemos, FullRegistry};
pub(crate) use read::FullAnalysisReader;
pub(crate) use write::{FullAnalysisWriter, PreparedAnalysis};

#[cfg(test)]
pub(crate) fn full_fixture_bytes() -> Vec<u8> {
    postcard::to_stdvec(&super::tests::source()).unwrap()
}
#[cfg(test)]
pub(crate) fn full_fixture_inputs(
    endian: pvd_protocol::Endian,
) -> Vec<(u64, &'static str, Vec<u8>)> {
    super::tests::buffers(endian)
}

#[derive(Clone)]
pub(crate) struct FullLimits {
    pub sequence_node_slots: usize,
    pub sequence_chunk_slots: usize,
    pub metadata_slots: usize,
    pub root_slots: usize,
    pub maps: ResidentLimits,
    pub writer_memo_bytes: usize,
}
impl Default for FullLimits {
    fn default() -> Self {
        Self {
            sequence_node_slots: 4096,
            sequence_chunk_slots: 4096,
            metadata_slots: 256,
            root_slots: 16,
            maps: ResidentLimits::default(),
            writer_memo_bytes: 32 << 20,
        }
    }
}

#[derive(Default, Debug)]
pub(crate) struct FullStatistics {
    pub map_nodes: usize,
    pub map_new_values: usize,
    pub map_new_strings: usize,
    pub map_new_pages: usize,
    pub map_reused_pages: usize,
    pub writer_pin_capacity_misses: usize,
    pub logical_bytes: u64,
    pub old_rows: [[usize; 4]; 2],
    pub staged_rows: [[usize; 4]; 2],
    pub published_rows: [[usize; 4]; 2],
    pub sequence_nodes: usize,
    pub sequence_chunks: usize,
    pub sequence_elements: usize,
    pub metadata_nodes: usize,
    pub cached_roots: usize,
    pub cache_capacity_bytes: usize,
    pub read_pages: usize,
}

pub(super) trait PageWrite {
    type Store: Clone;
    fn store(&self) -> Self::Store;
    fn same(&self, store: &Self::Store) -> bool;
    fn domain(&self) -> &Domain;
    fn check(&mut self) -> Result<(), Error>;
    fn page(&mut self, bytes: &[u8], dependencies: &[u64]) -> Result<u64, Error>;
    fn usable(&mut self, id: u64, staged: bool) -> bool;
    fn values(&mut self, values: &[ResidentValue]) -> Result<Vec<RowRef>, Error>;
}

pub(super) trait PageRead {
    type Store: Clone;
    fn store(&self) -> Self::Store;
    fn same(&self, store: &Self::Store) -> bool;
    fn domain(&self) -> &Domain;
    fn check(&mut self) -> Result<(), Error>;
    fn load(&mut self, id: u64, maximum: usize) -> Result<ReadPage, Error>;
    fn values(&mut self, references: &[RowRef]) -> Result<Owned<Vec<ResidentValue>>, Error>;
}

pub(super) struct ReadPage {
    pub loaded: LoadedPage,
    _memory: MemoryLease,
}
impl ReadPage {
    pub(super) fn new(loaded: LoadedPage, memory: MemoryLease) -> Self {
        Self {
            loaded,
            _memory: memory,
        }
    }
}
