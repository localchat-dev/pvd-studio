use super::*;

fn build(domain: &Domain, keys: impl IntoIterator<Item = u64>) -> Tree<u64, u64> {
    let mut draft = TreeDraft::new(domain, 0).unwrap();
    for key in keys {
        draft.insert(key, key + 100).unwrap();
    }
    draft.finish().unwrap()
}

fn assert_map(tree: &Tree<u64, u64>, expected: &BTreeMap<u64, u64>) {
    tree.validate().unwrap();
    assert_eq!(tree.len(), expected.len());
    assert_eq!(
        tree.iter()
            .map(|(k, v)| (*k, *v))
            .collect::<BTreeMap<_, _>>(),
        *expected
    );
    assert_eq!(
        tree.values().rev().copied().collect::<Vec<_>>(),
        expected.values().rev().copied().collect::<Vec<_>>()
    );
    let mut actual = tree.iter();
    let mut oracle = expected.iter();
    while oracle.len() != 0 {
        assert_eq!(actual.next(), oracle.next());
        assert_eq!(actual.len(), oracle.len());
        assert_eq!(actual.next_back(), oracle.next_back());
        assert_eq!(actual.len(), oracle.len());
    }
    assert_eq!(actual.next(), None);
    assert_eq!(actual.next_back(), None);
    assert_eq!(
        postcard::to_stdvec(tree).unwrap(),
        postcard::to_stdvec(expected).unwrap()
    );
}

