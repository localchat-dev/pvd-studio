use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct TestPage {
    bytes: Vec<u8>,
    dependencies: Vec<u64>,
    committed: bool,
}

#[derive(Default)]
struct TestStore {
    pages: BTreeMap<u64, TestPage>,
    values: BTreeMap<u64, Vec<Vec<u8>>>,
    next: u64,
}

type Store = Rc<RefCell<TestStore>>;

struct RegistrationIo(Store);
impl PageIo for RegistrationIo {
    type Store = Store;
    type Handle = u64;
    type Reservation = ();
    fn store(&self) -> Store {
        self.0.clone()
    }
    fn same(&self, store: &Store) -> bool {
        Rc::ptr_eq(&self.0, store)
    }
    fn pin(&self, id: u64) -> Result<u64, Error> {
        self.0
            .borrow()
            .pages
            .contains_key(&id)
            .then_some(id)
            .ok_or_else(|| error("registration page was released"))
    }
    fn id(&self, handle: &u64) -> u64 {
        *handle
    }
    fn committed(&self, handle: &u64) -> bool {
        self.0
            .borrow()
            .pages
            .get(handle)
            .is_some_and(|page| page.committed)
    }
    fn begin(&mut self, _: &Arc<()>) -> Result<(), Error> {
        unreachable!("registration must not start a transaction")
    }
    fn plan(&mut self, _: &[u8], _: &[u64]) -> Result<u64, Error> {
        unreachable!("registration must not write")
    }
    fn commit(&mut self) -> Result<(), Error> {
        unreachable!("registration must not commit")
    }
    fn rollback(&mut self) {
        unreachable!("registration must not roll back")
    }
    fn invalidate_reader(&mut self) {
        unreachable!("registration must not invalidate")
    }
    fn info(&self, _: u64) -> Result<(usize, usize), Error> {
        unreachable!("registration must not inspect payload")
    }
    fn read(&self, _: u64, _: usize, _: &mut impl FnMut() -> bool) -> Result<LoadedPage, Error> {
        unreachable!("registration must not read payload")
    }
    fn reserve_metadata(&self, _: usize) -> Result<(), Error> {
        unreachable!("registration caches are pre-reserved")
    }
}

struct Context {
    domain: Domain,
    store: Store,
    checks: usize,
    cancel_at: Option<usize>,
    fail_page_after: Option<usize>,
    planned: usize,
    loads: usize,
    value_calls: usize,
    decoded_values: BTreeMap<RowRef, ResidentValue>,
}

impl Context {
    fn new(domain: &Domain) -> Self {
        Self::with_store(domain, Rc::new(RefCell::new(TestStore::default())))
    }

    fn with_store(domain: &Domain, store: Store) -> Self {
        Self {
            domain: domain.clone(),
            store,
            checks: 0,
            cancel_at: None,
            fail_page_after: None,
            planned: 0,
            loads: 0,
            value_calls: 0,
            decoded_values: BTreeMap::new(),
        }
    }

    fn check(&mut self) -> Result<(), Error> {
        self.checks += 1;
        check_replay_cancelled(&mut || self.cancel_at.is_some_and(|at| self.checks >= at))
    }

    fn commit(&mut self) {
        for page in self.store.borrow_mut().pages.values_mut() {
            page.committed = true;
        }
    }

    fn rollback(&mut self) {
        let mut store = self.store.borrow_mut();
        let ids: Vec<_> = store
            .pages
            .iter()
            .filter_map(|(id, page)| (!page.committed).then_some(*id))
            .collect();
        for id in ids {
            store.pages.remove(&id);
            store.values.remove(&id);
        }
    }

    fn change_page(&mut self, id: u64, change: impl FnOnce(&mut TestPage)) {
        change(self.store.borrow_mut().pages.get_mut(&id).unwrap());
    }

    fn chunk_page(&self, reference: BufferRef) -> u64 {
        let store = self.store.borrow();
        let page = &store.pages[&reference.root.page];
        u64::from_le_bytes(page.bytes[52..60].try_into().unwrap())
    }
}

