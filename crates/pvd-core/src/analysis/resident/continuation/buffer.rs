use super::containers::Item;
use super::*;
use serde::ser::SerializeSeq;
use std::{iter::FusedIterator, ops::Range};

pub(super) mod pages;
#[cfg(test)]
mod tests;

const CHUNK_LIMIT: usize = 256;
const MAX_HEIGHT: usize = 64;
type Link<T> = Option<Owned<Node<T>>>;

struct Node<T> {
    left: Link<T>,
    chunk: Owned<Vec<T>>,
    right: Link<T>,
    len: usize,
    height: u8,
}

fn length<T>(root: &Link<T>) -> usize {
    root.as_ref().map_or(0, |node| node.len)
}

fn height<T>(root: &Link<T>) -> u8 {
    root.as_ref().map_or(0, |node| node.height)
}

fn same_link<T>(left: &Link<T>, right: &Link<T>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => left.same(right),
        _ => false,
    }
}

fn node<T>(
    left: Link<T>,
    chunk: Owned<Vec<T>>,
    right: Link<T>,
    domain: &Domain,
) -> Result<Owned<Node<T>>, Error> {
    if chunk.is_empty() || chunk.len() > CHUNK_LIMIT || !chunk.domain().same(domain) {
        return Err(error("invalid resident sequence chunk"));
    }
    if left
        .as_ref()
        .is_some_and(|node| !node.domain().same(domain))
        || right
            .as_ref()
            .is_some_and(|node| !node.domain().same(domain))
    {
        return Err(error("resident sequence cannot join foreign nodes"));
    }
    let len = add(add(length(&left), chunk.len())?, length(&right))?;
    if len > crate::analysis::MAX_RECORDS {
        return Err(error("resident buffer exceeds its record limit"));
    }
    let height = height(&left).max(height(&right)) + 1;
    if usize::from(height) >= MAX_HEIGHT {
        return Err(error("resident sequence height exceeds its limit"));
    }
    let result = Owned::new(
        Node {
            left,
            chunk,
            right,
            len,
            height,
        },
        domain,
        0,
    )?;
    owner::cumulative(&domain.0.buffer_allocated_nodes, 1);
    Ok(result)
}

fn balance<T>(
    left: Link<T>,
    chunk: Owned<Vec<T>>,
    right: Link<T>,
    domain: &Domain,
) -> Result<Owned<Node<T>>, Error> {
    let delta = i16::from(height(&left)) - i16::from(height(&right));
    if delta > 1 {
        let left = left.as_ref().unwrap();
        if height(&left.left) >= height(&left.right) {
            let right = node(left.right.clone(), chunk, right, domain)?;
            node(left.left.clone(), left.chunk.clone(), Some(right), domain)
        } else {
            let middle = left.right.as_ref().unwrap();
            let left = node(
                left.left.clone(),
                left.chunk.clone(),
                middle.left.clone(),
                domain,
            )?;
            let right = node(middle.right.clone(), chunk, right, domain)?;
            node(Some(left), middle.chunk.clone(), Some(right), domain)
        }
    } else if delta < -1 {
        let right = right.as_ref().unwrap();
        if height(&right.right) >= height(&right.left) {
            let left = node(left, chunk, right.left.clone(), domain)?;
            node(Some(left), right.chunk.clone(), right.right.clone(), domain)
        } else {
            let middle = right.left.as_ref().unwrap();
            let left = node(left, chunk, middle.left.clone(), domain)?;
            let right = node(
                middle.right.clone(),
                right.chunk.clone(),
                right.right.clone(),
                domain,
            )?;
            node(Some(left), middle.chunk.clone(), Some(right), domain)
        }
    } else {
        node(left, chunk, right, domain)
    }
}

