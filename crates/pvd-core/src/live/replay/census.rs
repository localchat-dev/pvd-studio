use super::*;
use serde::Serialize;
use std::{cell::RefCell, collections::VecDeque, marker::PhantomData, rc::Rc};

const MAX_CALLS: usize = 64;
const MAX_DETAILS: usize = 4096;
const LOOKBACK_ENTRIES: usize = 64;
const NAME_BYTES: usize = 128;
const RESOURCE_ID: u64 = 1 << 63;

thread_local! {
    static ACTIVE: RefCell<Option<State>> = const { RefCell::new(None) };
}

#[derive(Default, Serialize)]
struct Counts {
    update_calls: u64,
    changed_object_entries: u64,
    object_read_attempts: u64,
    objects_decoded: u64,
    pxscene_objects_decoded: u64,
    value_uses: u64,
    value_decodes: u64,
    external_validated_reads: u64,
    external_value_hits: u64,
    external_reference_hits: u64,
    external_misses: u64,
    external_entries: usize,
    external_allowance_bytes: usize,
    value_local_hits: u64,
    reference_uses: u64,
    reference_decodes: u64,
    reference_local_hits: u64,
    reference_entries_cloned: u64,
    reference_arc_clones: u64,
    reference_entries_behind_cloned_arcs: u64,
    array_arc_clones: u64,
    array_items_behind_cloned_arcs: u64,
    bytes_arc_clones: u64,
    bytes_behind_cloned_arcs: u64,
    string_clone_bytes: u64,
    high_resource_uses: u64,
    low_file_offset_uses: u64,
    repeated_high_ids_from_prior_calls: u64,
    lookback_evictions: u64,
    pxscene_detail_rows: u64,
    detail_rows_dropped: u64,
    names_truncated: u64,
}

#[derive(Serialize)]
struct Call {
    ordinal: usize,
    label: String,
    target: usize,
    source_epoch: usize,
    from: Option<usize>,
    segment_first: Option<usize>,
    route: &'static str,
    success: bool,
    counts: Counts,
}

#[derive(Serialize)]
struct Detail {
    call: usize,
    source_epoch: usize,
    object_id: u64,
    generation: u32,
    type_name: String,
    property: String,
    record_kind: &'static str,
    offset: u64,
    high_resource: bool,
    local_hit: bool,
    prior_call_seen: bool,
    value_kind: &'static str,
    elements_or_bytes: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Value,
    References,
}

struct Seen {
    kind: Kind,
    id: u64,
    call: usize,
}

struct Use {
    kind: Kind,
    offset: u64,
    local_hit: bool,
    value_kind: &'static str,
    count: usize,
}

#[derive(Default)]
struct State {
    source: Option<resources::Resources>,
    source_epoch: usize,
    current: Option<Call>,
    calls: Vec<Call>,
    details: Vec<Detail>,
    lookback: VecDeque<Seen>,
}

#[derive(Serialize)]
pub(super) struct Report {
    scope: &'static str,
    maximum_calls: usize,
    maximum_detail_rows: usize,
    maximum_name_bytes: usize,
    maximum_lookback_entries: usize,
    calls: Vec<Call>,
    details: Vec<Detail>,
}

pub(super) struct Recorder {
    _thread: PhantomData<Rc<()>>,
}

impl Recorder {
    pub(super) fn start() -> Self {
        ACTIVE.with(|active| {
            assert!(active.borrow().is_none(), "nested scene census");
            *active.borrow_mut() = Some(State::default());
        });
        Self {
            _thread: PhantomData,
        }
    }

    pub(super) fn reset_lookback(&self) {
        visit(|state| {
            assert!(state.current.is_none());
            state.lookback.clear();
        });
    }