#[test]
fn rotations_and_deletions_preserve_avl_order_and_old_roots() {
    let domain = Domain::new(16 << 20);
    for order in [[3, 2, 1], [1, 2, 3], [3, 1, 2], [1, 3, 2]] {
        let original = build(&domain, order);
        let mut expected: BTreeMap<_, _> = order.into_iter().map(|key| (key, key + 100)).collect();
        assert_map(&original, &expected);
        let mut current = original.clone();
        for key in order {
            let mut draft = current.edit(&domain, &mut || false).unwrap();
            assert_eq!(draft.remove(&key).unwrap(), expected.remove(&key));
            current = draft.finish().unwrap();
            assert_map(&current, &expected);
            assert_eq!(original.len(), 3);
        }
        assert!(current.same(&Tree::empty(&domain).unwrap()));
    }
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn mixed_metadata_operations_match_btree_and_keep_published_snapshots() {
    let domain = Domain::new(16 << 20);
    let mut current = Tree::<u64, u64>::empty(&domain).unwrap();
    let mut expected = BTreeMap::new();
    let mut random = 17u64;
    let mut snapshots = Vec::new();
    for step in 0..2400 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let key = random.rotate_left(11) % 1024;
        let mut draft = current.edit(&domain, &mut || false).unwrap();
        if random.is_multiple_of(3) {
            assert_eq!(draft.remove(&key).unwrap(), expected.remove(&key));
        } else {
            assert_eq!(
                draft.insert(key, random).unwrap(),
                expected.insert(key, random)
            );
        }
        current = draft.finish().unwrap();
        assert_map(&current, &expected);
        if step % 127 == 0 {
            snapshots.push((current.clone(), expected.clone()));
        }
    }
    for (snapshot, oracle) in &snapshots {
        assert_map(snapshot, oracle);
    }
    let before = domain.statistics();
    let no_change = TreeEdit::new(&current).finish().unwrap();
    assert!(no_change.same(&current));
    assert_eq!(
        domain.statistics().tree_copied_entries,
        before.tree_copied_entries
    );
    drop((current, no_change, snapshots));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn failure_at_any_path_allocation_cannot_publish_a_partial_update() {
    let domain = Domain::new(8 << 20);
    let old = build(&domain, 0..63);
    let expected: BTreeMap<_, _> = old.iter().map(|(k, v)| (*k, *v)).collect();
    let used = domain.statistics().logical_used;
    for remove in [false, true] {
        let mut failures = 0;
        let mut successes = 0;
        for headroom in (0..4096).step_by(32) {
            let hold = domain
                .reserve(domain.statistics().limit - used - headroom)
                .unwrap();
            let mut draft = old.edit(&domain, &mut || false).unwrap();
            let result = if remove {
                draft.remove(&31)
            } else {
                draft.insert(63, 163)
            };
            if result.is_err() {
                assert!(draft.finish().is_err());
                failures += 1;
            } else {
                let next = draft.finish().unwrap();
                next.validate().unwrap();
                assert_eq!(
                    next.len(),
                    if remove { old.len() - 1 } else { old.len() + 1 }
                );
                successes += 1;
            }
            drop(hold);
            assert_eq!(domain.statistics().logical_used, used);
            assert_map(&old, &expected);
        }
        assert!(failures > 0 && successes > 0);
    }
    drop(old);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn cancellation_poisoning_includes_existing_drafts_and_initial_editor_creation() {
    let domain = Domain::new(8 << 20);
    let old = build(&domain, 0..127);
    let wire = postcard::to_stdvec(&old).unwrap();
    let used = domain.statistics().logical_used;
    for allowed in 0..40 {
        let mut edit = TreeEdit::new(&old);
        let mut calls = 0;
        let result = edit.insert(200, 300, &mut || {
            calls += 1;
            calls > allowed
        });
        if result.is_err() {
            assert!(edit.finish().is_err());
        } else {
            drop(edit.finish().unwrap());
        }
        assert_eq!(domain.statistics().logical_used, used);
        assert_eq!(postcard::to_stdvec(&old).unwrap(), wire);
    }
    let mut edit = TreeEdit::new(&old);
    edit.insert(1000, 1100, &mut || false).unwrap();
    assert!(edit.remove(&2, &mut || true).is_err());
    assert!(edit.finish().is_err());
    drop(old);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn text_export_and_weak_root_do_not_pin_unrelated_metadata_nodes() {
    let domain = Domain::new(4 << 20);
    let text = Text::new("retained", &domain, &mut || false).unwrap();
    let mut draft = TreeDraft::new(&domain, 0).unwrap();
    for key in 0..1000u64 {
        draft.insert(key, text.clone()).unwrap();
    }
    let tree = draft.finish().unwrap();
    let root = tree.root.as_ref().unwrap().weak().unwrap();
    let value = tree.get(&10).unwrap().clone();
    let text_weak = value.0.weak().unwrap();
    drop((tree, text));
    assert!(root.upgrade().is_none());
    assert_eq!(domain.statistics().live_strong_cells, 1);
    assert_eq!(&*value, "retained");
    assert!(domain.statistics().logical_used > domain.statistics().logical_weak_fixed_bytes);
    drop(value);
    assert!(text_weak.upgrade().is_none());
    assert_eq!(domain.statistics().live_strong_cells, 0);
    assert_eq!(
        domain.statistics().logical_used,
        domain.statistics().logical_weak_fixed_bytes
    );
    drop((root, text_weak));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn foreign_nested_handles_and_invalid_hints_are_rejected_without_leaks() {
    let local = Domain::new(1 << 20);
    let foreign = Domain::new(1 << 20);
    let text = Text::new("foreign", &foreign, &mut || false).unwrap();
    let tree = Tree::<u64, Text>::empty(&local).unwrap();
    assert!(tree.edit(&foreign, &mut || false).is_err());
    assert!(TreeDraft::<u64, u64>::new(&local, MAX_ENTRIES + 1).is_err());
    let mut draft = tree.edit(&local, &mut || false).unwrap();
    assert!(draft.insert(1, text.clone()).is_err());
    assert!(draft.finish().is_err());
    let mut edit = TreeEdit::new(&tree);
    assert!(edit.insert(1, text.clone(), &mut || false).is_err());
    assert!(edit.finish().is_err());
    assert_eq!(local.statistics().logical_used, 0);
    drop((tree, text));
    assert_eq!(foreign.statistics().logical_used, 0);
}

#[test]
fn editing_large_metadata_copies_only_a_bounded_search_path() {
    let domain = Domain::new(16 << 20);
    let old = build(&domain, 0..8192);
    let before = domain.statistics();
    let mut edit = TreeEdit::new(&old);
    assert_eq!(edit.insert(4000, 1, &mut || false).unwrap(), Some(4100));
    let next = edit.finish().unwrap();
    let copied = domain.statistics().tree_copied_entries - before.tree_copied_entries;
    assert!(copied > 0 && copied < MAX_HEIGHT);
    assert_eq!(
        domain.statistics().tree_copied_layout_bytes - before.tree_copied_layout_bytes,
        copied * 2 * size_of::<u64>()
    );
    assert_eq!(old.get(&4000), Some(&4100));
    assert_eq!(next.get(&4000), Some(&1));
    next.validate().unwrap();
    drop((old, next));
    assert_eq!(domain.statistics().logical_used, 0);
}