fn join<T>(
    left: Link<T>,
    chunk: Owned<Vec<T>>,
    right: Link<T>,
    domain: &Domain,
) -> Result<Owned<Node<T>>, Error> {
    let delta = i16::from(height(&left)) - i16::from(height(&right));
    if delta > 1 {
        let top = left.as_ref().unwrap();
        let right = join(top.right.clone(), chunk, right, domain)?;
        balance(top.left.clone(), top.chunk.clone(), Some(right), domain)
    } else if delta < -1 {
        let top = right.as_ref().unwrap();
        let left = join(left, chunk, top.left.clone(), domain)?;
        balance(Some(left), top.chunk.clone(), top.right.clone(), domain)
    } else {
        node(left, chunk, right, domain)
    }
}

fn take_last<T>(root: &Owned<Node<T>>, domain: &Domain) -> Result<(Link<T>, Owned<Vec<T>>), Error> {
    match &root.right {
        Some(right) => {
            let (right, chunk) = take_last(right, domain)?;
            Ok((
                Some(join(root.left.clone(), root.chunk.clone(), right, domain)?),
                chunk,
            ))
        }
        None => Ok((root.left.clone(), root.chunk.clone())),
    }
}

fn concatenate<T>(left: Link<T>, right: Link<T>, domain: &Domain) -> Result<Link<T>, Error> {
    let Some(last) = &left else { return Ok(right) };
    if right.is_none() {
        return Ok(left);
    }
    let (left, chunk) = take_last(last, domain)?;
    Ok(Some(join(left, chunk, right, domain)?))
}

pub(super) struct Buffer<T> {
    domain: Domain,
    root: Link<T>,
    legacy_capacity: usize,
}

impl<T> Clone for Buffer<T> {
    fn clone(&self) -> Self {
        Self {
            domain: self.domain.clone(),
            root: self.root.clone(),
            legacy_capacity: self.legacy_capacity,
        }
    }
}

impl<T> Item for Buffer<T> {
    fn same_domain(&self, domain: &Domain) -> bool {
        self.domain.same(domain)
    }
}

impl<T> Buffer<T> {
    pub(super) fn empty(domain: &Domain) -> Result<Self, Error> {
        Ok(Self {
            domain: domain.clone(),
            root: None,
            legacy_capacity: 0,
        })
    }
    pub(super) fn domain(&self) -> Domain {
        self.domain.clone()
    }
    pub(super) fn view(&self) -> BufferView<'_, T> {
        BufferView::new(&self.root, &[])
    }
    pub(super) fn len(&self) -> usize {
        length(&self.root)
    }
    pub(super) fn capacity(&self) -> usize {
        self.legacy_capacity
    }
    #[cfg(test)]
    pub(super) fn same(&self, other: &Self) -> bool {
        self.domain.same(&other.domain)
            && same_link(&self.root, &other.root)
            && self.legacy_capacity == other.legacy_capacity
    }
}

impl<T: Clone + Item> Buffer<T> {
    pub(super) fn edit(
        &self,
        domain: &Domain,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<BufferDraft<T>, Error> {
        check_replay_cancelled(cancelled)?;
        if !self.domain.same(domain) {
            return Err(error("resident buffer belongs to another domain"));
        }
        Ok(BufferDraft {
            domain: domain.clone(),
            root: self.root.clone(),
            tail: None,
            len: self.len(),
            legacy_capacity: self.len(),
            failed: false,
        })
    }
}

impl<T: Serialize> Serialize for Buffer<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len()))?;
        for value in self.view().iter() {
            sequence.serialize_element(value)?;
        }
        sequence.end()
    }
}

pub(crate) struct BufferView<'a, T> {
    root: Option<&'a Node<T>>,
    tail: &'a [T],
    tree_len: usize,
}