    pub(super) fn call<T, E>(
        &self,
        label: &str,
        target: usize,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        visit(|state| {
            assert!(state.current.is_none());
            assert!(state.calls.len() < MAX_CALLS, "scene census call limit");
            state.current = Some(Call {
                ordinal: state.calls.len(),
                label: bounded_name(label).0,
                target,
                source_epoch: state.source_epoch,
                from: None,
                segment_first: None,
                route: "not_entered",
                success: false,
                counts: Counts::default(),
            });
        });
        let result = operation();
        visit(|state| {
            let mut call = state.current.take().unwrap();
            call.success = result.is_ok();
            state.calls.push(call);
        });
        result
    }

    pub(super) fn finish(self) -> Report {
        ACTIVE.with(|active| {
            let state = active.borrow_mut().take().unwrap();
            assert!(state.current.is_none());
            Report {
                scope: "Counts only; external fields only; details select decoded PxScene objects. Arc payload sizes are observed, not copied-byte or allocation measurements. Lookback is 64 IDs, separate from decoded-external cache hits and allowance. No timer is installed.",
                maximum_calls: MAX_CALLS,
                maximum_detail_rows: MAX_DETAILS,
                maximum_name_bytes: NAME_BYTES,
                maximum_lookback_entries: LOOKBACK_ENTRIES,
                calls: state.calls,
                details: state.details,
            }
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            active.borrow_mut().take();
        });
    }
}

fn visit(operation: impl FnOnce(&mut State)) {
    ACTIVE.with(|active| {
        if let Some(state) = active.borrow_mut().as_mut() {
            operation(state);
        }
    });
}

fn add(value: &mut u64, amount: usize) {
    *value = value
        .checked_add(u64::try_from(amount).expect("scene census count width"))
        .expect("scene census counter overflow");
}

fn bounded_name(value: &str) -> (String, bool) {
    let mut end = value.len().min(NAME_BYTES);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), end != value.len())
}

fn is_scene(object: &Object) -> bool {
    object.type_name.rsplit("::").next() == Some("PxScene")
}

pub(super) fn route(
    resources: &resources::Resources,
    frame: usize,
    from: Option<usize>,
    segment_first: usize,
    path: &'static str,
) {
    visit(|state| {
        if state.current.is_none() {
            return;
        }
        if state
            .source
            .as_ref()
            .is_none_or(|source| !source.same(resources))
        {
            state.source = Some(resources.clone());
            state.source_epoch = state.source_epoch.checked_add(1).unwrap();
            state.lookback.clear();
        }
        let call = state.current.as_mut().unwrap();
        assert_eq!(call.target, frame);
        call.source_epoch = state.source_epoch;
        call.from = from;
        call.segment_first = Some(segment_first);
        call.route = path;
    });
}

pub(super) fn update(entries: usize) {
    visit(|state| {
        if let Some(call) = &mut state.current {
            add(&mut call.counts.update_calls, 1);
            add(&mut call.counts.changed_object_entries, entries);
        }
    });
}

pub(super) fn object_read() {
    visit(|state| {
        if let Some(call) = &mut state.current {
            add(&mut call.counts.object_read_attempts, 1);
        }
    });
}

pub(super) fn object_decoded(object: &Object) {
    visit(|state| {
        if let Some(call) = &mut state.current {
            add(&mut call.counts.objects_decoded, 1);
            if is_scene(object) {
                add(&mut call.counts.pxscene_objects_decoded, 1);
            }
        }
    });
}

pub(super) fn decoded_value() {
    visit(|state| {
        if let Some(call) = &mut state.current {
            add(&mut call.counts.value_decodes, 1);
        }
    });
}

pub(super) fn decoded_references() {
    visit(|state| {
        if let Some(call) = &mut state.current {
            add(&mut call.counts.reference_decodes, 1);
        }
    });
}

pub(super) fn external_read(hit: bool, value: bool, entries: usize, bytes: usize) {
    visit(|state| {
        if let Some(call) = &mut state.current {
            add(&mut call.counts.external_validated_reads, 1);
            if hit {
                add(
                    if value {
                        &mut call.counts.external_value_hits
                    } else {
                        &mut call.counts.external_reference_hits
                    },
                    1,
                );
            } else {
                add(&mut call.counts.external_misses, 1);
            }
            call.counts.external_entries = entries;
            call.counts.external_allowance_bytes = bytes;
        }
    });
}

