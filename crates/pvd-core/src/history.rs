use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ops::{Index, IndexMut, Range},
    sync::Arc,
};

use crate::{
    Action, ErrorEvent, Object, SceneState, SourceCamera, Value, camera_entry_bytes, contacts,
    debug_draw,
};

const ACTIONS_PER_BLOCK: usize = 1024;
const INDEX_BLOCK_ITEMS: usize = 256;

/// Completed blocks stay immutable; a new snapshot copies at most the active block on append.
#[derive(Clone, Debug)]
pub struct SharedIndex<T> {
    blocks: Arc<VecDeque<Arc<Vec<T>>>>,
    active: Arc<Vec<T>>,
    head_skip: usize,
    len: usize,
}

impl<T> Default for SharedIndex<T> {
    fn default() -> Self {
        Self {
            blocks: Arc::new(VecDeque::new()),
            active: Arc::new(Vec::new()),
            head_skip: 0,
            len: 0,
        }
    }
}

impl<T> SharedIndex<T> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        if index >= self.len {
            return None;
        }
        let position = self.head_skip + index;
        let block = position / INDEX_BLOCK_ITEMS;
        let offset = position % INDEX_BLOCK_ITEMS;
        if block < self.blocks.len() {
            self.blocks[block].get(offset)
        } else {
            self.active.get(offset)
        }
    }

    pub fn first(&self) -> Option<&T> {
        self.get(0)
    }

    pub fn last(&self) -> Option<&T> {
        self.len.checked_sub(1).and_then(|index| self.get(index))
    }

    pub fn iter(&self) -> SharedIndexIter<'_, T> {
        SharedIndexIter {
            index: self,
            range: 0..self.len,
        }
    }

    pub fn windows(
        &self,
        size: usize,
    ) -> impl DoubleEndedIterator<Item = SharedIndexWindow<'_, T>> {
        assert!(size != 0, "history window size must be positive");
        let count = self.len.checked_sub(size).map_or(0, |len| len + 1);
        (0..count).map(move |start| SharedIndexWindow {
            index: self,
            start,
            len: size,
        })
    }

    pub fn binary_search_by_key<K: Ord>(
        &self,
        key: &K,
        mut value: impl FnMut(&T) -> K,
    ) -> Result<usize, usize> {
        let mut lower = 0;
        let mut upper = self.len;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            match value(&self[middle]).cmp(key) {
                std::cmp::Ordering::Less => lower = middle + 1,
                std::cmp::Ordering::Greater => upper = middle,
                std::cmp::Ordering::Equal => return Ok(middle),
            }
        }
        Err(lower)
    }

    pub fn partition_point(&self, mut predicate: impl FnMut(&T) -> bool) -> usize {
        let mut lower = 0;
        let mut upper = self.len;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            if predicate(&self[middle]) {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        lower
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn discard_front(&mut self, count: usize) {
        assert!(count <= self.len);
        if count == self.len {
            self.clear();
            return;
        }
        self.head_skip += count;
        self.len -= count;
        if self.head_skip >= INDEX_BLOCK_ITEMS {
            let blocks = Arc::make_mut(&mut self.blocks);
            for _ in 0..self.head_skip / INDEX_BLOCK_ITEMS {
                blocks.pop_front();
            }
            self.head_skip %= INDEX_BLOCK_ITEMS;
        }
    }
}

impl<T: Clone> SharedIndex<T> {
    pub fn push(&mut self, value: T) {
        if self.active.len() == INDEX_BLOCK_ITEMS {
            Arc::make_mut(&mut self.blocks)
                .push_back(std::mem::replace(&mut self.active, Arc::new(Vec::new())));
        }
        Arc::make_mut(&mut self.active).push(value);
        self.len += 1;
    }

    pub fn resize(&mut self, len: usize, value: T) {
        if len < self.len {
            *self = self.iter().take(len).cloned().collect();
        } else {
            while self.len < len {
                self.push(value.clone());
            }
        }
    }
}

impl<T> Index<usize> for SharedIndex<T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        self.get(index).expect("history index out of bounds")
    }
}