impl<T> Copy for BufferView<'_, T> {}
impl<T> Clone for BufferView<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, T> BufferView<'a, T> {
    fn new(root: &'a Link<T>, tail: &'a [T]) -> Self {
        Self {
            root: root.as_deref(),
            tail,
            tree_len: length(root),
        }
    }
    pub(crate) fn len(self) -> usize {
        self.tree_len + self.tail.len()
    }
    #[cfg(test)]
    pub(crate) fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub(crate) fn first(self) -> Option<&'a T> {
        self.get(0)
    }
    pub(crate) fn last(self) -> Option<&'a T> {
        self.len().checked_sub(1).and_then(|index| self.get(index))
    }
    pub(crate) fn get(self, mut index: usize) -> Option<&'a T> {
        if index >= self.tree_len {
            return self.tail.get(index - self.tree_len);
        }
        let mut next = self.root;
        while let Some(node) = next {
            let before = length(&node.left);
            if index < before {
                next = node.left.as_deref();
            } else if index < before + node.chunk.len() {
                return node.chunk.get(index - before);
            } else {
                index -= before + node.chunk.len();
                next = node.right.as_deref();
            }
        }
        None
    }
    pub(crate) fn iter(self) -> BufferIter<'a, T> {
        self.iter_range(0..self.len())
    }
    pub(crate) fn range(self, range: Range<usize>) -> Result<BufferIter<'a, T>, Error> {
        if range.start > range.end || range.end > self.len() {
            return Err(error("resident sequence range is out of bounds"));
        }
        Ok(self.iter_range(range))
    }
    fn iter_range(self, range: Range<usize>) -> BufferIter<'a, T> {
        let tree_end = range.end.min(self.tree_len);
        BufferIter {
            front: range.start,
            back: range.end,
            tree_len: self.tree_len,
            tail: self.tail,
            forward: if range.start < tree_end {
                Cursor::at(self.root, range.start, false)
            } else {
                Cursor::empty(false)
            },
            reverse: if range.start < tree_end {
                Cursor::at(self.root, tree_end - 1, true)
            } else {
                Cursor::empty(true)
            },
        }
    }
    pub(crate) fn partition_point(self, mut predicate: impl FnMut(&T) -> bool) -> usize {
        let (mut first, mut end) = (0, self.len());
        while first < end {
            let middle = first + (end - first) / 2;
            if predicate(self.get(middle).unwrap()) {
                first = middle + 1;
            } else {
                end = middle;
            }
        }
        first
    }
}

struct Cursor<'a, T> {
    stack: [Option<&'a Node<T>>; MAX_HEIGHT],
    depth: usize,
    offset: usize,
    reverse: bool,
}

impl<'a, T> Cursor<'a, T> {
    fn empty(reverse: bool) -> Self {
        Self {
            stack: [None; MAX_HEIGHT],
            depth: 0,
            offset: 0,
            reverse,
        }
    }
    fn push(&mut self, node: &'a Node<T>) {
        self.stack[self.depth] = Some(node);
        self.depth += 1;
    }
    fn at(mut root: Option<&'a Node<T>>, mut index: usize, reverse: bool) -> Self {
        let mut result = Self::empty(reverse);
        while let Some(node) = root {
            let before = length(&node.left);
            if index < before {
                if !reverse {
                    result.push(node);
                }
                root = node.left.as_deref();
            } else if index >= before + node.chunk.len() {
                if reverse {
                    result.push(node);
                }
                index -= before + node.chunk.len();
                root = node.right.as_deref();
            } else {
                result.push(node);
                result.offset = index - before;
                break;
            }
        }
        result
    }
    fn next(&mut self) -> Option<&'a T> {
        let current = self
            .stack
            .get(self.depth.checked_sub(1)?)?
            .as_ref()
            .copied()?;
        let value = &current.chunk[self.offset];
        if self.reverse && self.offset > 0 {
            self.offset -= 1;
        } else if !self.reverse && self.offset + 1 < current.chunk.len() {
            self.offset += 1;
        } else {
            self.depth -= 1;
            self.stack[self.depth] = None;
            let mut next = if self.reverse {
                current.left.as_deref()
            } else {
                current.right.as_deref()
            };
            while let Some(node) = next {
                self.push(node);
                next = if self.reverse {
                    node.right.as_deref()
                } else {
                    node.left.as_deref()
                };
            }
            if let Some(index) = self.depth.checked_sub(1) {
                let node = self.stack[index].unwrap();
                self.offset = if self.reverse {
                    node.chunk.len() - 1
                } else {
                    0
                };
            }
        }
        Some(value)
    }
}