fn use_record(state: &mut State, object: &Object, property: &str, observed: Use) {
    let Use {
        kind,
        offset,
        local_hit,
        value_kind,
        count,
    } = observed;
    let Some(call) = state.current.as_mut() else {
        return;
    };
    let high_resource = offset & RESOURCE_ID != 0;
    let prior_call_seen = if high_resource {
        add(&mut call.counts.high_resource_uses, 1);
        let previous = state
            .lookback
            .iter()
            .position(|entry| entry.kind == kind && entry.id == offset)
            .and_then(|position| state.lookback.remove(position));
        let prior = previous.is_some_and(|entry| entry.call != call.ordinal);
        if prior {
            add(&mut call.counts.repeated_high_ids_from_prior_calls, 1);
        }
        if state.lookback.len() == LOOKBACK_ENTRIES {
            state.lookback.pop_front();
            add(&mut call.counts.lookback_evictions, 1);
        }
        state.lookback.push_back(Seen {
            kind,
            id: offset,
            call: call.ordinal,
        });
        prior
    } else {
        add(&mut call.counts.low_file_offset_uses, 1);
        false
    };
    if !is_scene(object) {
        return;
    }
    if state.details.len() == MAX_DETAILS {
        add(&mut call.counts.detail_rows_dropped, 1);
        return;
    }
    let (type_name, type_truncated) = bounded_name(&object.type_name);
    let (property, property_truncated) = bounded_name(property);
    add(
        &mut call.counts.names_truncated,
        usize::from(type_truncated) + usize::from(property_truncated),
    );
    add(&mut call.counts.pxscene_detail_rows, 1);
    state.details.push(Detail {
        call: call.ordinal,
        source_epoch: state.source_epoch,
        object_id: object.id,
        generation: object.generation,
        type_name,
        property,
        record_kind: match kind {
            Kind::Value => "Value",
            Kind::References => "References",
        },
        offset,
        high_resource,
        local_hit,
        prior_call_seen,
        value_kind,
        elements_or_bytes: count,
    });
}

pub(super) fn value(object: &Object, property: &str, offset: u64, local_hit: bool, value: &Value) {
    visit(|state| {
        let Some(call) = &mut state.current else {
            return;
        };
        add(&mut call.counts.value_uses, 1);
        if local_hit {
            add(&mut call.counts.value_local_hits, 1);
        }
        let (kind, count) = match value {
            Value::Array(values) => {
                add(&mut call.counts.array_arc_clones, 1);
                add(
                    &mut call.counts.array_items_behind_cloned_arcs,
                    values.len(),
                );
                ("Array", values.len())
            }
            Value::Bytes(bytes) => {
                add(&mut call.counts.bytes_arc_clones, 1);
                add(&mut call.counts.bytes_behind_cloned_arcs, bytes.len());
                ("Bytes", bytes.len())
            }
            Value::String(text) => {
                add(&mut call.counts.string_clone_bytes, text.len());
                ("String", text.len())
            }
            _ => ("inline_scalar", 1),
        };
        use_record(
            state,
            object,
            property,
            Use {
                kind: Kind::Value,
                offset,
                local_hit,
                value_kind: kind,
                count,
            },
        );
    });
}

pub(super) fn references(
    object: &Object,
    property: &str,
    offset: u64,
    local_hit: bool,
    count: usize,
) {
    visit(|state| {
        let Some(call) = &mut state.current else {
            return;
        };
        add(&mut call.counts.reference_uses, 1);
        if local_hit {
            add(&mut call.counts.reference_local_hits, 1);
        }
        add(&mut call.counts.reference_arc_clones, 1);
        add(&mut call.counts.reference_entries_behind_cloned_arcs, count);
        use_record(
            state,
            object,
            property,
            Use {
                kind: Kind::References,
                offset,
                local_hit,
                value_kind: "generation_map",
                count,
            },
        );
    });
}

#[cfg(test)]
mod tests;
