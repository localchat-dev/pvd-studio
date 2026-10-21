use super::super::pages::{PageRead, PageWrite};
use super::*;

type DecodedNode<K, V> = Option<(Owned<Node<K, V>>, TreeRef<K>)>;

const MAGIC: &[u8; 8] = b"PVDMTP01";
const BAND: u8 = 5;
const ROWS: usize = (1 << BAND) - 1;
const SCRATCH_BYTES: usize = 64 << 10;

pub(in super::super) trait KeyCodec: TreeKey {
    const ZERO: Self;
    fn encode(self, out: &mut Encoder);
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error>;
}
impl KeyCodec for u64 {
    const ZERO: Self = 0;
    fn encode(self, out: &mut Encoder) {
        out.u64(self);
    }
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        input.u64()
    }
}
impl KeyCodec for u16 {
    const ZERO: Self = 0;
    fn encode(self, out: &mut Encoder) {
        out.u16(self);
    }
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        input.u16()
    }
}
impl KeyCodec for (u64, u16) {
    const ZERO: Self = (0, 0);
    fn encode(self, out: &mut Encoder) {
        out.u64(self.0);
        out.u16(self.1);
    }
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        Ok((input.u64()?, input.u16()?))
    }
}
impl KeyCodec for (u64, u32) {
    const ZERO: Self = (0, 0);
    fn encode(self, out: &mut Encoder) {
        out.u64(self.0);
        out.u32(self.1);
    }
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        Ok((input.u64()?, input.u32()?))
    }
}
impl KeyCodec for (u64, u16, u32, u64) {
    const ZERO: Self = (0, 0, 0, 0);
    fn encode(self, out: &mut Encoder) {
        out.u64(self.0);
        out.u16(self.1);
        out.u32(self.2);
        out.u64(self.3);
    }
    fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        Ok((input.u64()?, input.u16()?, input.u32()?, input.u64()?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) struct TreeRef<K> {
    pub page: u64,
    pub len: u64,
    pub height: u8,
    pub minimum: K,
    pub maximum: K,
}
impl<K: KeyCodec> TreeRef<K> {
    fn empty() -> Self {
        Self {
            page: 0,
            len: 0,
            height: 0,
            minimum: K::ZERO,
            maximum: K::ZERO,
        }
    }
    pub(in super::super) fn encode(self, out: &mut Encoder) {
        out.u64(self.page);
        out.u64(self.len);
        out.u8(self.height);
        self.minimum.encode(out);
        self.maximum.encode(out);
    }
    pub(in super::super) fn decode(input: &mut Decoder<'_>) -> Result<Self, Error> {
        let result = Self {
            page: input.u64()?,
            len: input.u64()?,
            height: input.u8()?,
            minimum: K::decode(input)?,
            maximum: K::decode(input)?,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(self) -> Result<(), Error> {
        if self.page == 0 {
            if self.len != 0
                || self.height != 0
                || self.minimum != K::ZERO
                || self.maximum != K::ZERO
            {
                return Err(error("empty metadata tree has nonempty summary"));
            }
        } else if self.len == 0
            || self.len > MAX_ENTRIES as u64
            || self.height == 0
            || usize::from(self.height) >= MAX_HEIGHT
            || self.minimum > self.maximum
            || u64::from(self.height) > self.len
        {
            return Err(error("invalid metadata tree summary"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(in super::super) struct ValueWire {
    pub bytes: [u8; 64],
    pub len: u8,
    dependencies: [u64; 4],
    dependency_count: u8,
}
impl ValueWire {
    pub(in super::super) fn new(bytes: &[u8], ids: &[u64]) -> Result<Self, Error> {
        let dependencies = dependency_ids(ids.iter().copied().filter(|id| *id != 0));
        if bytes.len() > 64 || dependencies.len() > 4 {
            return Err(error("metadata value exceeds fixed page layout"));
        }
        let mut result = Self {
            bytes: [0; 64],
            len: bytes.len().try_into()?,
            dependencies: [0; 4],
            dependency_count: dependencies.len().try_into()?,
        };
        result.bytes[..bytes.len()].copy_from_slice(bytes);
        result.dependencies[..dependencies.len()].copy_from_slice(&dependencies);
        Ok(result)
    }
    pub(in super::super) fn data(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
    pub(in super::super) fn check_dependencies(&self, ids: &[u64]) -> Result<(), Error> {
        dependencies_match(
            &dependency_ids(ids.iter().copied().filter(|id| *id != 0)),
            self.ids(),
        )
    }
    fn ids(&self) -> &[u64] {
        &self.dependencies[..usize::from(self.dependency_count)]
    }
}

pub(in super::super) struct TreeMemos<K, V, S> {
    domain: Domain,
    store: S,
    kind: u8,
    cache: Cache<usize, Node<K, V>, TreeRef<K>>,
    pub visited_nodes: usize,
    pub reused_pages: usize,
}
impl<K: KeyCodec, V, S: Clone> TreeMemos<K, V, S> {
    pub(in super::super) fn register<I: PageIo<Store = S>>(
        &self,
        registry: &mut TreeRegistry<K, V, S>,
        io: &I,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        if !self.domain.same(&registry.domain)
            || self.kind != registry.kind
            || !io.same(&self.store)
            || !io.same(&registry.store)
        {
            return Err(error(
                "metadata registration belongs to another source or domain",
            ));
        }
        self.cache.visit(|_, value, reference| {
            check_replay_cancelled(cancelled)?;
            if io
                .pin(reference.page)
                .is_ok_and(|handle| io.committed(&handle))
            {
                registry.cache.put(reference.page, value, reference);
            }
            Ok(())
        })
    }
    pub(in super::super) fn new(
        domain: &Domain,
        store: S,
        kind: u8,
        slots: usize,
    ) -> Result<Self, Error> {
        Ok(Self {
            domain: domain.clone(),
            store,
            kind,
            cache: Cache::dynamic(slots, domain)?,
            visited_nodes: 0,
            reused_pages: 0,
        })
    }
    pub(in super::super) fn absorb<I: PageIo<Store = S>>(
        &mut self,
        other: Self,
        io: &I,
    ) -> Result<(), Error> {
        if !self.domain.same(&other.domain)
            || self.kind != other.kind
            || !io.same(&self.store)
            || !io.same(&other.store)
        {
            return Err(error(
                "metadata writer memo belongs to another source or domain",
            ));
        }
        self.cache.absorb(other.cache)?;
        self.visited_nodes = other.visited_nodes;
        self.reused_pages = other.reused_pages;
        Ok(())
    }
    pub(in super::super) fn clear(&mut self) {
        self.cache.clear();
    }
    pub(in super::super) fn capacity_bytes(&self) -> usize {
        self.cache.capacity_bytes()
    }
}

pub(in super::super) struct TreeRegistry<K, V, S> {
    domain: Domain,
    store: S,
    kind: u8,
    cache: Cache<u64, Node<K, V>, TreeRef<K>>,
    pub loaded_pages: usize,
    pub cached_pages: usize,
}
impl<K: KeyCodec, V, S: Clone> TreeRegistry<K, V, S> {
    pub(in super::super) fn new(
        domain: &Domain,
        store: S,
        kind: u8,
        slots: usize,
    ) -> Result<Self, Error> {
        Ok(Self {
            domain: domain.clone(),
            store,
            kind,
            cache: Cache::live(slots, domain)?,
            loaded_pages: 0,
            cached_pages: 0,
        })
    }
    pub(in super::super) fn clear(&mut self) {
        self.cache.clear();
    }
    pub(in super::super) fn reclaim(
        &mut self,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        self.cache.reclaim_dead(cancelled)
    }
    pub(in super::super) fn capacity_bytes(&self) -> usize {
        self.cache.capacity_bytes()
    }
}

#[derive(Clone, Copy)]
enum LinkRef<K> {
    Empty,
    Local(u8),
    External(TreeRef<K>),
}
#[derive(Clone, Copy)]
struct Row<K> {
    key: K,
    value: ValueWire,
    left: LinkRef<K>,
    right: LinkRef<K>,
    len: u64,
    height: u8,
}

pub(in super::super) fn write<K: KeyCodec, V: Clone + Item, C: PageWrite>(
    source: &Tree<K, V>,
    old: &TreeMemos<K, V, C::Store>,
    staged: &mut TreeMemos<K, V, C::Store>,
    cx: &mut C,
    encode: &mut impl FnMut(&V, &mut C) -> Result<ValueWire, Error>,
) -> Result<TreeRef<K>, Error> {
    cx.check()?;
    if !source.domain.same(cx.domain())
        || !old.domain.same(cx.domain())
        || !staged.domain.same(cx.domain())
        || old.kind != staged.kind
        || !cx.same(&old.store)
        || !cx.same(&staged.store)
    {
        return Err(error(
            "metadata tree writer belongs to another source or domain",
        ));
    }
    let reference = match &source.root {
        None => TreeRef::empty(),
        Some(root) => write_node(root, old, staged, cx, encode)?,
    };
    if reference.len != source.len as u64 {
        return Err(error("metadata tree count disagrees"));
    }
    Ok(reference)
}

fn write_node<K: KeyCodec, V: Clone + Item, C: PageWrite>(
    source: &Owned<Node<K, V>>,
    old: &TreeMemos<K, V, C::Store>,
    staged: &mut TreeMemos<K, V, C::Store>,
    cx: &mut C,
    encode: &mut impl FnMut(&V, &mut C) -> Result<ValueWire, Error>,
) -> Result<TreeRef<K>, Error> {
    cx.check()?;
    for (memo, pending) in [(&staged.cache, true), (&old.cache, false)] {
        if let Some((alive, reference)) = memo.get(source.identity())
            && alive.same(source)
            && reference.len == source.len as u64
            && reference.height == source.height
            && cx.usable(reference.page, pending)
        {
            staged.reused_pages += 1;
            return Ok(reference);
        }
    }
    let _scratch = cx.domain().reserve(SCRATCH_BYTES)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(ROWS)?;
    let band = source
        .height
        .checked_sub(1)
        .ok_or_else(|| error("metadata tree has zero height"))?
        / BAND;
    let (_, mut reference) = local(source, band, &mut rows, old, staged, cx, encode)?;
    let mut out = Encoder { bytes: Vec::new() };
    out.bytes.try_reserve_exact(SCRATCH_BYTES / 2)?;
    out.bytes.extend_from_slice(MAGIC);
    out.u8(staged.kind);
    out.u64(reference.len);
    out.u8(reference.height);
    reference.minimum.encode(&mut out);
    reference.maximum.encode(&mut out);
    out.u8(rows.len().try_into()?);
    let mut dependencies = Vec::new();
    dependencies.try_reserve_exact(ROWS * 6)?;
    for row in &rows {
        row.key.encode(&mut out);
        out.u64(row.len);
        out.u8(row.height);
        out.u8(row.value.len);
        out.bytes.extend_from_slice(row.value.data());
        out.u8(row.value.dependency_count);
        for id in row.value.ids() {
            out.u64(*id);
            dependencies.push(*id);
        }
        for link in [row.left, row.right] {
            match link {
                LinkRef::Empty => out.u8(0),
                LinkRef::Local(index) => {
                    out.u8(1);
                    out.u8(index);
                }
                LinkRef::External(child) => {
                    out.u8(2);
                    child.encode(&mut out);
                    dependencies.push(child.page);
                }
            }
        }
    }
    let measured = out.bytes.capacity()
        + rows.capacity() * size_of::<Row<K>>()
        + dependencies.capacity() * size_of::<u64>();
    if measured > SCRATCH_BYTES || out.bytes.len() > MAX_PAGE_BYTES {
        return Err(error("metadata page exceeds scratch bound"));
    }
    reference.page = cx.page(&out.bytes, &dependencies)?;
    staged.cache.put(source.identity(), source, reference);
    Ok(reference)
}

fn local<K: KeyCodec, V: Clone + Item, C: PageWrite>(
    source: &Owned<Node<K, V>>,
    band: u8,
    rows: &mut Vec<Row<K>>,
    old: &TreeMemos<K, V, C::Store>,
    staged: &mut TreeMemos<K, V, C::Store>,
    cx: &mut C,
    encode: &mut impl FnMut(&V, &mut C) -> Result<ValueWire, Error>,
) -> Result<(LinkRef<K>, TreeRef<K>), Error> {
    cx.check()?;
    if !source.domain().same(cx.domain()) || !source.value.same_domain(cx.domain()) {
        return Err(error("metadata source has foreign ownership"));
    }
    let mut child = |link: &Link<K, V>, rows: &mut Vec<Row<K>>, cx: &mut C| {
        let Some(root) = link else {
            return Ok((LinkRef::Empty, TreeRef::empty()));
        };
        let child_band = root
            .height
            .checked_sub(1)
            .ok_or_else(|| error("metadata child has zero height"))?
            / BAND;
        if child_band < band {
            let reference = write_node(root, old, staged, cx, encode)?;
            Ok((LinkRef::External(reference), reference))
        } else if child_band == band {
            local(root, band, rows, old, staged, cx, encode)
        } else {
            Err(error("metadata child exceeds parent height band"))
        }
    };
    let (left, lm) = child(&source.left, rows, cx)?;
    let (right, rm) = child(&source.right, rows, cx)?;
    if rows.len() >= ROWS
        || lm.height.abs_diff(rm.height) > 1
        || source.height != 1 + lm.height.max(rm.height)
        || (lm.len != 0 && lm.maximum >= source.key)
        || (rm.len != 0 && rm.minimum <= source.key)
    {
        return Err(error("metadata source violates key or AVL invariants"));
    }
    let len = lm
        .len
        .checked_add(rm.len)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(|| error("metadata tree count overflow"))?;
    if len != source.len as u64 {
        return Err(error("metadata source subtree count disagrees"));
    }
    let reference = TreeRef {
        page: 1,
        len,
        height: source.height,
        minimum: if lm.len == 0 { source.key } else { lm.minimum },
        maximum: if rm.len == 0 { source.key } else { rm.maximum },
    };
    let value = encode(&source.value, cx)?;
    let index = rows.len().try_into()?;
    rows.push(Row {
        key: source.key,
        value,
        left,
        right,
        len,
        height: source.height,
    });
    staged.visited_nodes += 1;
    Ok((LinkRef::Local(index), reference))
}

pub(in super::super) fn read<K: KeyCodec, V: Clone + Item, C: PageRead>(
    reference: TreeRef<K>,
    registry: &mut TreeRegistry<K, V, C::Store>,
    cx: &mut C,
    decode: &mut impl FnMut(&ValueWire, &mut C) -> Result<V, Error>,
) -> Result<Tree<K, V>, Error> {
    cx.check()?;
    reference.validate()?;
    if !registry.domain.same(cx.domain()) || !cx.same(&registry.store) {
        return Err(error("metadata reader belongs to another source or domain"));
    }
    let mut visited = crate::analysis::resident::Traversal::new(reference.len)?;
    let root = read_node(reference, None, None, registry, cx, decode, &mut visited)?;
    Ok(Tree {
        root,
        len: reference.len.try_into()?,
        domain: cx.domain().clone(),
    })
}

fn read_node<K: KeyCodec, V: Clone + Item, C: PageRead>(
    reference: TreeRef<K>,
    lower: Option<K>,
    upper: Option<K>,
    registry: &mut TreeRegistry<K, V, C::Store>,
    cx: &mut C,
    decode: &mut impl FnMut(&ValueWire, &mut C) -> Result<V, Error>,
    visited: &mut crate::analysis::resident::Traversal,
) -> Result<Link<K, V>, Error> {
    cx.check()?;
    if reference.len == 0 {
        return Ok(None);
    }
    if lower.is_some_and(|key| reference.minimum <= key)
        || upper.is_some_and(|key| reference.maximum >= key)
    {
        return Err(error("metadata subtrees overlap or repeat"));
    }
    visited.insert(reference.page, &cx.domain().0)?;
    if let Some((node, summary)) = registry.cache.get(reference.page) {
        if summary != reference {
            return Err(error("metadata cached summary disagrees"));
        }
        registry.cached_pages += 1;
        return Ok(Some(node));
    }
    let page = cx.load(reference.page, MAX_PAGE_BYTES)?;
    let _scratch = cx.domain().reserve(SCRATCH_BYTES)?;
    let mut input = Decoder::new(&page.loaded.bytes, MAGIC)?;
    let kind = input.u8()?;
    let expected = TreeRef {
        page: reference.page,
        len: input.u64()?,
        height: input.u8()?,
        minimum: K::decode(&mut input)?,
        maximum: K::decode(&mut input)?,
    };
    if kind != registry.kind || expected != reference {
        return Err(error("metadata page summary or kind disagrees"));
    }
    let count = usize::from(input.u8()?);
    if count == 0 || count > ROWS {
        return Err(error("metadata page row limit exceeded"));
    }
    let mut rows = Vec::new();
    rows.try_reserve_exact(count)?;
    let mut dependencies = Vec::new();
    dependencies.try_reserve_exact(count * 6)?;
    for index in 0..count {
        let key = K::decode(&mut input)?;
        let len = input.u64()?;
        let height = input.u8()?;
        let size = usize::from(input.u8()?);
        if size > 64 {
            return Err(error("metadata value exceeds fixed layout"));
        }
        let mut value = ValueWire {
            bytes: [0; 64],
            len: size.try_into()?,
            dependencies: [0; 4],
            dependency_count: 0,
        };
        value.bytes[..size].copy_from_slice(input.take(size)?);
        let depcount = usize::from(input.u8()?);
        if depcount > 4 {
            return Err(error("metadata value dependency limit exceeded"));
        }
        value.dependency_count = depcount.try_into()?;
        for id in &mut value.dependencies[..depcount] {
            *id = input.u64()?;
            if *id == 0 {
                return Err(error("metadata value has empty dependency"));
            }
            dependencies.push(*id);
        }
        if value.ids().windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(error("metadata value dependencies are not canonical"));
        }
        let mut child = || {
            Ok::<_, Error>(match input.u8()? {
                0 => LinkRef::Empty,
                1 => {
                    let row = input.u8()?;
                    if usize::from(row) >= index {
                        return Err(error("metadata local link is not postorder"));
                    }
                    LinkRef::Local(row)
                }
                2 => {
                    let child = TreeRef::<K>::decode(&mut input)?;
                    if child.len == 0 || child.page >= reference.page {
                        return Err(error("metadata child violates page order"));
                    }
                    dependencies.push(child.page);
                    LinkRef::External(child)
                }
                _ => return Err(error("unknown metadata child kind")),
            })
        };
        let left = child()?;
        let right = child()?;
        rows.push(Row {
            key,
            value,
            left,
            right,
            len,
            height,
        });
    }
    input.finish()?;
    dependencies_match(&dependency_ids(dependencies), &page.loaded.dependencies)?;
    let mut nodes: Vec<DecodedNode<K, V>> = Vec::new();
    nodes.try_reserve_exact(count)?;
    if rows.capacity() * size_of::<Row<K>>()
        + nodes.capacity() * size_of::<DecodedNode<K, V>>()
        + count * 6 * size_of::<u64>()
        > SCRATCH_BYTES
    {
        return Err(error("metadata read scratch bound exceeded"));
    }
    let band = (reference.height - 1) / BAND;
    for row in rows {
        cx.check()?;
        let mut child = |link: LinkRef<K>,
                         lo: Option<K>,
                         hi: Option<K>,
                         nodes: &mut [DecodedNode<K, V>],
                         cx: &mut C| {
            let (node, summary) = match link {
                LinkRef::Empty => return Ok((None, TreeRef::empty())),
                LinkRef::Local(index) => nodes
                    .get_mut(usize::from(index))
                    .and_then(Option::take)
                    .ok_or_else(|| error("metadata child repeated"))?,
                LinkRef::External(meta) => {
                    if (meta.height - 1) / BAND >= band {
                        return Err(error("metadata external child crosses height band"));
                    }
                    let node = read_node(meta, lo, hi, registry, cx, decode, visited)?
                        .ok_or_else(|| error("empty metadata external child"))?;
                    (node, meta)
                }
            };
            if lo.is_some_and(|key| summary.minimum <= key)
                || hi.is_some_and(|key| summary.maximum >= key)
            {
                return Err(error("metadata child violates global key range"));
            }
            Ok::<_, Error>((Some(node), summary))
        };
        let (left, lm) = child(row.left, lower, Some(row.key), &mut nodes, cx)?;
        let (right, rm) = child(row.right, Some(row.key), upper, &mut nodes, cx)?;
        if lower.is_some_and(|key| row.key <= key)
            || upper.is_some_and(|key| row.key >= key)
            || row.height == 0
            || (row.height - 1) / BAND != band
            || row.height != 1 + lm.height.max(rm.height)
            || lm.height.abs_diff(rm.height) > 1
            || row.len
                != lm
                    .len
                    .checked_add(rm.len)
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| error("metadata subtree count overflow"))?
        {
            return Err(error("metadata row violates key, rank or AVL invariant"));
        }
        let value = decode(&row.value, cx)?;
        if !value.same_domain(cx.domain()) {
            return Err(error("metadata decoder returned a foreign value"));
        }
        let summary = TreeRef {
            page: 1,
            len: row.len,
            height: row.height,
            minimum: if lm.len == 0 { row.key } else { lm.minimum },
            maximum: if rm.len == 0 { row.key } else { rm.maximum },
        };
        let node = Owned::new(
            Node {
                key: row.key,
                value,
                left,
                right,
                height: row.height,
                len: row.len.try_into()?,
            },
            cx.domain(),
            0,
        )?;
        nodes.push(Some((node, summary)));
    }
    let (node, mut summary) = nodes
        .last_mut()
        .and_then(Option::take)
        .ok_or_else(|| error("metadata page lacks root"))?;
    if nodes.iter().any(Option::is_some) {
        return Err(error("metadata page has unreachable rows"));
    }
    summary.page = reference.page;
    if summary != reference {
        return Err(error("metadata root summary disagrees"));
    }
    registry.cache.put(reference.page, &node, summary);
    registry.loaded_pages += 1;
    Ok(Some(node))
}
