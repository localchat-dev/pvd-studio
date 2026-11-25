use std::sync::{Arc, Weak};

use crate::CHUNK_SIZE;

const MAX_BYTES: usize = 128 << 20;

#[derive(Default)]
pub(super) struct MemoryPrefix {
    lease: Arc<()>,
    chunks: Vec<Arc<Vec<u8>>>,
    tail: Vec<u8>,
    bytes: usize,
}

pub(super) struct MemoryReplay {
    _lease: Arc<()>,
    pub(super) chunks: Vec<Arc<Vec<u8>>>,
}

impl MemoryPrefix {
    pub(super) fn lease(&self) -> Weak<()> {
        Arc::downgrade(&self.lease)
    }

    pub(super) fn append(&mut self, mut bytes: &[u8]) -> bool {
        if self.bytes + bytes.len() > MAX_BYTES {
            return false;
        }
        self.bytes += bytes.len();
        while !bytes.is_empty() {
            if self.tail.is_empty() {
                self.tail.reserve_exact(CHUNK_SIZE);
            }
            let count = bytes.len().min(CHUNK_SIZE - self.tail.len());
            self.tail.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.tail.len() == CHUNK_SIZE {
                self.chunks.push(Arc::new(std::mem::take(&mut self.tail)));
            }
        }
        true
    }

    pub(super) fn replay(&self) -> MemoryReplay {
        let mut chunks = self.chunks.clone();
        if !self.tail.is_empty() {
            chunks.push(Arc::new(self.tail.clone()));
        }
        MemoryReplay {
            _lease: self.lease.clone(),
            chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_freezes_complete_blocks_and_the_current_tail() {
        let mut prefix = MemoryPrefix::default();
        let lease = prefix.lease();
        let block = vec![1; CHUNK_SIZE];
        assert!(prefix.append(&block[..CHUNK_SIZE - 2]));
        assert!(prefix.append(&[1, 1, 2, 3]));
        let replay = prefix.replay();
        assert!(prefix.append(&[4, 5]));
        assert_eq!(replay.chunks.len(), 2);
        assert_eq!(replay.chunks[0].as_slice(), block);
        assert_eq!(replay.chunks[1].as_slice(), [2, 3]);
        drop(prefix);
        assert!(lease.upgrade().is_some());
        drop(replay);
        assert!(lease.upgrade().is_none());
    }
}
