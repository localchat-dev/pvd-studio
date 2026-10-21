use super::*;

fn value(address: u64, domain: &Domain) -> ResidentValue {
    ResidentValue::from_source(
        &Allocation {
            address,
            size: 32,
            type_name: "Small".into(),
            file: "small.cpp".into(),
            line: 4,
            received_frame: 0,
        },
        domain,
        &mut || false,
    )
    .unwrap()
}

fn node(
    address: u64,
    left: ResidentLink,
    right: ResidentLink,
    domain: &Domain,
) -> Owned<ResidentNode> {
    let subtree_len = 1
        + left.as_ref().map_or(0, |node| node.subtree_len)
        + right.as_ref().map_or(0, |node| node.subtree_len);
    let height = 1 + left
        .as_ref()
        .map_or(0, |node| node.height)
        .max(right.as_ref().map_or(0, |node| node.height));
    Owned::new(
        ResidentNode {
            key: (1, address),
            value: value(address, domain),
            left,
            right,
            subtree_len,
            height,
        },
        domain,
        0,
    )
    .unwrap()
}

fn seven(domain: &Domain) -> Owned<ResidentNode> {
    let left = node(
        2,
        Some(node(1, None, None, domain)),
        Some(node(3, None, None, domain)),
        domain,
    );
    let right = node(
        6,
        Some(node(5, None, None, domain)),
        Some(node(7, None, None, domain)),
        domain,
    );
    node(4, Some(left), Some(right), domain)
}