impl PageWrite for Context {
    type Store = Store;
    fn store(&self) -> Store {
        self.store.clone()
    }
    fn same(&self, store: &Store) -> bool {
        Rc::ptr_eq(&self.store, store)
    }
    fn domain(&self) -> &Domain {
        &self.domain
    }
    fn check(&mut self) -> Result<(), Error> {
        self.check()
    }
    fn usable(&mut self, id: u64, staged: bool) -> bool {
        self.store
            .borrow()
            .pages
            .get(&id)
            .is_some_and(|page| staged || page.committed)
    }
    fn page(&mut self, bytes: &[u8], dependencies: &[u64]) -> Result<u64, Error> {
        self.check()?;
        if self.fail_page_after == Some(self.planned) {
            return Err(error("injected page planning failure"));
        }
        let mut store = self.store.borrow_mut();
        if dependencies.iter().any(|id| !store.pages.contains_key(id)) {
            return Err(error("test page has an unknown dependency"));
        }
        store.next += 1;
        let id = store.next;
        store.pages.insert(
            id,
            TestPage {
                bytes: bytes.to_vec(),
                dependencies: dependencies.to_vec(),
                committed: false,
            },
        );
        self.planned += 1;
        Ok(id)
    }
    fn values(&mut self, values: &[ResidentValue]) -> Result<Vec<RowRef>, Error> {
        self.check()?;
        self.value_calls += 1;
        if values
            .iter()
            .any(|value| !value.0.domain().same(&self.domain))
        {
            return Err(error("foreign test allocation"));
        }
        let mut references = Vec::new();
        references.try_reserve_exact(values.len())?;
        if values.is_empty() {
            return Ok(references);
        }
        let id = PageWrite::page(self, b"TESTVAL1", &[])?;
        let rows = values
            .iter()
            .map(postcard::to_stdvec)
            .collect::<Result<Vec<_>, _>>()?;
        self.store.borrow_mut().values.insert(id, rows);
        for row in 0..values.len() {
            references.push(RowRef {
                page: id,
                row: row.try_into()?,
            });
        }
        Ok(references)
    }
}

impl PageRead for Context {
    type Store = Store;
    fn store(&self) -> Store {
        self.store.clone()
    }
    fn same(&self, store: &Store) -> bool {
        Rc::ptr_eq(&self.store, store)
    }
    fn domain(&self) -> &Domain {
        &self.domain
    }
    fn check(&mut self) -> Result<(), Error> {
        self.check()
    }
    fn load(
        &mut self,
        id: u64,
        maximum: usize,
    ) -> Result<super::super::super::pages::ReadPage, Error> {
        self.check()?;
        self.loads += 1;
        let store = self.store.borrow();
        let page = store
            .pages
            .get(&id)
            .ok_or_else(|| error("missing test page"))?;
        if !page.committed || page.bytes.len() > maximum {
            return Err(error("uncommitted or oversized test page"));
        }
        let memory = self.domain.0.reserve(add(
            page.bytes.len(),
            page.dependencies.len() * size_of::<u64>(),
        )?)?;
        Ok(super::super::super::pages::ReadPage::new(
            LoadedPage {
                bytes: page.bytes.clone(),
                dependencies: page.dependencies.clone(),
                _release_probe: None,
            },
            memory,
        ))
    }
    fn values(&mut self, references: &[RowRef]) -> Result<Owned<Vec<ResidentValue>>, Error> {
        self.check()?;
        self.value_calls += 1;
        let payload = references
            .len()
            .checked_mul(size_of::<ResidentValue>())
            .unwrap();
        let memory = self
            .domain
            .0
            .reserve(add(Owned::<Vec<ResidentValue>>::fixed_bytes(), payload)?)?;
        let mut result = Vec::new();
        result.try_reserve_exact(references.len())?;
        assert_eq!(result.capacity(), references.len());
        for reference in references {
            self.check()?;
            if let Some(value) = self.decoded_values.get(reference) {
                result.push(value.clone());
                continue;
            }
            let bytes = {
                let store = self.store.borrow();
                if !store
                    .pages
                    .get(&reference.page)
                    .is_some_and(|page| page.committed)
                {
                    return Err(error("uncommitted value page"));
                }
                store
                    .values
                    .get(&reference.page)
                    .and_then(|rows| rows.get(reference.row as usize))
                    .ok_or_else(|| error("invalid test value row"))?
                    .clone()
            };
            let source: Allocation = postcard::from_bytes(&bytes)?;
            let value = ResidentValue::from_source(&source, &self.domain, &mut || false)?;
            self.decoded_values.insert(*reference, value.clone());
            result.push(value);
        }
        Owned::from_reserved(result, memory, payload)
    }
}