impl<T: Clone> IndexMut<usize> for SharedIndex<T> {
    fn index_mut(&mut self, index: usize) -> &mut T {
        assert!(index < self.len, "history index out of bounds");
        let position = self.head_skip + index;
        let block = position / INDEX_BLOCK_ITEMS;
        let offset = position % INDEX_BLOCK_ITEMS;
        if block < self.blocks.len() {
            &mut Arc::make_mut(&mut Arc::make_mut(&mut self.blocks)[block])[offset]
        } else {
            &mut Arc::make_mut(&mut self.active)[offset]
        }
    }
}

pub struct SharedIndexIter<'a, T> {
    index: &'a SharedIndex<T>,
    range: Range<usize>,
}

pub struct SharedIndexWindow<'a, T> {
    index: &'a SharedIndex<T>,
    start: usize,
    len: usize,
}

impl<T> Index<usize> for SharedIndexWindow<'_, T> {
    type Output = T;

    fn index(&self, index: usize) -> &T {
        assert!(index < self.len, "history window index out of bounds");
        &self.index[self.start + index]
    }
}

impl<'a, T> Iterator for SharedIndexIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        self.range.next().map(|position| &self.index[position])
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.range.size_hint()
    }
}

impl<T> DoubleEndedIterator for SharedIndexIter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.range.next_back().map(|position| &self.index[position])
    }
}

impl<T> ExactSizeIterator for SharedIndexIter<'_, T> {}

impl<'a, T> IntoIterator for &'a SharedIndex<T> {
    type Item = &'a T;
    type IntoIter = SharedIndexIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<T: Clone> FromIterator<T> for SharedIndex<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        let mut index = Self::default();
        for value in iter {
            index.push(value);
        }
        index
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ActionLog {
    blocks: Vec<Arc<Vec<Action>>>,
    len: usize,
}

impl ActionLog {
    pub fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, action: Action) {
        if self.len.is_multiple_of(ACTIONS_PER_BLOCK) {
            self.blocks.push(Arc::new(Vec::new()));
        }
        Arc::make_mut(self.blocks.last_mut().unwrap()).push(action);
        self.len += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = &Action> {
        self.blocks.iter().flat_map(|block| block.iter())
    }

    pub fn get(&self, range: Range<usize>) -> Option<impl Iterator<Item = &Action>> {
        if range.start > range.end || range.end > self.len {
            return None;
        }
        let start_block = range.start / ACTIONS_PER_BLOCK;
        let end_block = range.end.div_ceil(ACTIONS_PER_BLOCK);
        let take = range.end - range.start;
        Some(
            self.blocks[start_block..end_block]
                .iter()
                .flat_map(|block| block.iter())
                .skip(range.start % ACTIONS_PER_BLOCK)
                .take(take),
        )
    }
}

#[derive(Debug)]
pub(super) struct Checkpoint {
    objects: BTreeMap<u64, Arc<Object>>,
    reference_lifetimes: BTreeMap<(u64, String), Arc<BTreeMap<u64, u32>>>,
    generations: Arc<BTreeMap<u64, u32>>,
    errors: Arc<Vec<ErrorEvent>>,
    debug_draw: BTreeMap<u64, Arc<Vec<debug_draw::Primitive>>>,
    contacts: BTreeMap<u64, Arc<[contacts::Contact]>>,
    cameras: Arc<BTreeMap<String, SourceCamera>>,
}

impl Checkpoint {
    pub fn new(state: &SceneState, previous: Option<&Self>) -> Self {
        let objects = state
            .objects
            .iter()
            .map(|(&id, object)| {
                let shared = previous
                    .and_then(|p| p.objects.get(&id))
                    .filter(|old| old.as_ref() == object)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(object.clone()));
                (id, shared)
            })
            .collect();
        let reference_lifetimes = state
            .reference_lifetimes
            .iter()
            .map(|(key, targets)| {
                let shared = previous
                    .and_then(|p| p.reference_lifetimes.get(key))
                    .filter(|old| old.as_ref() == targets.as_ref())
                    .cloned()
                    .unwrap_or_else(|| targets.clone());
                (key.clone(), shared)
            })
            .collect();
        let generations = previous
            .map(|p| &p.generations)
            .filter(|old| old.as_ref() == &state.generations)
            .cloned()
            .unwrap_or_else(|| Arc::new(state.generations.clone()));
        let errors = previous
            .map(|p| &p.errors)
            .filter(|old| old.as_ref() == &state.errors)
            .cloned()
            .unwrap_or_else(|| Arc::new(state.errors.clone()));
        let cameras = previous
            .map(|p| &p.cameras)
            .filter(|old| old.as_ref() == &state.cameras)
            .cloned()
            .unwrap_or_else(|| Arc::new(state.cameras.clone()));
        Self {
            objects,
            reference_lifetimes,
            generations,
            errors,
            debug_draw: state.debug_draw.clone(),
            contacts: state.contacts.clone(),
            cameras,
        }
    }

