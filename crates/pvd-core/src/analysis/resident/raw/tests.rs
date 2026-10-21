use super::*;

struct Io(Arc<AtomicUsize>);
impl PageIo for Io {
    type Handle = u64;
    type Store = Arc<AtomicUsize>;
    type Reservation = ();
    fn store(&self) -> Self::Store {
        self.0.clone()
    }
    fn same(&self, store: &Self::Store) -> bool {
        Arc::ptr_eq(&self.0, store)
    }
    fn pin(&self, id: u64) -> Result<u64, Error> {
        Ok(id)
    }
    fn id(&self, id: &u64) -> u64 {
        *id
    }
    fn committed(&self, _: &u64) -> bool {
        true
    }
    fn begin(&mut self, _: &Arc<()>) -> Result<(), Error> {
        unreachable!()
    }
    fn plan(&mut self, _: &[u8], _: &[u64]) -> Result<u64, Error> {
        unreachable!()
    }
    fn commit(&mut self) -> Result<(), Error> {
        unreachable!()
    }
    fn rollback(&mut self) {
        unreachable!()
    }
    fn invalidate_reader(&mut self) {}
    fn info(&self, _: u64) -> Result<(usize, usize), Error> {
        Ok((1, 0))
    }
    fn read(
        &self,
        _: u64,
        _: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<LoadedPage, Error> {
        check_replay_cancelled(cancelled)?;
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(LoadedPage {
            bytes: vec![19],
            dependencies: Vec::new(),
            _release_probe: None,
        })
    }
    fn reserve_metadata(&self, _: usize) -> Result<(), Error> {
        unreachable!()
    }
}

fn fixture() -> (PageReader<Io>, Arc<DataPage>, Arc<AtomicUsize>) {
    let budget = MemoryBudget::new(256 << 10);
    let reads = Arc::new(AtomicUsize::new(0));
    let mut reader = PageReader::with_budget(
        Io(reads.clone()),
        Limits {
            page_cache_bytes: 1 << 20,
        },
        budget.clone(),
    )
    .unwrap();
    let bytes = MAX_PAGE_BYTES + size_of::<DataPage>() + 2 * size_of::<usize>() + 256;
    let memory = budget.reserve(bytes).unwrap();
    let payload = vec![0u8; MAX_PAGE_BYTES];
    assert_eq!(payload.capacity(), MAX_PAGE_BYTES);
    let page = Arc::new(DataPage {
        rows: DataRows::Strings {
            bytes: payload,
            ranges: Vec::new(),
        },
        identities: None,
        _memory: memory,
    });
    reader.pages.insert(1, page.clone());
    reader.page_ages.insert(0, 1);
    reader.page_bytes = bytes;
    (reader, page, reads)
}

#[test]
fn reclaimed_map_registry_keeps_live_nodes_and_rejects_conflicting_page_metadata() {
    let domain = Domain::new(1 << 20);
    let value = ResidentValue::from_source(
        &Allocation {
            address: 5,
            size: 16,
            type_name: "Map".into(),
            file: "map.cpp".into(),
            line: 1,
            received_frame: 0,
        },
        &domain,
        &mut || false,
    )
    .unwrap();
    let node = Owned::new(
        ResidentNode {
            key: (1, 5),
            value,
            left: None,
            right: None,
            subtree_len: 1,
            height: 1,
        },
        &domain,
        0,
    )
    .unwrap();
    let reference = MapRef {
        page: 17,
        len: 1,
        height: 1,
        minimum: (1, 5),
        maximum: (1, 5),
        charge: (TREE_ARC_BYTES
            + crate::analysis::resident::read_maps::logical_value_charge(&node.value).unwrap())
            as u64,
    };
    let reads = Arc::new(AtomicUsize::new(0));
    let mut reader = ResidentReader::dynamic(
        Io(reads.clone()),
        domain.clone(),
        ResidentLimits {
            tree_slots: 2,
            value_slots: 0,
            string_slots: 0,
            page_cache_bytes: 0,
        },
    )
    .unwrap();
    reader.trees.put(reference.page, &node, reference);
    reader.reclaim(&mut || false).unwrap();
    let mut wrong = reference;
    wrong.charge += 1;
    reader.trees.put(reference.page, &node, wrong);
    let mut visited = Traversal::new(1).unwrap();
    let mut remaining = 1 << 20;
    let failure = reader
        .tree(wrong, None, None, &mut visited, &mut remaining, &mut || {
            false
        })
        .err()
        .unwrap();
    assert!(failure.to_string().contains("cached tree summary"));
    drop(visited);
    let mut visited = Traversal::new(1).unwrap();
    let restored = reader
        .tree(
            reference,
            None,
            None,
            &mut visited,
            &mut remaining,
            &mut || false,
        )
        .unwrap()
        .unwrap();
    assert!(restored.same(&node));
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    drop((visited, restored, reader, node));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn bounded_data_cache_is_evicted_before_read_workspace_is_rejected() {
    let (mut reader, page, reads) = fixture();
    let budget = reader.budget.clone();
    drop(page);
    let (loaded, memory) = reader.loaded(2, 1, &mut || false).unwrap();
    assert_eq!(loaded.bytes, [19]);
    assert!(reader.pages.is_empty());
    assert!(reader.page_ages.is_empty());
    assert_eq!(reader.page_bytes, 0);
    assert_eq!(reads.load(Ordering::Relaxed), 1);
    assert!(budget.used.load(Ordering::Acquire) <= budget.limit);
    drop((loaded, memory, reader));
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
}

#[test]
fn externally_pinned_pages_remain_charged_after_cache_eviction() {
    let (mut reader, page, reads) = fixture();
    let budget = reader.budget.clone();
    let failure = reader.loaded(2, 1, &mut || false).err().unwrap();
    assert!(failure.downcast_ref::<ResidentBudgetExceeded>().is_some());
    assert!(reader.pages.is_empty());
    assert_eq!(budget.used.load(Ordering::Acquire), page._memory.bytes);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    drop(page);
    let (loaded, memory) = reader.loaded(2, 1, &mut || false).unwrap();
    drop((loaded, memory, reader));
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
}

#[test]
fn cancelled_read_does_not_evict_a_cached_page_or_start_io() {
    let (mut reader, page, reads) = fixture();
    let budget = reader.budget.clone();
    let before = budget.used.load(Ordering::Acquire);
    let failure = reader.loaded(2, 1, &mut || true).err().unwrap();
    assert_eq!(
        failure.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(reader.pages.len(), 1);
    assert_eq!(budget.used.load(Ordering::Acquire), before);
    assert_eq!(reads.load(Ordering::Relaxed), 0);
    drop((page, reader));
    assert_eq!(budget.used.load(Ordering::Acquire), 0);
}