pub(crate) struct BufferIter<'a, T> {
    forward: Cursor<'a, T>,
    reverse: Cursor<'a, T>,
    tail: &'a [T],
    front: usize,
    back: usize,
    tree_len: usize,
}
impl<'a, T> Iterator for BufferIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        let value = if self.front < self.tree_len {
            self.forward.next()
        } else {
            self.tail.get(self.front - self.tree_len)
        };
        self.front += 1;
        value
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.back - self.front;
        (len, Some(len))
    }
}
impl<T> DoubleEndedIterator for BufferIter<'_, T> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.front == self.back {
            return None;
        }
        self.back -= 1;
        if self.back < self.tree_len {
            self.reverse.next()
        } else {
            self.tail.get(self.back - self.tree_len)
        }
    }
}
impl<T> ExactSizeIterator for BufferIter<'_, T> {}
impl<T> FusedIterator for BufferIter<'_, T> {}

struct Tail<T> {
    values: Vec<T>,
    memory: MemoryLease,
}
impl<T> Tail<T> {
    fn new(domain: &Domain, capacity: usize) -> Result<Self, Error> {
        if capacity > CHUNK_LIMIT {
            return Err(error("resident chunk exceeds its capacity limit"));
        }
        let payload = capacity
            .checked_mul(size_of::<T>())
            .ok_or_else(|| error("resident chunk size overflow"))?;
        let memory = domain
            .0
            .reserve(add(Owned::<Vec<T>>::fixed_bytes(), payload)?)?;
        let mut values = Vec::new();
        values.try_reserve_exact(capacity)?;
        if values.capacity() != capacity {
            return Err(error("resident chunk capacity exceeds its reservation"));
        }
        Ok(Self { values, memory })
    }
    fn grow(&mut self, domain: &Domain) -> Result<(), Error> {
        let capacity = self
            .values
            .capacity()
            .max(2)
            .checked_mul(2)
            .ok_or_else(|| error("resident chunk capacity overflow"))?
            .min(CHUNK_LIMIT);
        let mut next = Self::new(domain, capacity)?;
        let moved = self.values.len();
        next.values.append(&mut self.values);
        owner::cumulative(&domain.0.buffer_moved_elements, moved);
        owner::cumulative(&domain.0.buffer_moved_layout_bytes, moved * size_of::<T>());
        *self = next;
        Ok(())
    }
    fn finish(self) -> Result<Owned<Vec<T>>, Error> {
        let payload = self.values.capacity() * size_of::<T>();
        Owned::from_reserved(self.values, self.memory, payload)
    }
}
impl<T: Clone> Tail<T> {
    fn copy(values: &[T], domain: &Domain) -> Result<Self, Error> {
        let mut result = Self::new(domain, values.len())?;
        for value in values {
            result.values.push(value.clone());
            owner::cumulative(&domain.0.buffer_copied_elements, 1);
            owner::cumulative(&domain.0.buffer_copied_layout_bytes, size_of::<T>());
        }
        Ok(result)
    }
}

pub(super) struct BufferDraft<T> {
    domain: Domain,
    root: Link<T>,
    tail: Option<Tail<T>>,
    len: usize,
    legacy_capacity: usize,
    failed: bool,
}

