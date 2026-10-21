use super::*;

fn repeated(domain: &Domain) -> ResidentAnalysis {
    let mut analysis = ResidentAnalysis::new(domain.clone()).unwrap();
    let mut dictionary = TreeDraft::new(domain, 2048).unwrap();
    for key in 0..2048u32 {
        dictionary
            .insert(
                (1, key),
                Text::new(&format!("name-{key}"), domain, &mut || false).unwrap(),
            )
            .unwrap();
    }
    let dictionary = dictionary.finish().unwrap();
    let allocation = ResidentValue::new(
        AllocationData {
            address: 7,
            size: 8,
            type_name: dictionary.get(&(1, 0)).unwrap().clone(),
            file: dictionary.get(&(1, 1)).unwrap().clone(),
            line: 1,
            received_frame: 0,
        },
        domain,
    )
    .unwrap();
    let allocate = EventRef(
        Owned::new(
            EventData {
                buffer: 1,
                address: 7,
                received_frame: 0,
                kind: EventKind::Allocate {
                    allocation: allocation.clone(),
                    replaced: None,
                },
            },
            domain,
            0,
        )
        .unwrap(),
    );
    let free = EventRef(
        Owned::new(
            EventData {
                buffer: 1,
                address: 7,
                received_frame: 0,
                kind: EventKind::Free {
                    allocation: Some(allocation),
                },
            },
            domain,
            0,
        )
        .unwrap(),
    );
    let mut events = BufferDraft::new(domain, 5000).unwrap();
    for _ in 0..2500 {
        events.push(allocate.clone()).unwrap();
        events.push(free.clone()).unwrap();
    }
    let mut data = (*analysis.data).clone();
    let mut memory = (*data.memory).clone();
    memory.strings = dictionary;
    memory.events = events.finish().unwrap();
    memory.allocations = 2500;
    memory.frees = 2500;
    memory.total_allocated_bytes = 20_000;
    memory.peak_bytes = 8;
    data.memory = Owned::new(memory, domain, 0).unwrap();
    analysis.publish(data, &mut || false).unwrap();
    analysis
}

#[test]
fn large_reference_hints_cannot_prevent_a_small_unique_recount_from_completing() {
    let domain = Domain::new(8 << 20);
    let mut analysis = repeated(&domain);
    let old = analysis.clone();
    let wire = postcard::to_stdvec(&old).unwrap();
    let used = domain.statistics().logical_used;
    let hold = domain
        .reserve(domain.statistics().limit - used - (90 << 10))
        .unwrap();
    let before = domain.statistics().logical_used;
    let failure = identity_bytes(
        analysis.memory(),
        &domain,
        Some((5000, 12_048)),
        &mut || false,
    )
    .unwrap_err();
    assert!(scratch::allocation_failure(&failure));
    assert_eq!(domain.statistics().logical_used, before);
    let plain = identity_bytes(analysis.memory(), &domain, None, &mut || false).unwrap();
    assert!(plain > 0);
    assert_eq!(domain.statistics().logical_used, before);
    analysis.try_retain_recent(0, &mut || false).unwrap();
    assert_eq!(postcard::to_stdvec(&old).unwrap(), wire);
    assert_eq!(analysis.memory().events().len(), 5000);
    assert_eq!(analysis.memory().allocations, 2500);
    drop((analysis, old, hold));
    assert_eq!(domain.statistics().logical_used, 0);
    assert_eq!(domain.statistics().live_strong_cells, 0);
}

#[test]
fn cancellation_is_not_retried_as_a_memory_allocation_failure() {
    let domain = Domain::new(8 << 20);
    let analysis = repeated(&domain);
    let before = domain.statistics().logical_used;
    let mut calls = 0;
    let failure = identity_bytes(
        analysis.memory(),
        &domain,
        Some((5000, 12_048)),
        &mut || {
            calls += 1;
            true
        },
    )
    .unwrap_err();
    assert!(!scratch::allocation_failure(&failure));
    assert_eq!(calls, 1);
    assert_eq!(domain.statistics().logical_used, before);
    drop(analysis);
    assert_eq!(domain.statistics().logical_used, 0);
}