fn build<T: BufferCodec>(domain: &Domain, values: impl IntoIterator<Item = T>) -> Buffer<T> {
    let mut draft = BufferDraft::new(domain, 0).unwrap();
    for value in values {
        draft.push(value).unwrap();
    }
    draft.finish().unwrap()
}

fn write_new<T: BufferCodec>(
    buffer: &Buffer<T>,
    cx: &mut Context,
) -> (BufferRef, BufferMemos<T, Store>) {
    let old = BufferMemos::new(&cx.domain, cx.store.clone(), 32, 32).unwrap();
    let mut staged = BufferMemos::new(&cx.domain, cx.store.clone(), 32, 32).unwrap();
    let reference = write(buffer, &old, &mut staged, cx).unwrap();
    cx.commit();
    (reference, staged)
}

#[test]
fn live_registry_reclaim_preserves_owners_and_cached_metadata_mismatches_are_rejected() {
    let domain = Domain::new(8 << 20);
    let source = build(&domain, 0u64..257);
    let conflicting = build(&domain, 500u64..758);
    let mut context = Context::new(&domain);
    let (reference, memos) = write_new(&source, &mut context);
    let mut registry = BufferRegistry::new(&domain, context.store.clone(), 32, 32).unwrap();
    memos
        .register(
            &mut registry,
            &RegistrationIo(context.store.clone()),
            &mut || false,
        )
        .unwrap();
    registry.reclaim(&mut || false).unwrap();
    let mut wrong = reference;
    wrong.root.len += 1;
    wrong.capacity = wrong.capacity.max(wrong.root.len);
    registry.nodes.put(
        reference.root.page,
        conflicting.root.as_ref().unwrap(),
        wrong.root,
    );
    let error = read(wrong, &mut registry, &mut context).err().unwrap();
    assert!(error.to_string().contains("cached node metadata"));
    assert_eq!(context.loads, 0);
    let restored = read(reference, &mut registry, &mut context).unwrap();
    assert!(restored.same(&source));
    assert_eq!(context.loads, 0);
    drop((restored, source, conflicting, context, memos, registry));
    assert_eq!(domain.statistics().logical_used, 0);
}