#[test]
fn one_weak_group_reuses_every_leaf_and_height_two_or_three_root() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    let before = domain.statistics();
    index.register(&root, &mut || false).unwrap();
    assert_eq!(index.entries(), 7);
    assert_eq!(index.groups, 1);
    assert_eq!(
        domain.statistics().logical_weak_fixed_bytes - before.logical_weak_fixed_bytes,
        Owned::<ResidentNode>::fixed_bytes()
    );
    for path in 1..=7 {
        let expected = at_path(&root, path).unwrap();
        let (restored, reused) = index.intern((*expected).clone(), &mut || false).unwrap();
        assert!(reused);
        assert!(restored.same(&expected));
    }
    assert_eq!(index.hits, 7);
    assert!(index.bytes() <= index.limit_bytes);
    drop((root, index));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn matching_hash_still_checks_full_content_and_equal_values_keep_distinct_ids() {
    let domain = Domain::new(8 << 20);
    let root = node(1, None, None, &domain);
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    let mut data = (*root.value).clone();
    data.size += 1;
    let conflicting = ResidentNode {
        value: ResidentValue(
            Owned::imported(data, &domain, 0, root.value.0.wire_identity().unwrap()).unwrap(),
        ),
        ..(*root).clone()
    };
    assert_eq!(
        fingerprint(&root, &domain).unwrap(),
        fingerprint(&conflicting, &domain).unwrap()
    );
    let (restored, reused) = index.intern(conflicting, &mut || false).unwrap();
    assert!(!reused);
    assert!(!restored.same(&root));
    assert_eq!(restored.value.size, 33);
    let independent = node(1, None, None, &domain);
    let (separate, reused) = index.intern((*independent).clone(), &mut || false).unwrap();
    assert!(!reused);
    assert!(!separate.same(&root));
    assert_ne!(
        separate.value.0.wire_identity().unwrap(),
        root.value.0.wire_identity().unwrap()
    );
    assert!(equivalent(&root, &root, &domain).unwrap());
    drop((index, root, restored, independent, separate));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn group_weak_does_not_keep_payload_alive_and_cannot_retarget_after_reclamation() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let weak = root.weak().unwrap();
    let exported = at_path(&root, 4).unwrap();
    let exported_weak = exported.weak().unwrap();
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    drop(root);
    assert!(weak.upgrade().is_none());
    assert!(exported_weak.upgrade().is_some());
    assert!(index.reclaim(&mut || true).is_err());
    assert_eq!(index.entries(), 7);
    index.reclaim(&mut || false).unwrap();
    assert_eq!(index.bytes(), 0);
    let replacement = seven(&domain);
    index.register(&replacement, &mut || false).unwrap();
    let (restored, reused) = index.intern((*replacement).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(restored.same(&replacement));
    drop(exported);
    assert!(exported_weak.upgrade().is_none());
    drop((restored, replacement, index, weak, exported_weak));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn byte_limit_includes_old_and_new_tables_and_group_ownership() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let mut index = SmallSubtrees::with_limits(&domain, 128, MAX_BYTES).unwrap();
    index.register(&root, &mut || false).unwrap();
    assert_eq!(index.slots.capacity(), 64);
    let before_bytes = index.bytes();
    let new_bytes = 128 * size_of::<Slot>();
    let before = domain.statistics();
    index.limit_bytes = before_bytes + new_bytes - 1;
    assert!(!index.grow(128, &mut || false).unwrap());
    assert_eq!(index.slots.capacity(), 64);
    assert_eq!(domain.statistics().logical_used, before.logical_used);
    index.limit_bytes += 1;
    assert!(index.grow(128, &mut || false).unwrap());
    assert_eq!(index.slots.capacity(), 128);
    assert!(domain.statistics().logical_peak >= before.logical_used + new_bytes);
    assert!(index.bytes() <= index.limit_bytes);
    let (restored, reused) = index.intern((*root).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(restored.same(&root));
    drop((restored, root, index));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn cancelled_layout_build_preserves_the_original_directory_and_tickets() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let mut index = SmallSubtrees::with_limits(&domain, 1024, MAX_BYTES).unwrap();
    index.register(&root, &mut || false).unwrap();
    assert!(index.grow(512, &mut || false).unwrap());
    let before = domain.statistics().logical_used;
    let mut checks = 0;
    let error = index
        .grow(1024, &mut || {
            checks += 1;
            checks == 7
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(index.slots.capacity(), 512);
    assert_eq!(index.entries(), 7);
    assert_eq!(domain.statistics().logical_used, before);
    let (restored, reused) = index.intern((*root).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(restored.same(&root));
    drop((restored, root, index));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn saturated_or_full_domain_index_is_optional_and_does_not_lose_old_owners() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let mut index = SmallSubtrees::with_limits(&domain, 8, 4096).unwrap();
    index.register(&root, &mut || false).unwrap();
    assert_eq!(index.entries(), 6);
    let before = domain.statistics();
    let filled = domain.reserve(before.limit - before.logical_used).unwrap();
    index.register(&root, &mut || false).unwrap();
    let (restored, reused) = index.intern((*root).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(restored.same(&root));
    assert_eq!(index.entries(), 6);
    assert_eq!(domain.statistics().logical_used, before.limit);
    drop((filled, restored));
    let mut empty = SmallSubtrees::with_limits(&domain, 64, 0).unwrap();
    empty.register(&root, &mut || false).unwrap();
    assert_eq!(empty.entries(), 0);
    assert_eq!(empty.bytes(), 0);
    drop((empty, index, root));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn domain_and_shape_guards_are_checked_before_reusing_a_candidate() {
    let domain = Domain::new(8 << 20);
    let foreign = Domain::new(8 << 20);
    let root = seven(&domain);
    let other = seven(&foreign);
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    assert!(index.register(&other, &mut || false).is_err());
    assert!(index.intern((*other).clone(), &mut || false).is_err());
    let mut malformed = (*root).clone();
    malformed.height = 1;
    assert!(index.intern(malformed, &mut || false).is_err());
    let mut another_reader = SmallSubtrees::new(&domain, true).unwrap();
    let (restored, reused) = another_reader
        .intern((*root).clone(), &mut || false)
        .unwrap();
    assert!(!reused);
    assert!(!restored.same(&root));
    drop((restored, another_reader, index, root, other));
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(foreign.statistics().logical_used, 0);
}

fn imported_value(origin: u8, domain: &Domain) -> ResidentValue {
    let wire = |sequence| identity::WireIdentity {
        origin: [origin; 16],
        sequence,
    };
    ResidentValue(
        Owned::imported(
            AllocationData {
                address: 1,
                size: 32,
                type_name: Text::with_wire_identity("Small", domain, &mut || false, Some(wire(2)))
                    .unwrap(),
                file: Text::with_wire_identity("small.cpp", domain, &mut || false, Some(wire(3)))
                    .unwrap(),
                line: 4,
                received_frame: 0,
            },
            domain,
            0,
            wire(1),
        )
        .unwrap(),
    )
}

fn leaf_with(value: ResidentValue, domain: &Domain) -> Owned<ResidentNode> {
    Owned::new(
        ResidentNode {
            key: (1, 1),
            value,
            left: None,
            right: None,
            subtree_len: 1,
            height: 1,
        },
        domain,
        0,
    )
    .unwrap()
}

#[test]
fn imported_aliases_reuse_local_ids_but_equal_sequences_from_other_origins_do_not() {
    let domain = Domain::new(8 << 20);
    let root = leaf_with(imported_value(11, &domain), &domain);
    let alias = leaf_with(imported_value(11, &domain), &domain);
    assert!(!root.value.0.same(&alias.value.0));
    assert!(!root.value.type_name.0.same(&alias.value.type_name.0));
    assert_eq!(
        root.value.0.semantic_identity(),
        alias.value.0.semantic_identity()
    );
    assert_eq!(
        root.value.type_name.0.semantic_identity(),
        alias.value.type_name.0.semantic_identity()
    );
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    let (restored, reused) = index.intern((*alias).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(restored.same(&root));
    let independent = leaf_with(imported_value(22, &domain), &domain);
    let first_wire = root.value.0.wire_identity().unwrap();
    let other_wire = independent.value.0.wire_identity().unwrap();
    assert_eq!(first_wire.sequence, other_wire.sequence);
    assert_ne!(first_wire.origin, other_wire.origin);
    assert_ne!(
        root.value.0.semantic_identity(),
        independent.value.0.semantic_identity()
    );
    assert!(!equivalent(&root, &independent, &domain).unwrap());
    let (separate, reused) = index.intern((*independent).clone(), &mut || false).unwrap();
    assert!(!reused);
    assert!(!separate.same(&root));
    drop((root, alias, restored, independent, separate, index));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn matching_local_ids_still_check_every_scalar_and_text_content() {
    let domain = Domain::new(8 << 20);
    let root = leaf_with(imported_value(11, &domain), &domain);
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    let data = &*root.value;
    let conflicts = [
        AllocationData {
            address: 2,
            ..data.clone()
        },
        AllocationData {
            size: 33,
            ..data.clone()
        },
        AllocationData {
            line: 5,
            ..data.clone()
        },
        AllocationData {
            received_frame: 1,
            ..data.clone()
        },
        AllocationData {
            type_name: Text::with_wire_identity(
                "Other",
                &domain,
                &mut || false,
                Some(data.type_name.0.wire_identity().unwrap()),
            )
            .unwrap(),
            ..data.clone()
        },
        AllocationData {
            file: Text::with_wire_identity(
                "other.cpp",
                &domain,
                &mut || false,
                Some(data.file.0.wire_identity().unwrap()),
            )
            .unwrap(),
            ..data.clone()
        },
        AllocationData {
            type_name: Text::with_wire_identity(
                "Small",
                &domain,
                &mut || false,
                Some(identity::WireIdentity {
                    origin: [22; 16],
                    sequence: 2,
                }),
            )
            .unwrap(),
            ..data.clone()
        },
        AllocationData {
            file: Text::with_wire_identity(
                "small.cpp",
                &domain,
                &mut || false,
                Some(identity::WireIdentity {
                    origin: [22; 16],
                    sequence: 3,
                }),
            )
            .unwrap(),
            ..data.clone()
        },
    ];
    for conflict in conflicts {
        let conflicting = ResidentNode {
            value: ResidentValue(
                Owned::imported(conflict, &domain, 0, root.value.0.wire_identity().unwrap())
                    .unwrap(),
            ),
            ..(*root).clone()
        };
        assert_eq!(
            fingerprint(&root, &domain).unwrap(),
            fingerprint(&conflicting, &domain).unwrap()
        );
        assert!(!equivalent(&root, &conflicting, &domain).unwrap());
        let (restored, reused) = index.intern(conflicting, &mut || false).unwrap();
        assert!(!reused);
        assert!(!restored.same(&root));
    }
    drop((root, index));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn recycled_import_slot_cannot_revive_a_dead_group_with_the_same_local_hash() {
    let domain = Domain::new(8 << 20);
    let root = leaf_with(imported_value(11, &domain), &domain);
    let old_identity = root.value.0.semantic_identity();
    let old_hash = fingerprint(&root, &domain).unwrap();
    let weak = root.weak().unwrap();
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    assert_eq!(index.entries(), 1);
    drop(root);
    assert!(weak.upgrade().is_none());
    let replacement = leaf_with(imported_value(22, &domain), &domain);
    assert_eq!(replacement.value.0.semantic_identity(), old_identity);
    assert_eq!(fingerprint(&replacement, &domain).unwrap(), old_hash);
    let (restored, reused) = index.intern((*replacement).clone(), &mut || false).unwrap();
    assert!(!reused);
    assert!(!restored.same(&replacement));
    index.register(&replacement, &mut || false).unwrap();
    let (registered, reused) = index.intern((*replacement).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(registered.same(&replacement));
    assert!(weak.upgrade().is_none());
    drop((replacement, restored, registered, weak, index));
    assert_eq!(domain.statistics().logical_used, 0);
}

fn replace_leftmost(
    root: &Owned<ResidentNode>,
    value: ResidentValue,
    leaf_domain: &Domain,
    domain: &Domain,
) -> ResidentNode {
    let mut left = (**root.left.as_ref().unwrap()).clone();
    let mut leaf = (**left.left.as_ref().unwrap()).clone();
    leaf.value = value;
    left.left = Some(Owned::new(leaf, leaf_domain, 0).unwrap());
    ResidentNode {
        left: Some(Owned::new(left, domain, 0).unwrap()),
        ..(**root).clone()
    }
}

#[test]
fn nested_foreign_nodes_values_and_each_text_are_rejected_before_cache_reuse() {
    let domain = Domain::new(8 << 20);
    let foreign = Domain::new(8 << 20);
    let root = seven(&domain);
    let leaf = at_path(&root, 4).unwrap();
    let mut index = SmallSubtrees::new(&domain, true).unwrap();
    index.register(&root, &mut || false).unwrap();
    let mut foreign_type = (*leaf.value).clone();
    foreign_type.type_name = Text::new("Small", &foreign, &mut || false).unwrap();
    let mut foreign_file = (*leaf.value).clone();
    foreign_file.file = Text::new("small.cpp", &foreign, &mut || false).unwrap();
    let values = [
        value(1, &foreign),
        ResidentValue(Owned::new(foreign_type, &domain, 0).unwrap()),
        ResidentValue(Owned::new(foreign_file, &domain, 0).unwrap()),
    ];
    for value in values {
        let mixed = replace_leftmost(&root, value, &domain, &domain);
        assert!(index.intern(mixed.clone(), &mut || false).is_err());
        let mixed = Owned::new(mixed, &domain, 0).unwrap();
        assert!(index.register(&mixed, &mut || false).is_err());
    }
    let mixed = replace_leftmost(&root, leaf.value.clone(), &foreign, &domain);
    assert!(index.intern(mixed.clone(), &mut || false).is_err());
    let mixed = Owned::new(mixed, &domain, 0).unwrap();
    assert!(index.register(&mixed, &mut || false).is_err());
    assert_eq!(index.entries(), 7);
    let (restored, reused) = index.intern((*root).clone(), &mut || false).unwrap();
    assert!(reused);
    assert!(restored.same(&root));
    drop((mixed, leaf, root, restored, index));
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(foreign.statistics().logical_used, 0);
}

struct MapFixturePage {
    bytes: Vec<u8>,
    dependencies: Vec<u64>,
    ready: bool,
}
#[derive(Default)]
struct MapFixtureStore {
    next: u64,
    pages: BTreeMap<u64, MapFixturePage>,
}
#[derive(Clone, Default)]
struct MapFixtureIo(Arc<std::sync::Mutex<MapFixtureStore>>);
impl PageIo for MapFixtureIo {
    type Handle = u64;
    type Store = Arc<std::sync::Mutex<MapFixtureStore>>;
    type Reservation = ();
    fn store(&self) -> Self::Store {
        self.0.clone()
    }
    fn same(&self, store: &Self::Store) -> bool {
        Arc::ptr_eq(&self.0, store)
    }
    fn pin(&self, id: u64) -> Result<u64, Error> {
        self.0
            .lock()
            .unwrap()
            .pages
            .contains_key(&id)
            .then_some(id)
            .ok_or_else(|| error("missing map fixture page"))
    }
    fn id(&self, handle: &u64) -> u64 {
        *handle
    }
    fn committed(&self, handle: &u64) -> bool {
        self.0
            .lock()
            .unwrap()
            .pages
            .get(handle)
            .is_some_and(|page| page.ready)
    }
    fn begin(&mut self, _: &Arc<()>) -> Result<(), Error> {
        Ok(())
    }
    fn plan(&mut self, bytes: &[u8], dependencies: &[u64]) -> Result<u64, Error> {
        let mut store = self.0.lock().unwrap();
        store.next += 1;
        let id = store.next;
        store.pages.insert(
            id,
            MapFixturePage {
                bytes: bytes.to_vec(),
                dependencies: dependencies.to_vec(),
                ready: false,
            },
        );
        Ok(id)
    }
    fn commit(&mut self) -> Result<(), Error> {
        for page in self.0.lock().unwrap().pages.values_mut() {
            page.ready = true;
        }
        Ok(())
    }
    fn rollback(&mut self) {
        self.0.lock().unwrap().pages.retain(|_, page| page.ready);
    }
    fn invalidate_reader(&mut self) {}
    fn info(&self, id: u64) -> Result<(usize, usize), Error> {
        let store = self.0.lock().unwrap();
        let page = store
            .pages
            .get(&id)
            .ok_or_else(|| error("missing map fixture page"))?;
        Ok((page.bytes.len(), page.dependencies.len()))
    }
    fn read(
        &self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<LoadedPage, Error> {
        check_replay_cancelled(cancelled)?;
        let store = self.0.lock().unwrap();
        let page = store
            .pages
            .get(&id)
            .ok_or_else(|| error("missing map fixture page"))?;
        if !page.ready || page.bytes.len() > maximum {
            return Err(error("unready or oversized map fixture page"));
        }
        Ok(LoadedPage {
            bytes: page.bytes.clone(),
            dependencies: page.dependencies.clone(),
            _release_probe: None,
        })
    }
    fn reserve_metadata(&self, _: usize) -> Result<(), Error> {
        Ok(())
    }
}

fn map_fixture(
    root: &Owned<ResidentNode>,
    domain: &Domain,
    slots: usize,
) -> (ResidentReader<MapFixtureIo>, MapRef, Vec<u8>) {
    use crate::analysis::resident::write_maps::{ResidentBuild, memo::ResidentMemos};
    let source = ResidentMap {
        root: Some(root.clone()),
        len: root.subtree_len as usize,
        domain: domain.clone(),
    };
    let expected = postcard::to_stdvec(&source).unwrap();
    let mut io = MapFixtureIo::default();
    let budget = identity_table::MetadataBudget::new(0);
    let old = ResidentMemos::new(0, &budget);
    let reference = {
        let mut cancelled = || false;
        let mut build = ResidentBuild {
            identified: true,
            registry: None,
            io: &mut io,
            old: &old,
            staged: ResidentMemos::new(0, &budget),
            pins: BTreeMap::new(),
            statistics: Statistics::default(),
            cancelled: &mut cancelled,
        };
        build.map(&source).unwrap()
    };
    io.commit().unwrap();
    let mut reader = ResidentReader::dynamic(
        io,
        domain.clone(),
        ResidentLimits {
            tree_slots: 0,
            value_slots: 64,
            string_slots: 64,
            page_cache_bytes: 64 << 10,
        },
    )
    .unwrap();
    reader.small = SmallSubtrees::with_limits(domain, slots, MAX_BYTES).unwrap();
    (reader, reference, expected)
}

fn decode_fixture(
    reader: &mut ResidentReader<MapFixtureIo>,
    reference: MapRef,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<ResidentMap, Error> {
    let mut remaining = reference.charge.try_into()?;
    let root = reader.tree(
        reference,
        None,
        None,
        &mut Traversal::new(reference.len)?,
        &mut remaining,
        cancelled,
    )?;
    assert_eq!(remaining, 0);
    Ok(ResidentMap {
        root,
        len: reference.len.try_into()?,
        domain: reader.domain.clone(),
    })
}

#[test]
fn decoded_partial_group_keeps_its_root_and_avoids_repeated_registration_work() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let (mut reader, reference, expected) = map_fixture(&root, &domain, 64);
    reader.small.register(&root, &mut || false).unwrap();
    for slot in 0..reader.small.slots.len() {
        if reader.small.slots[slot].group.is_some() && reader.small.slots[slot].path != 1 {
            reader.small.remove(slot);
        }
    }
    assert_eq!(reader.small.entries(), 1);
    let restored = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert_eq!(postcard::to_stdvec(&restored).unwrap(), expected);
    assert!(restored.root.as_ref().unwrap().same(&root));
    assert!(reader.statistics.visited_nodes > 0);
    assert_eq!(reader.small.entries(), 1);
    let before = domain.statistics().logical_used;
    let mut checks = 0;
    let cancelled = decode_fixture(&mut reader, reference, &mut || {
        checks += 1;
        checks == 4
    })
    .err()
    .unwrap();
    assert_eq!(
        cancelled.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::Interrupted
    );
    assert_eq!(domain.statistics().logical_used, before);
    assert_eq!(reader.small.entries(), 1);
    assert_eq!(postcard::to_stdvec(&restored).unwrap(), expected);
    let retried = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert!(retried.root.as_ref().unwrap().same(&root));
    assert_eq!(postcard::to_stdvec(&retried).unwrap(), expected);
    assert_eq!(reader.small.entries(), 1);
    drop((retried, restored, root, reader));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn saturated_decoded_group_keeps_weak_lifetimes_and_can_rebuild_after_last_root_drops() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let weak = root.weak().unwrap();
    let (mut reader, reference, expected) = map_fixture(&root, &domain, 8);
    reader.small.register(&root, &mut || false).unwrap();
    assert_eq!(reader.small.entries(), 6);
    let restored = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert!(restored.root.as_ref().unwrap().same(&root));
    assert_eq!(reader.small.entries(), 6);
    drop(root);
    assert!(weak.upgrade().is_some());
    let next = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert!(
        next.root
            .as_ref()
            .unwrap()
            .same(restored.root.as_ref().unwrap())
    );
    assert_eq!(postcard::to_stdvec(&next).unwrap(), expected);
    drop((restored, next));
    assert!(weak.upgrade().is_none());
    assert_eq!(domain.statistics().live_strong_cells, reader.small.groups);
    assert_eq!(
        domain.statistics().live_strong_allocation_bytes,
        reader.small.groups * Owned::<Group>::fixed_bytes(),
    );
    reader.small.reclaim(&mut || false).unwrap();
    assert_eq!(reader.small.bytes(), 0);
    let fresh = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert_eq!(reader.small.entries(), 6);
    assert_eq!(postcard::to_stdvec(&fresh).unwrap(), expected);
    assert!(weak.upgrade().is_none());
    drop((fresh, weak, reader));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn decoded_replacement_root_keeps_its_group_when_an_older_partial_group_is_reclaimed() {
    let domain = Domain::new(8 << 20);
    let root = seven(&domain);
    let weak = root.weak().unwrap();
    let (mut reader, reference, expected) = map_fixture(&root, &domain, 64);
    reader.small.register(&root, &mut || false).unwrap();
    let root_slot = reader
        .small
        .slots
        .iter()
        .position(|slot| slot.group.is_some() && slot.path == 1)
        .unwrap();
    reader.small.remove(root_slot);
    assert_eq!(reader.small.entries(), 6);
    let restored = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert!(!restored.root.as_ref().unwrap().same(&root));
    assert_eq!(reader.small.entries(), 7);
    assert_eq!(postcard::to_stdvec(&restored).unwrap(), expected);
    assert_eq!(reader.small.groups, 2);
    drop(root);
    assert!(weak.upgrade().is_none());
    reader.small.reclaim(&mut || false).unwrap();
    assert_eq!(reader.small.entries(), 1);
    assert_eq!(reader.small.groups, 1);
    let next = decode_fixture(&mut reader, reference, &mut || false).unwrap();
    assert!(
        next.root
            .as_ref()
            .unwrap()
            .same(restored.root.as_ref().unwrap())
    );
    assert_eq!(postcard::to_stdvec(&next).unwrap(), expected);
    assert_eq!(reader.small.entries(), 1);
    drop((next, restored, reader, weak));
    assert_eq!(domain.statistics().logical_used, 0);
}