    pub fn restore(&self) -> SceneState {
        SceneState {
            objects: self
                .objects
                .iter()
                .map(|(&id, object)| (id, object.as_ref().clone()))
                .collect(),
            reference_lifetimes: self
                .reference_lifetimes
                .iter()
                .map(|(key, targets)| (key.clone(), targets.clone()))
                .collect(),
            generations: self.generations.as_ref().clone(),
            errors: self.errors.as_ref().clone(),
            debug_draw: self.debug_draw.clone(),
            contacts: self.contacts.clone(),
            cameras: self.cameras.as_ref().clone(),
        }
    }
}

pub(super) fn checkpoint_bytes(checkpoints: &BTreeMap<usize, Arc<Checkpoint>>) -> usize {
    let mut objects = BTreeSet::new();
    let mut bindings = BTreeSet::new();
    let mut generations = BTreeSet::new();
    let mut errors = BTreeSet::new();
    let mut cameras = BTreeSet::new();
    let mut total = 0;
    for checkpoint in checkpoints.values() {
        total += 128
            + checkpoint.objects.len() * 80
            + (checkpoint.debug_draw.len() + checkpoint.contacts.len()) * 80;
        for object in checkpoint.objects.values() {
            if objects.insert(Arc::as_ptr(object)) {
                total += std::mem::size_of::<Object>()
                    + 32
                    + object.type_name.len()
                    + object
                        .properties
                        .iter()
                        .map(|(key, value)| {
                            key.len()
                                + std::mem::size_of::<Value>()
                                + 64
                                + match value {
                                    Value::String(text) => text.len(),
                                    _ => 0,
                                }
                        })
                        .sum::<usize>();
            }
        }
        for ((_, key), targets) in &checkpoint.reference_lifetimes {
            total += 96 + key.len();
            if bindings.insert(Arc::as_ptr(targets)) {
                total += 32 + targets.len() * 80;
            }
        }
        if generations.insert(Arc::as_ptr(&checkpoint.generations)) {
            total += 32 + checkpoint.generations.len() * 64;
        }
        if errors.insert(Arc::as_ptr(&checkpoint.errors)) {
            total += 32
                + checkpoint
                    .errors
                    .iter()
                    .map(|event| {
                        std::mem::size_of::<ErrorEvent>() + event.message.len() + event.file.len()
                    })
                    .sum::<usize>();
        }
        if cameras.insert(Arc::as_ptr(&checkpoint.cameras)) {
            total += 32
                + checkpoint
                    .cameras
                    .keys()
                    .map(|name| camera_entry_bytes(name))
                    .sum::<usize>();
        }
    }
    total
}

pub(super) fn state_structure_bytes(state: &SceneState) -> usize {
    128 + state
        .objects
        .values()
        .map(|object| {
            80 + std::mem::size_of::<Object>()
                + object.type_name.len()
                + object
                    .properties
                    .iter()
                    .map(|(key, value)| {
                        key.len()
                            + std::mem::size_of::<Value>()
                            + 64
                            + match value {
                                Value::String(text) => text.len(),
                                _ => 0,
                            }
                    })
                    .sum::<usize>()
        })
        .sum::<usize>()
        + state
            .reference_lifetimes
            .iter()
            .map(|((_, key), bindings)| 96 + key.len() + bindings.len() * 80)
            .sum::<usize>()
        + state.generations.len() * 64
        + state
            .errors
            .iter()
            .map(|event| std::mem::size_of::<ErrorEvent>() + event.file.len() + event.message.len())
            .sum::<usize>()
        + (state.contacts.len() + state.debug_draw.len()) * 80
        + state
            .cameras
            .keys()
            .map(|name| camera_entry_bytes(name))
            .sum::<usize>()
}