fn roundtrip<T: BufferCodec + Serialize>(values: Vec<T>) {
    let domain = Domain::new(16 << 20);
    let buffer = build(&domain, values);
    let wire = postcard::to_stdvec(&buffer).unwrap();
    let mut writer = Context::new(&domain);
    let (reference, memo) = write_new(&buffer, &mut writer);
    let target = Domain::new(16 << 20);
    let mut reader = Context::with_store(&target, writer.store.clone());
    let mut registry = BufferRegistry::new(&target, writer.store.clone(), 32, 32).unwrap();
    let decoded: Buffer<T> = read(reference, &mut registry, &mut reader).unwrap();
    assert_eq!(postcard::to_stdvec(&decoded).unwrap(), wire);
    assert_eq!(decoded.capacity(), buffer.capacity());
    assert_eq!(
        registry.statistics.loaded_chunks,
        memo.statistics.encoded_chunks
    );
    drop((decoded, reader, registry));
    assert_eq!(target.statistics().logical_used, 0);
    drop((buffer, memo, writer));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn fixed_rows_preserve_all_fields_empty_and_chunk_boundaries() {
    for count in [0, 1, 255, 256, 257, 4097] {
        roundtrip((0..count).map(|value| value as u64).collect());
    }
    roundtrip(vec![ProfileSpan {
        zone: u64::MAX,
        event: u16::MAX,
        thread: u32::MAX,
        context: 7,
        start: 89,
        end: 55,
        received_frame: 999,
    }]);
    roundtrip(vec![ProfileValue {
        zone: 19,
        event: 2,
        thread: 3,
        context: u64::MAX,
        raw_value: u64::MAX,
        received_frame: 17,
    }]);
    roundtrip(vec![MemorySample {
        received_frame: 32,
        live_bytes: u64::MAX,
        live_allocations: 13,
        allocations: 27,
        frees: 29,
    }]);
}

fn allocation(domain: &Domain, address: u64) -> ResidentValue {
    ResidentValue::new(
        AllocationData {
            address,
            size: 99,
            type_name: Text::new("跨块", domain, &mut || false).unwrap(),
            file: Text::new("source.cpp", domain, &mut || false).unwrap(),
            line: 73,
            received_frame: 19,
        },
        domain,
    )
    .unwrap()
}

#[test]
fn event_rows_preserve_allocate_replacement_matched_and_unmatched_free() {
    let domain = Domain::new(32 << 20);
    let first = allocation(&domain, 1);
    let second = allocation(&domain, 2);
    let kinds = [
        EventKind::Allocate {
            allocation: first.clone(),
            replaced: None,
        },
        EventKind::Allocate {
            allocation: second.clone(),
            replaced: Some(first.clone()),
        },
        EventKind::Free {
            allocation: Some(second),
        },
        EventKind::Free { allocation: None },
    ];
    let buffer = build(
        &domain,
        (0..513).map(|index| {
            EventRef(
                Owned::new(
                    EventData {
                        buffer: u64::MAX - index,
                        address: index,
                        received_frame: index as usize,
                        kind: kinds[index as usize % kinds.len()].clone(),
                    },
                    &domain,
                    0,
                )
                .unwrap(),
            )
        }),
    );
    let wire = postcard::to_stdvec(&buffer).unwrap();
    let mut cx = Context::new(&domain);
    let (reference, memo) = write_new(&buffer, &mut cx);
    assert_eq!(memo.statistics.encoded_elements, 513);
    let value_calls = cx.value_calls;
    let mut staged = BufferMemos::new(&domain, cx.store.clone(), 16, 16).unwrap();
    assert_eq!(
        write(&buffer, &memo, &mut staged, &mut cx).unwrap(),
        reference
    );
    assert_eq!(cx.value_calls, value_calls);
    assert_eq!(staged.statistics.encoded_elements, 0);
    let mut edit = buffer.edit(&domain, &mut || false).unwrap();
    edit.push(buffer.view().last().unwrap().clone()).unwrap();
    let next = edit.finish().unwrap();
    let mut staged = BufferMemos::new(&domain, cx.store.clone(), 16, 16).unwrap();
    write(&next, &memo, &mut staged, &mut cx).unwrap();
    assert!(staged.statistics.reused_nodes > 0);
    assert_eq!(staged.statistics.encoded_elements, 2);
    assert_eq!(cx.value_calls, value_calls + 1);
    cx.commit();
    let target = Domain::new(32 << 20);
    let mut reader = Context::with_store(&target, cx.store.clone());
    let mut registry = BufferRegistry::new(&target, cx.store.clone(), 16, 16).unwrap();
    let restored: Buffer<EventRef> = read(reference, &mut registry, &mut reader).unwrap();
    assert_eq!(postcard::to_stdvec(&restored).unwrap(), wire);
    let reused: Buffer<EventRef> = read(reference, &mut registry, &mut reader).unwrap();
    assert!(
        restored
            .root
            .as_ref()
            .unwrap()
            .same(reused.root.as_ref().unwrap())
    );
    assert_eq!(reader.value_calls, 3);
    let escaped_event = restored.view().get(0).unwrap().clone();
    let escaped_text = match &escaped_event.kind {
        EventKind::Allocate { allocation, .. } => allocation.type_name.clone(),
        _ => unreachable!(),
    };
    drop((restored, reused, registry, reader));
    assert!(target.statistics().logical_used > 0);
    assert_eq!(escaped_event.address, 0);
    drop(escaped_event);
    assert_eq!(&*escaped_text, "跨块");
    assert!(target.statistics().logical_used > 0);
    drop(escaped_text);
    assert_eq!(target.statistics().logical_used, 0);
}

#[test]
fn retained_prefix_and_sparse_chunks_roundtrip_without_flattening() {
    let domain = Domain::new(32 << 20);
    let buffer = build(&domain, 0..4097_u64);
    let mut cx = Context::new(&domain);
    let (_, old) = write_new(&buffer, &mut cx);
    let original_wire = postcard::to_stdvec(&buffer).unwrap();
    let mut edit = buffer.edit(&domain, &mut || false).unwrap();
    edit.remove_prefix(1025).unwrap();
    let retained = edit.finish().unwrap();
    let mut staged = BufferMemos::new(&domain, cx.store.clone(), 32, 32).unwrap();
    let reference = write(&retained, &old, &mut staged, &mut cx).unwrap();
    assert!(staged.statistics.reused_nodes > 0);
    assert_eq!(staged.statistics.encoded_elements, 255);
    cx.commit();
    let mut registry = BufferRegistry::<u64, _>::new(&domain, cx.store.clone(), 32, 32).unwrap();
    let restored = read(reference, &mut registry, &mut cx).unwrap();
    assert_eq!(
        restored.view().iter().copied().collect::<Vec<_>>(),
        (1025..4097_u64).collect::<Vec<_>>()
    );
    let mut edit = retained.edit(&domain, &mut || false).unwrap();
    edit.retain(|value| value.is_multiple_of(257), &mut || false)
        .unwrap();
    let sparse = edit.finish().unwrap();
    let mut sparse_memo = BufferMemos::new(&domain, cx.store.clone(), 32, 32).unwrap();
    let sparse_ref = write(&sparse, &staged, &mut sparse_memo, &mut cx).unwrap();
    cx.commit();
    let restored = read(sparse_ref, &mut registry, &mut cx).unwrap();
    assert_eq!(
        postcard::to_stdvec(&restored).unwrap(),
        postcard::to_stdvec(&sparse).unwrap()
    );
    assert_eq!(
        restored.view().iter().rev().copied().collect::<Vec<_>>(),
        (1025..4097_u64)
            .filter(|value| value.is_multiple_of(257))
            .rev()
            .collect::<Vec<_>>()
    );
    assert_eq!(postcard::to_stdvec(&buffer).unwrap(), original_wire);
}

#[test]
fn root_and_chunk_memos_skip_codec_and_preserve_virtual_capacity() {
    let domain = Domain::new(32 << 20);
    let buffer = build(&domain, 0..2049_u64);
    let mut cx = Context::new(&domain);
    let (reference, old) = write_new(&buffer, &mut cx);
    let mut staged = BufferMemos::new(&domain, cx.store.clone(), 32, 32).unwrap();
    let plans = cx.planned;
    assert_eq!(
        write(&buffer, &old, &mut staged, &mut cx).unwrap(),
        reference
    );
    assert_eq!(cx.planned, plans);
    assert_eq!(staged.statistics.encoded_elements, 0);
    assert_eq!(staged.statistics.reused_nodes, 1);
    let mut edit = buffer.edit(&domain, &mut || false).unwrap();
    edit.push(2049).unwrap();
    let next = edit.finish().unwrap();
    let mut staged = BufferMemos::new(&domain, cx.store.clone(), 32, 32).unwrap();
    let next_ref = write(&next, &old, &mut staged, &mut cx).unwrap();
    assert!(staged.statistics.reused_nodes > 0);
    assert!(staged.statistics.encoded_elements <= CHUNK_LIMIT);
    cx.commit();
    let mut compact = next.clone();
    compact.legacy_capacity = compact.len();
    let mut compact_memo = BufferMemos::new(&domain, cx.store.clone(), 32, 32).unwrap();
    let compact_ref = write(&compact, &staged, &mut compact_memo, &mut cx).unwrap();
    assert_eq!(compact_ref.root, next_ref.root);
    assert_eq!(compact_ref.capacity, compact.len() as u64);
    assert_eq!(compact_memo.statistics.encoded_elements, 0);
    assert_eq!(
        postcard::to_stdvec(&buffer).unwrap(),
        postcard::to_stdvec(&(0..2049_u64).collect::<Vec<_>>()).unwrap()
    );
}

#[test]
fn source_domain_type_and_descriptor_mismatches_are_rejected_before_io() {
    let domain = Domain::new(16 << 20);
    let buffer = build(&domain, 0..10_u64);
    let mut cx = Context::new(&domain);
    let (reference, old) = write_new(&buffer, &mut cx);
    let other = Context::new(&domain);
    let mut wrong = BufferMemos::new(&domain, other.store.clone(), 1, 1).unwrap();
    assert!(write(&buffer, &old, &mut wrong, &mut cx).is_err());
    let foreign = Domain::new(16 << 20);
    let mut wrong = BufferMemos::new(&foreign, cx.store.clone(), 1, 1).unwrap();
    assert!(write(&buffer, &old, &mut wrong, &mut cx).is_err());
    let mut registry = BufferRegistry::<u64, _>::new(&domain, other.store, 1, 1).unwrap();
    assert!(read(reference, &mut registry, &mut cx).is_err());
    let mut registry =
        BufferRegistry::<ProfileSpan, _>::new(&domain, cx.store.clone(), 1, 1).unwrap();
    assert!(read(reference, &mut registry, &mut cx).is_err());
    let mut registry = BufferRegistry::<u64, _>::new(&domain, cx.store.clone(), 1, 1).unwrap();
    for invalid in [
        BufferRef {
            capacity: 0,
            ..reference
        },
        BufferRef {
            capacity: crate::analysis::MAX_RECORDS as u64 + 1,
            ..reference
        },
        BufferRef {
            root: NodeRef {
                page: 0,
                ..reference.root
            },
            ..reference
        },
        BufferRef {
            root: NodeRef {
                height: MAX_HEIGHT as u8,
                ..reference.root
            },
            ..reference
        },
    ] {
        assert!(read(invalid, &mut registry, &mut cx).is_err());
    }
    assert_eq!(cx.loads, 0);
}

#[test]
fn malformed_tree_chunk_dependencies_cycles_and_lengths_are_rejected() {
    let domain = Domain::new(16 << 20);
    let buffer = build(&domain, 0..257_u64);
    let mut cx = Context::new(&domain);
    let (reference, _) = write_new(&buffer, &mut cx);
    let original = cx.store.borrow().pages.clone();
    let chunk = cx.chunk_page(reference);
    for fault in 0..9 {
        cx.store.borrow_mut().pages = original.clone();
        match fault {
            0 => cx.change_page(reference.root.page, |page| page.bytes[8] = 3),
            1 => cx.change_page(reference.root.page, |page| {
                page.bytes[9..17].copy_from_slice(&999_u64.to_le_bytes())
            }),
            2 => cx.change_page(reference.root.page, |page| {
                page.bytes[18..26].copy_from_slice(&reference.root.page.to_le_bytes())
            }),
            3 => cx.change_page(reference.root.page, |page| page.dependencies.clear()),
            4 => cx.change_page(chunk, |page| {
                page.bytes[9..11].copy_from_slice(&0_u16.to_le_bytes())
            }),
            5 => cx.change_page(chunk, |page| page.bytes.push(0)),
            6 => cx.change_page(chunk, |page| {
                page.bytes.pop();
            }),
            7 => cx.change_page(chunk, |page| page.dependencies.push(reference.root.page)),
            _ => cx.change_page(chunk, |page| page.committed = false),
        }
        let mut registry = BufferRegistry::<u64, _>::new(&domain, cx.store.clone(), 0, 0).unwrap();
        assert!(
            read(reference, &mut registry, &mut cx).is_err(),
            "fault {fault}"
        );
    }
    cx.store.borrow_mut().pages = original;
    let mut registry = BufferRegistry::<u64, _>::new(&domain, cx.store.clone(), 0, 0).unwrap();
    assert_eq!(read(reference, &mut registry, &mut cx).unwrap().len(), 257);
}

#[test]
fn failed_planning_and_cancelled_reads_keep_old_roots_and_release_payload() {
    let domain = Domain::new(32 << 20);
    let buffer = build(&domain, 0..1025_u64);
    let original_wire = postcard::to_stdvec(&buffer).unwrap();
    let mut cx = Context::new(&domain);
    let (reference, old) = write_new(&buffer, &mut cx);
    let committed_count = cx.store.borrow().pages.len();
    let mut edit = buffer.edit(&domain, &mut || false).unwrap();
    edit.push(10000).unwrap();
    let next = edit.finish().unwrap();
    cx.fail_page_after = Some(cx.planned + 1);
    let mut staged = BufferMemos::new(&domain, cx.store.clone(), 8, 8).unwrap();
    assert!(write(&next, &old, &mut staged, &mut cx).is_err());
    cx.rollback();
    assert_eq!(cx.store.borrow().pages.len(), committed_count);
    assert_eq!(postcard::to_stdvec(&buffer).unwrap(), original_wire);
    drop(staged);
    let target = Domain::new(16 << 20);
    for stop in [1, 3, 10, 60, 300] {
        let mut reader = Context::with_store(&target, cx.store.clone());
        reader.cancel_at = Some(stop);
        let mut registry = BufferRegistry::<u64, _>::new(&target, cx.store.clone(), 8, 8).unwrap();
        assert!(read(reference, &mut registry, &mut reader).is_err());
        drop((reader, registry));
        assert_eq!(target.statistics().logical_used, 0);
    }
    let target = Domain::new(1024);
    let mut reader = Context::with_store(&target, cx.store.clone());
    let mut registry = BufferRegistry::<u64, _>::new(&target, cx.store.clone(), 0, 0).unwrap();
    assert!(read(reference, &mut registry, &mut reader).is_err());
    drop((reader, registry));
    assert_eq!(target.statistics().logical_used, 0);
}

#[test]
fn registry_weak_entries_release_chunk_payload_and_descriptor_wire_is_exact() {
    let domain = Domain::new(16 << 20);
    let buffer = build(&domain, 0..4096_u64);
    let mut cx = Context::new(&domain);
    let (reference, _) = write_new(&buffer, &mut cx);
    let mut bytes = Encoder { bytes: Vec::new() };
    reference.encode(&mut bytes);
    assert_eq!(bytes.bytes.len(), 26);
    let mut input = Decoder {
        bytes: &bytes.bytes,
        offset: 0,
    };
    assert_eq!(BufferRef::decode(&mut input).unwrap(), reference);
    input.finish().unwrap();
    let target = Domain::new(16 << 20);
    let mut reader = Context::with_store(&target, cx.store.clone());
    let mut registry = BufferRegistry::<u64, _>::new(&target, cx.store.clone(), 32, 32).unwrap();
    let decoded = read(reference, &mut registry, &mut reader).unwrap();
    let used_with_root = target.statistics().logical_used;
    let loads = reader.loads;
    let held = read(reference, &mut registry, &mut reader).unwrap();
    assert_eq!(reader.loads, loads);
    drop((decoded, held));
    assert!(used_with_root - target.statistics().logical_used >= 4096 * size_of::<u64>());
    assert_eq!(target.statistics().live_strong_cells, 0);
    registry.clear();
    assert_eq!(target.statistics().logical_used, registry.capacity_bytes());
    drop((reader, registry));
    assert_eq!(target.statistics().logical_used, 0);
}

#[test]
fn memory_event_invalid_kinds_missing_and_out_of_range_rows_are_rejected() {
    let domain = Domain::new(16 << 20);
    let value = allocation(&domain, 7);
    let buffer = build(
        &domain,
        [EventRef(
            Owned::new(
                EventData {
                    buffer: 3,
                    address: 7,
                    received_frame: 9,
                    kind: EventKind::Allocate {
                        allocation: value,
                        replaced: None,
                    },
                },
                &domain,
                0,
            )
            .unwrap(),
        )],
    );
    let mut cx = Context::new(&domain);
    let (reference, _) = write_new(&buffer, &mut cx);
    let chunk = cx.chunk_page(reference);
    let original = cx.store.borrow().pages.clone();
    for fault in 0..5 {
        cx.store.borrow_mut().pages = original.clone();
        cx.change_page(chunk, |page| match fault {
            0 => page.bytes[35] = 2,
            1 => page.bytes[36..48].fill(0),
            2 => {
                page.bytes[36..44].fill(0);
                page.bytes[44..48].copy_from_slice(&1_u32.to_le_bytes());
            }
            3 => page.bytes[44..48].copy_from_slice(&u32::MAX.to_le_bytes()),
            _ => {
                page.bytes[35] = 1;
                page.bytes.copy_within(36..48, 48);
            }
        });
        let target = Domain::new(16 << 20);
        let mut reader = Context::with_store(&target, cx.store.clone());
        let mut registry =
            BufferRegistry::<EventRef, _>::new(&target, cx.store.clone(), 0, 0).unwrap();
        assert!(
            read(reference, &mut registry, &mut reader).is_err(),
            "fault {fault}"
        );
        drop((reader, registry));
        assert_eq!(target.statistics().logical_used, 0);
    }
}

#[test]
fn committed_source_registration_reuses_live_roots_with_weak_only_ownership() {
    let domain = Domain::new(16 << 20);
    let buffer = build(&domain, 0..4097_u64);
    let mut cx = Context::new(&domain);
    let (reference, memo) = write_new(&buffer, &mut cx);
    let io = RegistrationIo(cx.store.clone());
    let mut registry = BufferRegistry::<u64, _>::new(&domain, cx.store.clone(), 32, 32).unwrap();
    memo.register(&mut registry, &io, &mut || false).unwrap();
    let decoded = read(reference, &mut registry, &mut cx).unwrap();
    assert_eq!(cx.loads, 0);
    assert!(
        buffer
            .root
            .as_ref()
            .unwrap()
            .same(decoded.root.as_ref().unwrap())
    );
    let with_root = domain.statistics().logical_used;
    drop((buffer, decoded));
    assert!(with_root - domain.statistics().logical_used >= 4097 * size_of::<u64>());
    assert_eq!(domain.statistics().live_strong_cells, 0);
    registry.clear();
    drop((registry, memo, cx));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn registration_rejects_foreign_owners_cancels_and_skips_expired_or_uncommitted_pages() {
    let domain = Domain::new(16 << 20);
    let buffer = build(&domain, 0..1025_u64);
    let mut cx = Context::new(&domain);
    let (reference, memo) = write_new(&buffer, &mut cx);
    let io = RegistrationIo(cx.store.clone());
    let mut registry = BufferRegistry::<u64, _>::new(&domain, cx.store.clone(), 16, 16).unwrap();
    let foreign = Domain::new(16 << 20);
    let mut foreign_registry =
        BufferRegistry::<u64, _>::new(&foreign, cx.store.clone(), 1, 1).unwrap();
    assert!(
        memo.register(&mut foreign_registry, &io, &mut || false)
            .is_err()
    );
    let other_store = Context::new(&domain);
    let mut wrong_store =
        BufferRegistry::<u64, _>::new(&domain, other_store.store.clone(), 1, 1).unwrap();
    assert!(memo.register(&mut wrong_store, &io, &mut || false).is_err());
    assert!(
        memo.register(
            &mut registry,
            &RegistrationIo(other_store.store),
            &mut || false
        )
        .is_err()
    );
    for stop in [0, 3] {
        let mut checks = 0;
        assert!(
            memo.register(&mut registry, &io, &mut || {
                checks += 1;
                checks > stop
            })
            .is_err()
        );
        registry.clear();
    }
    let original = cx.store.borrow().pages.clone();
    for page in cx.store.borrow_mut().pages.values_mut() {
        page.committed = false;
    }
    memo.register(&mut registry, &io, &mut || false).unwrap();
    assert!(registry.nodes.get(reference.root.page).is_none());
    cx.store.borrow_mut().pages.clear();
    memo.register(&mut registry, &io, &mut || false).unwrap();
    assert!(registry.nodes.get(reference.root.page).is_none());
    cx.store.borrow_mut().pages = original;
    let stats = domain.statistics();
    let hold = domain.reserve(stats.limit - stats.logical_used).unwrap();
    memo.register(&mut registry, &io, &mut || false).unwrap();
    assert!(registry.nodes.get(reference.root.page).is_none());
    drop(hold);
    memo.register(&mut registry, &io, &mut || false).unwrap();
    assert!(registry.nodes.get(reference.root.page).is_some());
}
