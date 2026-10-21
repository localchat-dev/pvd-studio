use super::super::pages::{PageRead, PageWrite};
use super::*;

mod codec;
#[cfg(test)]
#[cfg(test)]
mod tests;
pub(in super::super) use codec::BufferCodec;

const NODE_MAGIC: &[u8; 8] = b"PVDBNP01";
const CHUNK_MAGIC: &[u8; 8] = b"PVDBCP01";
const NODE_BYTES: usize = 62;
const CHUNK_HEADER_BYTES: usize = 11;
const MAX_DEPENDENCIES: usize = CHUNK_LIMIT * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) struct NodeRef {
    pub page: u64,
    pub len: u64,
    pub height: u8,
}

impl NodeRef {
    fn empty() -> Self {
        Self {
            page: 0,
            len: 0,
            height: 0,
        }
    }
    fn validate(self) -> Result<(), Error> {
        if self.page == 0 {
            if self.len != 0 || self.height != 0 {
                return Err(error("empty sequence reference has nonempty metadata"));
            }
        } else if self.len == 0
            || self.len > crate::analysis::MAX_RECORDS as u64
            || self.height == 0
            || usize::from(self.height) >= MAX_HEIGHT
            || u64::from(self.height) > self.len
        {
            return Err(error("invalid sequence node reference"));
        }
        Ok(())
    }
    fn encode(self, out: &mut Encoder) {
        out.u64(self.page);
        out.u64(self.len);
        out.u8(self.height);
    }
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        let result = Self {
            page: input.u64()?,
            len: input.u64()?,
            height: input.u8()?,
        };
        result.validate()?;
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) struct BufferRef {
    pub kind: u8,
    pub root: NodeRef,
    pub capacity: u64,
}

