use super::*;

fn collect(buffer: BufferView<'_, u64>) -> Vec<u64> {
    buffer.iter().copied().collect()
}

fn build(domain: &Domain, count: usize) -> Buffer<u64> {
    let mut draft = BufferDraft::new(domain, count).unwrap();
    for value in 0..count {
        draft.push(value as u64).unwrap();
    }
    draft.finish().unwrap()
}

fn validate<T>(root: &Link<T>) -> (usize, u8) {
    let Some(node) = root else { return (0, 0) };
    let (left_len, left_height) = validate(&node.left);
    let (right_len, right_height) = validate(&node.right);
    assert!(!node.chunk.is_empty());
    assert!(node.chunk.len() <= CHUNK_LIMIT);
    assert!(node.chunk.capacity() <= CHUNK_LIMIT);
    assert_eq!(node.len, left_len + node.chunk.len() + right_len);
    assert_eq!(node.height, 1 + left_height.max(right_height));
    assert!((i16::from(left_height) - i16::from(right_height)).abs() <= 1);
    (node.len, node.height)
}

#[test]
fn chunk_boundaries_and_ranked_bidirectional_ranges_match_vec() {
    let domain = Domain::new(16 << 20);
    for count in [0, 1, 255, 256, 257, 511, 512, 513, 4097] {
        let buffer = build(&domain, count);
        validate(&buffer.root);
        let expected: Vec<_> = (0..count as u64).collect();
        assert_eq!(collect(buffer.view()), expected);
        assert_eq!(
            buffer.view().iter().rev().copied().collect::<Vec<_>>(),
            expected.iter().rev().copied().collect::<Vec<_>>()
        );
        assert_eq!(buffer.view().get(count), None);
        assert_eq!(buffer.view().first(), expected.first());
        assert_eq!(buffer.view().last(), expected.last());
        assert!(buffer.view().range(Range { start: 1, end: 0 }).is_err());
        assert!(buffer.view().range(0..count + 1).is_err());
        for start in [0, count / 3, count / 2, count] {
            for end in [start, (start + count) / 2, count] {
                let mut actual = buffer.view().range(start..end).unwrap();
                let mut expected = expected[start..end].iter();
                while !expected.as_slice().is_empty() {
                    assert_eq!(actual.next(), expected.next());
                    assert_eq!(actual.len(), expected.len());
                    assert_eq!(actual.next_back(), expected.next_back());
                    assert_eq!(actual.len(), expected.len());
                }
                assert_eq!(actual.next(), None);
                assert_eq!(actual.next_back(), None);
            }
        }
        for point in [0, 1, 255, 256, count, count + 1] {
            assert_eq!(
                buffer.view().partition_point(|value| *value < point as u64),
                expected.partition_point(|value| *value < point as u64)
            );
        }
        assert_eq!(
            postcard::to_stdvec(&buffer).unwrap(),
            postcard::to_stdvec(&expected).unwrap()
        );
    }
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn draft_tail_is_readable_and_pop_crosses_chunks_without_mutating_old_roots() {
    let domain = Domain::new(16 << 20);
    let original = build(&domain, 1025);
    let wire = postcard::to_stdvec(&original).unwrap();
    let mut draft = original.edit(&domain, &mut || false).unwrap();
    let mut expected: Vec<_> = (0..1025).collect();
    for _ in 0..769 {
        assert_eq!(draft.pop().unwrap(), expected.pop());
        assert_eq!(draft.last(), expected.last());
        assert_eq!(collect(draft.view()), expected);
    }
    for value in 2000..2300 {
        draft.push(value).unwrap();
        expected.push(value);
    }
    assert_eq!(
        draft.view().iter().rev().copied().collect::<Vec<_>>(),
        expected.iter().rev().copied().collect::<Vec<_>>()
    );
    let next = draft.finish().unwrap();
    validate(&next.root);
    assert_eq!(collect(next.view()), expected);
    assert_eq!(postcard::to_stdvec(&original).unwrap(), wire);
    let mut empty = next.edit(&domain, &mut || false).unwrap();
    for _ in 0..next.len() {
        assert!(empty.pop().unwrap().is_some());
    }
    assert!(empty.pop().unwrap().is_none());
    empty.push(9000).unwrap();
    assert_eq!(collect(empty.finish().unwrap().view()), [9000]);
    drop((original, next));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn filtering_and_prefix_removal_preserve_order_and_share_unchanged_chunks() {
    let domain = Domain::new(32 << 20);
    let old = build(&domain, 8193);
    let wire = postcard::to_stdvec(&old).unwrap();
    let mut keep_all = old.edit(&domain, &mut || false).unwrap();
    let before = domain.statistics().buffer_copied_elements;
    keep_all.retain(|_| true, &mut || false).unwrap();
    let keep_all = keep_all.finish().unwrap();
    assert_eq!(domain.statistics().buffer_copied_elements, before);
    assert!(same_link(&old.root, &keep_all.root));
    for divisor in [2, 3, 255, 257, 10000] {
        let mut draft = old.edit(&domain, &mut || false).unwrap();
        draft
            .retain(|value| value % divisor == 0, &mut || false)
            .unwrap();
        draft.compact().unwrap();
        let filtered = draft.finish().unwrap();
        validate(&filtered.root);
        assert_eq!(filtered.capacity(), filtered.len());
        assert_eq!(
            collect(filtered.view()),
            (0..8193)
                .filter(|value| value % divisor == 0)
                .collect::<Vec<_>>()
        );
    }
    for count in [0, 1, 255, 256, 257, 4096, 8192, 8193] {
        let mut draft = old.edit(&domain, &mut || false).unwrap();
        let before = domain.statistics().buffer_copied_elements;
        draft.remove_prefix(count).unwrap();
        let next = draft.finish().unwrap();
        validate(&next.root);
        assert!(domain.statistics().buffer_copied_elements - before <= CHUNK_LIMIT);
        assert_eq!(
            collect(next.view()),
            (count as u64..8193).collect::<Vec<_>>()
        );
    }
    let mut mixed = old.edit(&domain, &mut || false).unwrap();
    mixed.retain(|value| value % 3 != 0, &mut || false).unwrap();
    mixed.remove_prefix(1025).unwrap();
    for value in 9000..9500 {
        mixed.push(value).unwrap();
    }
    let mixed = mixed.finish().unwrap();
    validate(&mixed.root);
    assert_eq!(
        collect(mixed.view()),
        (0..8193)
            .filter(|value| value % 3 != 0)
            .skip(1025)
            .chain(9000..9500)
            .collect::<Vec<_>>()
    );
    assert_eq!(postcard::to_stdvec(&old).unwrap(), wire);
    drop((old, mixed, keep_all));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn append_copy_work_is_bounded_by_one_tail_for_small_and_large_prefixes() {
    for count in [257, 2561, 25601] {
        let domain = Domain::new(32 << 20);
        let old = build(&domain, count);
        let before = domain.statistics();
        let clone = old.clone();
        assert!(old.same(&clone));
        assert_eq!(
            domain.statistics().buffer_copied_elements,
            before.buffer_copied_elements
        );
        let mut draft = old.edit(&domain, &mut || false).unwrap();
        for value in 0..700 {
            draft.push(value).unwrap();
        }
        let next = draft.finish().unwrap();
        assert_eq!(
            domain.statistics().buffer_copied_elements - before.buffer_copied_elements,
            1
        );
        assert_eq!(
            domain.statistics().buffer_copied_layout_bytes - before.buffer_copied_layout_bytes,
            size_of::<u64>()
        );
        assert_eq!(next.len(), count + 700);
        assert!(domain.statistics().buffer_allocated_nodes - before.buffer_allocated_nodes < 80);
        validate(&next.root);
        assert_eq!(old.len(), count);
        drop((old, clone, next));
        assert_eq!(domain.statistics().logical_used, 0);
    }
}

#[test]
fn failed_and_cancelled_drafts_cannot_publish_a_partial_sequence() {
    let domain = Domain::new(4 << 20);
    let old = build(&domain, 2049);
    let wire = postcard::to_stdvec(&old).unwrap();
    let initial = domain.statistics().logical_used;
    let held = domain.reserve(domain.statistics().limit - initial).unwrap();
    let mut draft = old.edit(&domain, &mut || false).unwrap();
    assert!(draft.push(99).is_err());
    assert!(draft.finish().is_err());
    drop(held);
    assert_eq!(domain.statistics().logical_used, initial);
    assert_eq!(postcard::to_stdvec(&old).unwrap(), wire);
    for limit in 0..12 {
        let mut draft = old.edit(&domain, &mut || false).unwrap();
        let mut calls = 0;
        let result = draft.retain(|value| value % 2 == 0, &mut || {
            calls += 1;
            calls > limit
        });
        if result.is_err() {
            assert!(draft.finish().is_err());
        } else {
            drop(draft.finish().unwrap());
        }
        assert_eq!(domain.statistics().logical_used, initial);
        assert_eq!(postcard::to_stdvec(&old).unwrap(), wire);
    }
    let mut invalid = old.edit(&domain, &mut || false).unwrap();
    assert!(invalid.remove_prefix(old.len() + 1).is_err());
    assert!(invalid.finish().is_err());
    assert!(BufferDraft::<u64>::new(&domain, usize::MAX).is_err());
    let foreign = Domain::new(1024);
    assert!(old.edit(&foreign, &mut || false).is_err());
    let untouched = BufferEdit::new(&old).finish().unwrap();
    assert!(untouched.same(&old));
    drop(untouched);
    let mut interrupted = BufferEdit::new(&old);
    interrupted.push(9000, &mut || false).unwrap();
    assert!(interrupted.push(9001, &mut || true).is_err());
    assert!(interrupted.finish().is_err());
    drop(old);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn chunks_release_payload_with_only_weak_identity_and_exported_items_remaining() {
    let domain = Domain::new(4 << 20);
    let mut draft = BufferDraft::new(&domain, 0).unwrap();
    let text = Text::new("tracked", &domain, &mut || false).unwrap();
    for _ in 0..300 {
        draft.push(text.clone()).unwrap();
    }
    let buffer = draft.finish().unwrap();
    let chunk = buffer.root.as_ref().unwrap().chunk.clone();
    let weak = chunk.weak().unwrap();
    let exported = buffer.view().get(0).unwrap().clone();
    let used = domain.statistics().logical_used;
    drop((chunk, buffer, text));
    assert!(domain.statistics().logical_used < used);
    assert_eq!(&*exported, "tracked");
    assert!(weak.upgrade().is_none());
    drop(weak);
    assert!(domain.statistics().logical_used > 0);
    drop(exported);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn virtual_capacity_preserves_legacy_growth_and_does_not_preallocate_wire_size() {
    let domain = Domain::new(4 << 20);
    let baseline = domain.statistics().logical_used;
    let mut hinted = BufferDraft::<u64>::new(&domain, crate::analysis::MAX_RECORDS).unwrap();
    assert_eq!(domain.statistics().logical_used, baseline);
    hinted.push(1).unwrap();
    let hinted = hinted.finish().unwrap();
    assert_eq!(hinted.capacity(), crate::analysis::MAX_RECORDS);
    assert!(domain.statistics().logical_used - baseline < 4096);
    let old = build(&domain, 3);
    let mut next = old.edit(&domain, &mut || false).unwrap();
    next.push(4).unwrap();
    let next = next.finish().unwrap();
    assert_eq!(next.capacity(), 6);
    assert_eq!(collect(next.view()), [0, 1, 2, 4]);
    let mut compact = next.edit(&domain, &mut || false).unwrap();
    compact.compact().unwrap();
    assert_eq!(compact.finish().unwrap().capacity(), 4);
    drop((old, next, hinted));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn budget_failures_across_tail_and_path_construction_leave_the_published_root_unchanged() {
    let domain = Domain::new(8 << 20);
    let old = build(&domain, 8193);
    let wire = postcard::to_stdvec(&old).unwrap();
    let used = domain.statistics().logical_used;
    let mut failures = 0;
    let mut successes = 0;
    for headroom in (0..8192).step_by(64) {
        let hold = domain
            .reserve(domain.statistics().limit - used - headroom)
            .unwrap();
        let mut draft = old.edit(&domain, &mut || false).unwrap();
        if draft.push(9000).is_err() {
            assert!(draft.finish().is_err());
            failures += 1;
        } else {
            match draft.finish() {
                Ok(next) => {
                    assert_eq!(next.len(), old.len() + 1);
                    assert_eq!(next.view().last(), Some(&9000));
                    validate(&next.root);
                    successes += 1;
                }
                Err(_) => failures += 1,
            }
        }
        drop(hold);
        assert_eq!(domain.statistics().logical_used, used);
        assert_eq!(postcard::to_stdvec(&old).unwrap(), wire);
    }
    assert!(failures > 0 && successes > 0);
    drop(old);
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn mixed_persistent_edits_preserve_snapshots_and_rank_after_sparse_filters() {
    let domain = Domain::new(32 << 20);
    let original = build(&domain, 4097);
    let mut current = original.clone();
    let mut expected = (0..4097).collect::<Vec<_>>();
    let mut random = 7u64;
    for step in 0..600 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut draft = current.edit(&domain, &mut || false).unwrap();
        match random % 5 {
            0 | 1 => {
                for value in 0..17 {
                    draft.push(random.wrapping_add(value)).unwrap();
                    expected.push(random.wrapping_add(value));
                }
            }
            2 => {
                assert_eq!(draft.pop().unwrap(), expected.pop());
            }
            3 => {
                let count = (random as usize % 11).min(expected.len());
                draft.remove_prefix(count).unwrap();
                expected.drain(..count);
            }
            _ => {
                let remove = random % 31;
                draft
                    .retain(|value| value % 31 != remove, &mut || false)
                    .unwrap();
                expected.retain(|value| value % 31 != remove);
            }
        }
        assert_eq!(collect(draft.view()), expected);
        current = draft.finish().unwrap();
        validate(&current.root);
        if step % 23 == 0 {
            let mut snapshot = current.edit(&domain, &mut || false).unwrap();
            snapshot.retain(|_| false, &mut || false).unwrap();
            assert!(snapshot.finish().unwrap().view().is_empty());
            assert_eq!(collect(current.view()), expected);
        }
    }
    assert_eq!(collect(original.view()), (0..4097).collect::<Vec<_>>());
    drop((original, current));
    assert_eq!(domain.statistics().logical_used, 0);
}

#[test]
fn a_foreign_item_poisoned_draft_cannot_publish_and_local_handles_remain_budgeted() {
    let local = Domain::new(4 << 20);
    let foreign = Domain::new(4 << 20);
    let value = Text::new("foreign", &foreign, &mut || false).unwrap();
    let mut draft = BufferDraft::new(&local, 0).unwrap();
    assert!(draft.push(value.clone()).is_err());
    assert!(draft.finish().is_err());
    assert_eq!(local.statistics().logical_used, 0);
    assert!(foreign.statistics().logical_used > 0);
    drop(value);
    assert_eq!(foreign.statistics().logical_used, 0);
}

#[test]
fn reverse_tail_edits_copy_only_the_chunks_actually_crossed() {
    let domain = Domain::new(16 << 20);
    let old = build(&domain, 4097);
    let before = domain.statistics();
    let initial_height = usize::from(height(&old.root));
    let mut draft = old.edit(&domain, &mut || false).unwrap();
    for value in (3840..4097).rev() {
        assert_eq!(draft.pop().unwrap(), Some(value));
    }
    let next = draft.finish().unwrap();
    assert_eq!(
        domain.statistics().buffer_copied_elements - before.buffer_copied_elements,
        257
    );
    assert_eq!(
        domain.statistics().buffer_copied_layout_bytes - before.buffer_copied_layout_bytes,
        257 * size_of::<u64>()
    );
    assert!(
        domain.statistics().buffer_allocated_nodes - before.buffer_allocated_nodes
            <= 4 * (initial_height + 1)
    );
    assert_eq!(next.len(), 3840);
    assert_eq!(old.len(), 4097);
    validate(&next.root);
    drop((old, next));
    assert_eq!(domain.statistics().logical_used, 0);
}
