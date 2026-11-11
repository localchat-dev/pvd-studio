use crate::{Error, debug_draw::Primitive, error, frame_sources::FrameSourceKey};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

const STREAM_BYTES: usize = 64 << 20;
const SOURCE_BYTES: usize = 128;
const BATCH_BYTES: usize = 128;
const OWNER_BYTES: usize = 64;
const CACHE_BYTES: usize = 128;
const MAX_SOURCES: usize = 4096;

type DrawMap = BTreeMap<u64, Arc<Vec<Primitive>>>;
type ViewKey = (u64, usize, usize);

struct Batch {
    owner: u64,
    primitives: Arc<[Primitive]>,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct DrawHistory {
    batches: BTreeMap<usize, Batch>,
    owners: BTreeMap<u64, usize>,
    cursors: BTreeMap<FrameSourceKey, usize>,
    cache: BTreeMap<ViewKey, Weak<Vec<Primitive>>>,
    next: usize,
    discarded_before: usize,
    payload_bytes: usize,
}

pub(crate) fn primitive_bytes(values: &[Primitive]) -> usize {
    std::mem::size_of_val(values).saturating_add(
        values
            .iter()
            .map(|primitive| match primitive {
                Primitive::Text { text, .. } => text.len(),
                _ => 0,
            })
            .sum::<usize>(),
    )
}

impl DrawHistory {
    pub fn index_bytes(&self) -> usize {
        self.batches
            .len()
            .saturating_mul(BATCH_BYTES)
            .saturating_add(self.owners.len().saturating_mul(OWNER_BYTES))
            .saturating_add(self.cursors.len().saturating_mul(SOURCE_BYTES))
            .saturating_add(self.cache.len().saturating_mul(CACHE_BYTES))
    }
    pub fn memory_bytes(&self, retain_complete: bool) -> usize {
        self.index_bytes().saturating_add(if retain_complete {
            0
        } else {
            self.payload_bytes
        })
    }
    fn retained_bytes(&self) -> usize {
        self.index_bytes().saturating_add(self.payload_bytes)
    }
    fn drop_batch(&mut self, index: usize) {
        let batch = self.batches.remove(&index).unwrap();
        self.payload_bytes -= batch.bytes;
        let count = self.owners.get_mut(&batch.owner).unwrap();
        *count -= 1;
        if *count == 0 {
            self.owners.remove(&batch.owner);
        }
    }
    fn reserve(
        &mut self,
        additional: usize,
        limit: usize,
        retain_complete: bool,
    ) -> Result<(), Error> {
        let limit = if retain_complete {
            limit
        } else {
            limit.min(STREAM_BYTES)
        };
        if self.retained_bytes().saturating_add(additional) <= limit {
            return Ok(());
        }
        self.cache.retain(|_, view| view.strong_count() != 0);
        if !retain_complete {
            let floor = self.cursors.values().copied().min().unwrap_or(0);
            while self.retained_bytes().saturating_add(additional) > limit {
                let Some((&index, _)) = self
                    .batches
                    .first_key_value()
                    .filter(|(index, _)| **index < floor)
                else {
                    break;
                };
                self.drop_batch(index);
                self.discarded_before = self.discarded_before.max(index + 1);
            }
        }
        if self.retained_bytes().saturating_add(additional) > limit {
            return Err(error(
                "renderer source window exceeds its retained history budget; a source without a later End can prevent reclamation",
            ));
        }
        Ok(())
    }
    pub fn append(
        &mut self,
        owner: u64,
        primitives: Arc<[Primitive]>,
        limit: usize,
        retain_complete: bool,
    ) -> Result<(), Error> {
        if primitives.is_empty() {
            return Ok(());
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or_else(|| error("renderer history ordinal overflow"))?;
        let bytes = primitive_bytes(&primitives);
        let extra = bytes
            .checked_add(BATCH_BYTES)
            .and_then(|bytes| bytes.checked_add(OWNER_BYTES))
            .ok_or_else(|| error("renderer history accounting overflow"))?;
        self.reserve(extra, limit, retain_complete)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(bytes)
            .ok_or_else(|| error("renderer history accounting overflow"))?;
        *self.owners.entry(owner).or_default() += 1;
        self.batches.insert(
            self.next,
            Batch {
                owner,
                primitives,
                bytes,
            },
        );
        self.next = next;
        Ok(())
    }
    pub fn invalidate_owner(&mut self, owner: u64) {
        if self.owners.remove(&owner).is_some() {
            let mut removed_bytes = 0usize;
            self.batches.retain(|_, batch| {
                if batch.owner == owner {
                    removed_bytes += batch.bytes;
                    false
                } else {
                    true
                }
            });
            self.payload_bytes -= removed_bytes;
        }
        if self
            .cache
            .range((owner, 0, 0)..=(owner, usize::MAX, usize::MAX))
            .next()
            .is_some()
        {
            self.cache.retain(|(id, _, _), _| *id != owner);
        }
    }
    pub fn prepare(
        &mut self,
        source: FrameSourceKey,
        limit: usize,
        retain_complete: bool,
        projection_budget: usize,
    ) -> Result<DrawMap, Error> {
        let lower = self.cursors.get(&source).copied().unwrap_or(0);
        if lower < self.discarded_before {
            return Err(error(
                "renderer history before this source's previous End is no longer retained; a first source frame requires history from the start",
            ));
        }
        let source_bytes = if self.cursors.contains_key(&source) {
            0
        } else {
            if self.cursors.len() >= MAX_SOURCES {
                return Err(error("renderer source directory exceeds its source budget"));
            }
            SOURCE_BYTES
        };
        self.reserve(source_bytes, limit, retain_complete)?;
        if lower < self.discarded_before {
            return Err(error(
                "renderer history before this source's previous End is no longer retained; a first source frame requires history from the start",
            ));
        }
        self.cursors.entry(source).or_insert(0);
        let result = self.project_window(
            lower,
            source_bytes,
            limit,
            retain_complete,
            projection_budget,
        );
        if source_bytes != 0 && result.is_err() {
            self.cursors.remove(&source);
        }
        result
    }
    fn project_window(
        &mut self,
        lower: usize,
        source_bytes: usize,
        limit: usize,
        retain_complete: bool,
        projection_budget: usize,
    ) -> Result<DrawMap, Error> {
        let batch_count = self.batches.range(lower..).count();
        let temporary_bytes = batch_count
            .checked_mul(224)
            .ok_or_else(|| error("renderer projection index overflow"))?;
        if temporary_bytes > projection_budget {
            return Err(error("renderer projection exceeds the core memory budget"));
        }
        let mut grouped = BTreeMap::<u64, Vec<(usize, Arc<[Primitive]>)>>::new();
        for (&index, batch) in self.batches.range(lower..) {
            grouped
                .entry(batch.owner)
                .or_default()
                .push((index, batch.primitives.clone()));
        }
        let mut required = temporary_bytes;
        let mut cache_entries = 0usize;
        for (&owner, batches) in &grouped {
            required = required
                .checked_add(128)
                .ok_or_else(|| error("renderer projection accounting overflow"))?;
            let key = (owner, batches.first().unwrap().0, batches.last().unwrap().0);
            if self
                .cache
                .get(&key)
                .is_none_or(|cached| cached.strong_count() == 0)
            {
                required = batches
                    .iter()
                    .try_fold(required, |bytes, (_, values)| {
                        bytes.checked_add(primitive_bytes(values))
                    })
                    .ok_or_else(|| error("renderer projection accounting overflow"))?;
                cache_entries += 1;
            }
        }
        required = required
            .saturating_add(source_bytes)
            .saturating_add(cache_entries.saturating_mul(CACHE_BYTES));
        if required > projection_budget {
            return Err(error("renderer projection exceeds the core memory budget"));
        }
        self.reserve(
            cache_entries.saturating_mul(CACHE_BYTES),
            limit,
            retain_complete,
        )?;
        let mut result = BTreeMap::new();
        for (owner, batches) in grouped {
            let key = (owner, batches.first().unwrap().0, batches.last().unwrap().0);
            let values = if let Some(values) = self.cache.get(&key).and_then(Weak::upgrade) {
                values
            } else {
                let count = batches
                    .iter()
                    .try_fold(0usize, |count, (_, values)| count.checked_add(values.len()))
                    .ok_or_else(|| error("renderer projection primitive count overflow"))?;
                let mut values = Vec::new();
                values.try_reserve_exact(count)?;
                for (_, batch) in &batches {
                    values.extend(batch.iter().cloned());
                }
                let values = Arc::new(values);
                self.cache.insert(key, Arc::downgrade(&values));
                values
            };
            result.insert(owner, values);
        }
        Ok(result)
    }
    pub fn commit(&mut self, source: FrameSourceKey) {
        *self
            .cursors
            .get_mut(&source)
            .expect("frame draw view must be prepared before commit") = self.next;
    }
}

#[cfg(test)]
mod tests;