impl<T> BufferDraft<T> {
    #[cfg(test)]
    pub(super) fn new(domain: &Domain, capacity: usize) -> Result<Self, Error> {
        if capacity > crate::analysis::MAX_RECORDS {
            return Err(error("resident buffer exceeds its record limit"));
        }
        Ok(Self {
            domain: domain.clone(),
            root: None,
            tail: None,
            len: 0,
            legacy_capacity: capacity,
            failed: false,
        })
    }
    pub(super) fn len(&self) -> usize {
        self.len
    }
    #[cfg(test)]
    pub(super) fn view(&self) -> BufferView<'_, T> {
        BufferView::new(
            &self.root,
            self.tail
                .as_ref()
                .map_or(&[], |tail| tail.values.as_slice()),
        )
    }
    #[cfg(test)]
    pub(super) fn last(&self) -> Option<&T> {
        self.view().last()
    }
    fn valid(&self) -> Result<(), Error> {
        if self.failed {
            Err(error("resident buffer draft has failed"))
        } else {
            Ok(())
        }
    }
    fn fail<R>(&mut self, result: Result<R, Error>) -> Result<R, Error> {
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn flush_tail(&mut self) -> Result<(), Error> {
        if let Some(tail) = self.tail.take().filter(|tail| !tail.values.is_empty()) {
            let chunk = tail.finish()?;
            self.root = Some(join(self.root.clone(), chunk, None, &self.domain)?);
        }
        Ok(())
    }
    pub(super) fn compact(&mut self) -> Result<(), Error> {
        self.valid()?;
        self.legacy_capacity = self.len;
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<Buffer<T>, Error> {
        self.valid()?;
        self.flush_tail()?;
        Ok(Buffer {
            domain: self.domain,
            root: self.root,
            legacy_capacity: self.legacy_capacity,
        })
    }
}

impl<T: Clone + Item> BufferDraft<T> {
    fn ensure_tail(&mut self) -> Result<(), Error> {
        if self.tail.is_none() {
            if let Some(root) = &self.root {
                let (root, chunk) = take_last(root, &self.domain)?;
                let tail = Tail::copy(&chunk, &self.domain)?;
                self.root = root;
                self.tail = Some(tail);
            } else {
                self.tail = Some(Tail::new(&self.domain, 0)?);
            }
        }
        Ok(())
    }
    pub(super) fn push(&mut self, value: T) -> Result<(), Error> {
        self.valid()?;
        let result = self.push_inner(value);
        self.fail(result)
    }
    fn push_inner(&mut self, value: T) -> Result<(), Error> {
        if !value.same_domain(&self.domain) {
            return Err(error("resident buffer cannot adopt a foreign value"));
        }
        if self.len >= crate::analysis::MAX_RECORDS {
            return Err(error("resident buffer exceeds its record limit"));
        }
        let capacity = if self.len == self.legacy_capacity {
            self.legacy_capacity
                .max(2)
                .checked_mul(2)
                .ok_or_else(|| error("resident buffer capacity overflow"))?
                .min(crate::analysis::MAX_RECORDS)
        } else {
            self.legacy_capacity
        };
        if self.tail.is_none()
            && self.root.as_ref().is_some_and(|root| {
                let mut last = root;
                while let Some(right) = &last.right {
                    last = right;
                }
                last.chunk.len() == CHUNK_LIMIT
            })
        {
            self.tail = Some(Tail::new(&self.domain, 0)?);
        } else {
            self.ensure_tail()?;
        }
        if self.tail.as_ref().unwrap().values.len() == CHUNK_LIMIT {
            self.flush_tail()?;
            self.tail = Some(Tail::new(&self.domain, 0)?);
        }
        let tail = self.tail.as_mut().unwrap();
        if tail.values.len() == tail.values.capacity() {
            tail.grow(&self.domain)?;
        }
        tail.values.push(value);
        self.len += 1;
        self.legacy_capacity = capacity;
        Ok(())
    }
    pub(super) fn pop(&mut self) -> Result<Option<T>, Error> {
        self.valid()?;
        let result = self.pop_inner();
        self.fail(result)
    }
    fn pop_inner(&mut self) -> Result<Option<T>, Error> {
        if self.len == 0 {
            return Ok(None);
        }
        self.ensure_tail()?;
        let result = self.tail.as_mut().unwrap().values.pop();
        self.len -= 1;
        if self.tail.as_ref().unwrap().values.is_empty() {
            self.tail = None;
        }
        Ok(result)
    }
    pub(super) fn remove_prefix(&mut self, count: usize) -> Result<(), Error> {
        self.valid()?;
        let result = (|| {
            if count > self.len {
                return Err(error("resident sequence prefix exceeds its length"));
            }
            if count == 0 {
                return Ok(());
            }
            self.flush_tail()?;
            self.root = cut_prefix(&self.root, count, &self.domain)?;
            self.len -= count;
            Ok(())
        })();
        self.fail(result)
    }
    pub(super) fn retain(
        &mut self,
        mut keep: impl FnMut(&T) -> bool,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.valid()?;
        let result = (|| {
            check_replay_cancelled(cancelled)?;
            self.flush_tail()?;
            self.root = filter(&self.root, &mut keep, cancelled, &self.domain)?;
            self.len = length(&self.root);
            Ok(())
        })();
        self.fail(result)
    }
}

fn cut_prefix<T: Clone>(root: &Link<T>, count: usize, domain: &Domain) -> Result<Link<T>, Error> {
    if count == 0 {
        return Ok(root.clone());
    }
    if count == length(root) {
        return Ok(None);
    }
    let current = root.as_ref().unwrap();
    let before = length(&current.left);
    if count < before {
        let left = cut_prefix(&current.left, count, domain)?;
        Ok(Some(join(
            left,
            current.chunk.clone(),
            current.right.clone(),
            domain,
        )?))
    } else if count == before {
        Ok(Some(join(
            None,
            current.chunk.clone(),
            current.right.clone(),
            domain,
        )?))
    } else if count < before + current.chunk.len() {
        let chunk = Tail::copy(&current.chunk[count - before..], domain)?.finish()?;
        Ok(Some(join(None, chunk, current.right.clone(), domain)?))
    } else {
        cut_prefix(&current.right, count - before - current.chunk.len(), domain)
    }
}

fn filter<T: Clone>(
    root: &Link<T>,
    keep: &mut impl FnMut(&T) -> bool,
    cancelled: &mut dyn FnMut() -> bool,
    domain: &Domain,
) -> Result<Link<T>, Error> {
    let Some(current) = root else { return Ok(None) };
    check_replay_cancelled(cancelled)?;
    let left = filter(&current.left, keep, cancelled, domain)?;
    let mut mask = [false; CHUNK_LIMIT];
    let mut count = 0;
    for (index, value) in current.chunk.iter().enumerate() {
        mask[index] = keep(value);
        count += usize::from(mask[index]);
    }
    let right = filter(&current.right, keep, cancelled, domain)?;
    check_replay_cancelled(cancelled)?;
    if count == 0 {
        return concatenate(left, right, domain);
    }
    if count == current.chunk.len() {
        if same_link(&left, &current.left) && same_link(&right, &current.right) {
            return Ok(root.clone());
        }
        return Ok(Some(join(left, current.chunk.clone(), right, domain)?));
    }
    let mut tail = Tail::new(domain, count)?;
    for (index, value) in current.chunk.iter().enumerate() {
        if mask[index] {
            tail.values.push(value.clone());
            owner::cumulative(&domain.0.buffer_copied_elements, 1);
            owner::cumulative(&domain.0.buffer_copied_layout_bytes, size_of::<T>());
        }
    }
    Ok(Some(join(left, tail.finish()?, right, domain)?))
}

pub(super) struct BufferEdit<T> {
    source: Buffer<T>,
    draft: Option<BufferDraft<T>>,
    failed: bool,
}
impl<T: Clone + Item> BufferEdit<T> {
    pub(super) fn new(source: &Buffer<T>) -> Self {
        Self {
            source: source.clone(),
            draft: None,
            failed: false,
        }
    }
    pub(super) fn len(&self) -> usize {
        self.draft
            .as_ref()
            .map_or_else(|| self.source.len(), BufferDraft::len)
    }
    pub(super) fn edit(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<&mut BufferDraft<T>, Error> {
        if self.failed {
            return Err(error("resident buffer edit has failed"));
        }
        if let Err(error) = check_replay_cancelled(cancelled) {
            self.failed = true;
            return Err(error);
        }
        if self.draft.is_none() {
            match self.source.edit(&self.source.domain(), cancelled) {
                Ok(draft) => self.draft = Some(draft),
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        Ok(self.draft.as_mut().unwrap())
    }
    pub(super) fn push(
        &mut self,
        value: T,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.edit(cancelled)?.push(value)
    }
    pub(super) fn finish(self) -> Result<Buffer<T>, Error> {
        if self.failed {
            return Err(error("resident buffer edit has failed"));
        }
        self.draft
            .map_or_else(|| Ok(self.source), BufferDraft::finish)
    }
}