pub(super) fn evict_checkpoint(checkpoints: &mut BTreeMap<usize, Arc<Checkpoint>>) -> bool {
    let keys: Vec<_> = checkpoints.keys().copied().collect();
    let index = if keys.len() > 2 {
        keys.windows(3)
            .min_by_key(|frames| frames[2] - frames[0])
            .map(|frames| frames[1])
    } else {
        keys.first().copied()
    };
    if let Some(index) = index {
        checkpoints.remove(&index);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_index_matches_deque_through_partial_eviction_and_snapshot_mutation() {
        let mut index = SharedIndex::default();
        let mut expected = VecDeque::new();
        let mut held = Vec::new();
        for value in 0..4200 {
            index.push(value);
            expected.push_back(value);
            if value % 17 == 0 {
                held.push((index.clone(), expected.clone()));
            }
            if value % 31 == 0 && index.len() > 79 {
                index.discard_front(79);
                expected.drain(..79);
            }
            assert_eq!(index.iter().copied().collect::<VecDeque<_>>(), expected);
            assert_eq!(index.first(), expected.front());
            assert_eq!(index.last(), expected.back());
            assert_eq!(
                index.iter().rev().copied().collect::<Vec<_>>(),
                expected.iter().rev().copied().collect::<Vec<_>>()
            );
            assert!(index.windows(2).all(|pair| pair[0] < pair[1]));
        }
        for (snapshot, expected) in held {
            assert_eq!(snapshot.iter().copied().collect::<VecDeque<_>>(), expected);
        }
        let snapshot = index.clone();
        index[0] = -1;
        assert_eq!(snapshot.first(), expected.front());
        index.resize(100, 0);
        assert_eq!(index.len(), 100);
        index.resize(110, -2);
        assert_eq!(index.last(), Some(&-2));
        index.discard_front(index.len());
        index.push(9);
        assert_eq!(index.first(), Some(&9));
    }

    #[test]
    fn shared_index_publication_clones_only_the_partial_block() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Debug)]
        struct Counted(Arc<AtomicUsize>);
        impl Clone for Counted {
            fn clone(&self) -> Self {
                self.0.fetch_add(1, Ordering::Relaxed);
                Self(self.0.clone())
            }
        }
        let copies = Arc::new(AtomicUsize::new(0));
        let mut index = SharedIndex::default();
        for _ in 0..1_000_000 {
            index.push(Counted(copies.clone()));
        }
        for _ in 0..60 {
            let snapshot = index.clone();
            assert!(Arc::ptr_eq(&index.blocks, &snapshot.blocks));
            let before = copies.load(Ordering::Relaxed);
            index.push(Counted(copies.clone()));
            assert!(copies.load(Ordering::Relaxed) - before < INDEX_BLOCK_ITEMS);
            assert_eq!(snapshot.len() + 1, index.len());
        }
        assert!(copies.load(Ordering::Relaxed) < 60 * INDEX_BLOCK_ITEMS);
    }

    #[test]
    fn snapshots_share_complete_blocks_and_keep_partial_blocks_immutable() {
        let mut history = ActionLog::default();
        for id in 0..1500 {
            history.push(Action::Destroy(id));
        }
        let snapshot = history.clone();
        history.push(Action::Destroy(1500));
        assert!(Arc::ptr_eq(&history.blocks[0], &snapshot.blocks[0]));
        assert!(!Arc::ptr_eq(&history.blocks[1], &snapshot.blocks[1]));
        assert_eq!(snapshot.len(), 1500);
        let ids = history
            .get(1000..1050)
            .unwrap()
            .map(|action| {
                let Action::Destroy(id) = action else {
                    unreachable!()
                };
                *id
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, (1000..1050).collect::<Vec<_>>());
        assert_eq!(history.get(1501..1501).unwrap().count(), 0);
        assert!(history.get(1500..1502).is_none());
    }
}