impl BufferRef {
    pub(in super::super) fn encode(self, out: &mut Encoder) {
        out.u8(self.kind);
        self.root.encode(out);
        out.u64(self.capacity);
    }
    pub(in super::super) fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        let result = Self {
            kind: input.u8()?,
            root: NodeRef::decode(input)?,
            capacity: input.u64()?,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(self) -> Result<(), Error> {
        self.root.validate()?;
        if !(1..=5).contains(&self.kind)
            || self.capacity < self.root.len
            || self.capacity > crate::analysis::MAX_RECORDS as u64
        {
            return Err(error("invalid sequence descriptor"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChunkRef {
    page: u64,
    len: u16,
}

#[derive(Clone, Copy, Default, Debug)]
pub(in super::super) struct BufferStatistics {
    pub encoded_nodes: usize,
    pub encoded_chunks: usize,
    pub encoded_elements: usize,
    pub reused_nodes: usize,
    pub reused_chunks: usize,
    pub loaded_nodes: usize,
    pub loaded_chunks: usize,
}

pub(in super::super) struct BufferMemos<T, S> {
    domain: Domain,
    store: S,
    nodes: Cache<usize, Node<T>, NodeRef>,
    chunks: Cache<usize, Vec<T>, ChunkRef>,
    pub statistics: BufferStatistics,
}

impl<T, S: Clone> BufferMemos<T, S> {
    pub(in super::super) fn new(
        domain: &Domain,
        store: S,
        node_slots: usize,
        chunk_slots: usize,
    ) -> Result<Self, Error> {
        Ok(Self {
            domain: domain.clone(),
            store,
            nodes: Cache::dynamic(node_slots, domain)?,
            chunks: Cache::dynamic(chunk_slots, domain)?,
            statistics: BufferStatistics::default(),
        })
    }
    pub(in super::super) fn absorb<I: PageIo<Store = S>>(
        &mut self,
        other: Self,
        io: &I,
    ) -> Result<(), Error> {
        if !self.domain.same(&other.domain) || !io.same(&self.store) || !io.same(&other.store) {
            return Err(error(
                "sequence writer memos belong to another source or domain",
            ));
        }
        self.nodes.absorb(other.nodes)?;
        self.chunks.absorb(other.chunks)?;
        self.statistics = other.statistics;
        Ok(())
    }
    pub(in super::super) fn clear(&mut self) {
        self.nodes.clear();
        self.chunks.clear();
    }
    pub(in super::super) fn register<I: PageIo<Store = S>>(
        &self,
        registry: &mut BufferRegistry<T, S>,
        io: &I,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        if !self.domain.same(&registry.domain) || !io.same(&self.store) || !io.same(&registry.store)
        {
            return Err(error(
                "sequence registry belongs to another source or domain",
            ));
        }
        self.nodes.visit(|identity, node, reference| {
            check_replay_cancelled(cancelled)?;
            reference.validate()?;
            if identity != node.identity()
                || !node.domain().same(&self.domain)
                || reference.len != node.len as u64
                || reference.height != node.height
            {
                return Err(error("sequence node memo metadata disagrees"));
            }
            if let Ok(handle) = io.pin(reference.page)
                && io.committed(&handle)
            {
                registry.nodes.put(reference.page, node, reference);
            }
            Ok(())
        })?;
        self.chunks.visit(|identity, chunk, reference| {
            check_replay_cancelled(cancelled)?;
            if identity != chunk.identity()
                || !chunk.domain().same(&self.domain)
                || reference.page == 0
                || reference.len == 0
                || usize::from(reference.len) > CHUNK_LIMIT
                || usize::from(reference.len) != chunk.len()
            {
                return Err(error("sequence chunk memo metadata disagrees"));
            }
            if let Ok(handle) = io.pin(reference.page)
                && io.committed(&handle)
            {
                registry.chunks.put(reference.page, chunk, reference);
            }
            Ok(())
        })?;
        check_replay_cancelled(cancelled)
    }
    pub(in super::super) fn capacity_bytes(&self) -> usize {
        self.nodes.capacity_bytes() + self.chunks.capacity_bytes()
    }
}

pub(in super::super) struct BufferRegistry<T, S> {
    domain: Domain,
    store: S,
    nodes: Cache<u64, Node<T>, NodeRef>,
    chunks: Cache<u64, Vec<T>, ChunkRef>,
    pub statistics: BufferStatistics,
}

impl<T, S: Clone> BufferRegistry<T, S> {
    pub(in super::super) fn new(
        domain: &Domain,
        store: S,
        node_slots: usize,
        chunk_slots: usize,
    ) -> Result<Self, Error> {
        Ok(Self {
            domain: domain.clone(),
            store,
            nodes: Cache::live(node_slots, domain)?,
            chunks: Cache::live(chunk_slots, domain)?,
            statistics: BufferStatistics::default(),
        })
    }
    pub(in super::super) fn clear(&mut self) {
        self.nodes.clear();
        self.chunks.clear();
    }
    pub(in super::super) fn reclaim(
        &mut self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.nodes.reclaim_dead(cancelled)?;
        self.chunks.reclaim_dead(cancelled)
    }
    pub(in super::super) fn capacity_bytes(&self) -> usize {
        self.nodes.capacity_bytes() + self.chunks.capacity_bytes()
    }
}

pub(in super::super) fn write<T: BufferCodec, C: PageWrite>(
    buffer: &Buffer<T>,
    old: &BufferMemos<T, C::Store>,
    staged: &mut BufferMemos<T, C::Store>,
    cx: &mut C,
) -> Result<BufferRef, Error> {
    cx.check()?;
    if !buffer.domain.same(cx.domain())
        || !old.domain.same(cx.domain())
        || !staged.domain.same(cx.domain())
        || !cx.same(&old.store)
        || !cx.same(&staged.store)
    {
        return Err(error("sequence writer belongs to another source or domain"));
    }
    let root = match &buffer.root {
        Some(root) => write_node(root, old, staged, cx)?,
        None => NodeRef::empty(),
    };
    let result = BufferRef {
        kind: T::KIND,
        root,
        capacity: buffer.legacy_capacity.try_into()?,
    };
    result.validate()?;
    cx.check()?;
    Ok(result)
}

fn write_node<T: BufferCodec, C: PageWrite>(
    source: &Owned<Node<T>>,
    old: &BufferMemos<T, C::Store>,
    staged: &mut BufferMemos<T, C::Store>,
    cx: &mut C,
) -> Result<NodeRef, Error> {
    cx.check()?;
    if !source.domain().same(cx.domain()) || !source.chunk.domain().same(cx.domain()) {
        return Err(error("sequence node belongs to another domain"));
    }
    for (memo, is_staged) in [(&staged.nodes, true), (&old.nodes, false)] {
        if let Some((alive, reference)) = memo.get(source.identity())
            && alive.same(source)
            && reference.len == source.len as u64
            && reference.height == source.height
            && cx.usable(reference.page, is_staged)
        {
            increment(&mut staged.statistics.reused_nodes, 1)?;
            return Ok(reference);
        }
    }
    let left = match &source.left {
        Some(node) => write_node(node, old, staged, cx)?,
        None => NodeRef::empty(),
    };
    let right = match &source.right {
        Some(node) => write_node(node, old, staged, cx)?,
        None => NodeRef::empty(),
    };
    let chunk = write_chunk(&source.chunk, old, staged, cx)?;
    let declared = NodeRef {
        page: 1,
        len: source.len.try_into()?,
        height: source.height,
    };
    validate_parts(declared, left, right, chunk)?;
    let mut out = encoder(NODE_MAGIC, NODE_BYTES)?;
    out.u8(T::KIND);
    out.u64(declared.len);
    out.u8(declared.height);
    left.encode(&mut out);
    right.encode(&mut out);
    out.u64(chunk.page);
    out.u16(chunk.len);
    let mut dependencies = Dependencies::new();
    for id in [left.page, right.page, chunk.page] {
        dependencies.add(id)?;
    }
    let page = cx.page(&out.bytes, dependencies.sorted())?;
    if page == 0 {
        return Err(error("sequence writer returned an empty page"));
    }
    let reference = NodeRef { page, ..declared };
    staged.nodes.put(source.identity(), source, reference);
    increment(&mut staged.statistics.encoded_nodes, 1)?;
    Ok(reference)
}

fn write_chunk<T: BufferCodec, C: PageWrite>(
    source: &Owned<Vec<T>>,
    old: &BufferMemos<T, C::Store>,
    staged: &mut BufferMemos<T, C::Store>,
    cx: &mut C,
) -> Result<ChunkRef, Error> {
    cx.check()?;
    if source.is_empty() || source.len() > CHUNK_LIMIT || !source.domain().same(cx.domain()) {
        return Err(error("invalid sequence chunk owner"));
    }
    for (memo, is_staged) in [(&staged.chunks, true), (&old.chunks, false)] {
        if let Some((alive, reference)) = memo.get(source.identity())
            && alive.same(source)
            && usize::from(reference.len) == source.len()
            && cx.usable(reference.page, is_staged)
        {
            increment(&mut staged.statistics.reused_chunks, 1)?;
            return Ok(reference);
        }
    }
    let bytes = add(
        CHUNK_HEADER_BYTES,
        source
            .len()
            .checked_mul(T::WIDTH)
            .ok_or_else(|| error("sequence chunk byte count overflow"))?,
    )?;
    let mut out = encoder(CHUNK_MAGIC, bytes)?;
    out.u8(T::KIND);
    out.u16(source.len().try_into()?);
    let mut dependencies = T::encode(source, &mut out, cx)?;
    if out.bytes.len() != bytes {
        return Err(error("sequence codec emitted the wrong width"));
    }
    let page = cx.page(&out.bytes, dependencies.sorted())?;
    if page == 0 {
        return Err(error("sequence writer returned an empty chunk page"));
    }
    let reference = ChunkRef {
        page,
        len: source.len().try_into()?,
    };
    staged.chunks.put(source.identity(), source, reference);
    increment(&mut staged.statistics.encoded_chunks, 1)?;
    increment(&mut staged.statistics.encoded_elements, source.len())?;
    Ok(reference)
}

pub(in super::super) fn read<T: BufferCodec, C: PageRead>(
    reference: BufferRef,
    registry: &mut BufferRegistry<T, C::Store>,
    cx: &mut C,
) -> Result<Buffer<T>, Error> {
    cx.check()?;
    reference.validate()?;
    if reference.kind != T::KIND || !registry.domain.same(cx.domain()) || !cx.same(&registry.store)
    {
        return Err(error(
            "sequence reader has the wrong type, source or domain",
        ));
    }
    let root = read_node(reference.root, registry, cx)?;
    cx.check()?;
    Ok(Buffer {
        domain: cx.domain().clone(),
        root,
        legacy_capacity: reference.capacity.try_into()?,
    })
}

fn read_node<T: BufferCodec, C: PageRead>(
    reference: NodeRef,
    registry: &mut BufferRegistry<T, C::Store>,
    cx: &mut C,
) -> Result<Link<T>, Error> {
    cx.check()?;
    reference.validate()?;
    if reference.page == 0 {
        return Ok(None);
    }
    if let Some((node, cached)) = registry.nodes.get(reference.page) {
        if cached != reference || !node.domain().same(cx.domain()) {
            return Err(error("sequence cached node metadata does not match"));
        }
        return Ok(Some(node));
    }
    let (left, right, chunk) = {
        let page = cx.load(reference.page, NODE_BYTES)?;
        increment(&mut registry.statistics.loaded_nodes, 1)?;
        let mut input = Decoder::new(&page.loaded.bytes, NODE_MAGIC)?;
        if input.u8()? != T::KIND
            || input.u64()? != reference.len
            || input.u8()? != reference.height
        {
            return Err(error("sequence node metadata does not match its reference"));
        }
        let left = NodeRef::decode(&mut input)?;
        let right = NodeRef::decode(&mut input)?;
        let chunk = ChunkRef {
            page: input.u64()?,
            len: input.u16()?,
        };
        input.finish()?;
        validate_parts(reference, left, right, chunk)?;
        let mut dependencies = Dependencies::new();
        for id in [left.page, right.page, chunk.page] {
            dependencies.add(id)?;
        }
        dependencies.validate(&page.loaded.dependencies)?;
        (left, right, chunk)
    };
    let left = read_node(left, registry, cx)?;
    let right = read_node(right, registry, cx)?;
    let chunk = read_chunk(chunk, registry, cx)?;
    let result = node(left, chunk, right, cx.domain())?;
    if result.len as u64 != reference.len || result.height != reference.height {
        return Err(error("sequence decoded node metadata does not match"));
    }
    cx.check()?;
    registry.nodes.put(reference.page, &result, reference);
    Ok(Some(result))
}

fn read_chunk<T: BufferCodec, C: PageRead>(
    reference: ChunkRef,
    registry: &mut BufferRegistry<T, C::Store>,
    cx: &mut C,
) -> Result<Owned<Vec<T>>, Error> {
    cx.check()?;
    if reference.page == 0 || reference.len == 0 || usize::from(reference.len) > CHUNK_LIMIT {
        return Err(error("invalid sequence chunk reference"));
    }
    if let Some((chunk, cached)) = registry.chunks.get(reference.page) {
        if cached != reference || !chunk.domain().same(cx.domain()) {
            return Err(error("sequence cached chunk metadata does not match"));
        }
        return Ok(chunk);
    }
    let count = usize::from(reference.len);
    let length = add(
        CHUNK_HEADER_BYTES,
        count
            .checked_mul(T::WIDTH)
            .ok_or_else(|| error("sequence chunk byte count overflow"))?,
    )?;
    let chunk = {
        let page = cx.load(reference.page, length)?;
        increment(&mut registry.statistics.loaded_chunks, 1)?;
        let mut input = Decoder::new(&page.loaded.bytes, CHUNK_MAGIC)?;
        if input.u8()? != T::KIND || input.u16()? != reference.len {
            return Err(error("sequence chunk metadata does not match"));
        }
        let (chunk, mut dependencies) = T::decode(&mut input, count, cx)?;
        input.finish()?;
        dependencies.validate(&page.loaded.dependencies)?;
        chunk
    };
    cx.check()?;
    registry.chunks.put(reference.page, &chunk, reference);
    Ok(chunk)
}

fn validate_parts(
    reference: NodeRef,
    left: NodeRef,
    right: NodeRef,
    chunk: ChunkRef,
) -> Result<(), Error> {
    reference.validate()?;
    left.validate()?;
    right.validate()?;
    let len = left
        .len
        .checked_add(right.len)
        .and_then(|len| len.checked_add(u64::from(chunk.len)));
    if reference.page == 0
        || chunk.page == 0
        || chunk.len == 0
        || usize::from(chunk.len) > CHUNK_LIMIT
        || len != Some(reference.len)
        || left.len >= reference.len
        || right.len >= reference.len
        || left.height >= reference.height
        || right.height >= reference.height
        || left.height.abs_diff(right.height) > 1
        || left.height.max(right.height).checked_add(1) != Some(reference.height)
    {
        return Err(error("invalid sequence page tree shape"));
    }
    Ok(())
}

fn increment(counter: &mut usize, amount: usize) -> Result<(), Error> {
    *counter = counter
        .checked_add(amount)
        .ok_or_else(|| error("sequence page statistics overflow"))?;
    Ok(())
}

fn encoder(magic: &[u8; 8], bytes: usize) -> Result<Encoder, Error> {
    if !(8..=MAX_PAGE_BYTES).contains(&bytes) {
        return Err(error("sequence page exceeds its byte limit"));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(bytes)?;
    if out.capacity() != bytes {
        return Err(error("sequence encoding exceeds its scratch allowance"));
    }
    out.extend_from_slice(magic);
    Ok(Encoder { bytes: out })
}

pub(in super::super) struct Dependencies {
    ids: [u64; MAX_DEPENDENCIES],
    len: usize,
}
impl Dependencies {
    fn new() -> Self {
        Self {
            ids: [0; MAX_DEPENDENCIES],
            len: 0,
        }
    }
    fn add(&mut self, id: u64) -> Result<(), Error> {
        if id == 0 {
            return Ok(());
        }
        if self.len == self.ids.len() {
            return Err(error("sequence page dependency limit exceeded"));
        }
        self.ids[self.len] = id;
        self.len += 1;
        Ok(())
    }
    fn sorted(&mut self) -> &[u64] {
        self.ids[..self.len].sort_unstable();
        let mut output = 0;
        for index in 0..self.len {
            if output == 0 || self.ids[index] != self.ids[output - 1] {
                self.ids[output] = self.ids[index];
                output += 1;
            }
        }
        self.len = output;
        &self.ids[..self.len]
    }
    fn validate(&mut self, actual: &[u64]) -> Result<(), Error> {
        if self.sorted() != actual {
            return Err(error("sequence page dependency list does not match"));
        }
        Ok(())
    }
}
