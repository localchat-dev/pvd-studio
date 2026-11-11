use super::*;
use crate::native::{read_source, snapshot};

mod array_undo;
mod initial_source;
mod message_alias;
mod origin_history;

pub(super) use initial_source::Proof as InitialProof;
pub(super) use origin_history::Proof as OriginProof;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RawInstance {
    class: u32,
    values: BTreeMap<u32, Vec<u8>>,
    defined: BTreeSet<u32>,
}

type RawState = BTreeMap<u32, RawInstance>;

pub(super) fn validate_message_instance(
    metadata: &Metadata,
    id: u32,
    mut class: u32,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let message = metadata
        .messages
        .get(id as usize)
        .and_then(Option::as_ref)
        .ok_or_else(|| error("undefined property message"))?;
    let mut ancestors = Vec::new();
    while class != u32::MAX {
        check(proceed)?;
        if ancestors.len() == 128 || ancestors.contains(&class) {
            return Err(error("invalid property-message class inheritance"));
        }
        ancestors.push(class);
        let description = metadata.class(class)?;
        if class == message.class {
            return Ok(());
        }
        class = description.base;
    }
    Err(error("property message does not belong to instance class"))
}

pub(super) fn validate_message(
    metadata: &Metadata,
    id: u32,
    data: &[u8],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    check(proceed)?;
    let message = metadata
        .messages
        .get(id as usize)
        .and_then(Option::as_ref)
        .ok_or_else(|| error("undefined property message"))?;
    if metadata.class(message.class)?.name != message.class_name {
        return Err(error("property-message owner identity is inconsistent"));
    }
    let mut tail = data
        .get(message.size..)
        .ok_or_else(|| error("property-message payload is truncated"))?;
    let has_tail = !tail.is_empty();
    for entry in &message.entries {
        check(proceed)?;
        let property = metadata.property(entry.property)?;
        let datatype = metadata.class(property.datatype_id)?;
        let incoming = metadata.class(entry.datatype_id)?;
        if datatype.name != property.datatype || incoming.name != entry.datatype {
            return Err(error("property-message datatype identity is inconsistent"));
        }
        if property.kind == 2 {
            return Err(error(
                "frame trim does not support array-target property messages",
            ));
        }
        if datatype.dynamic || incoming.dynamic {
            return Err(error("property-message requires a fixed datatype layout"));
        }
        if incoming.size == 0
            || entry.size != incoming.size
            || entry.destination_size != datatype.size
            || entry.size != entry.destination_size
        {
            return Err(error(
                "property-message field width disagrees with datatype",
            ));
        }
        let bool_alias =
            message_alias::is_bool_alias(metadata, entry.datatype_id, property.datatype_id);
        if entry.datatype_id != property.datatype_id {
            let names = (|| {
                Some((
                    metadata.strings.get(&incoming.name.namespace)?.as_str(),
                    metadata.strings.get(&incoming.name.name)?.as_str(),
                    metadata.strings.get(&datatype.name.namespace)?.as_str(),
                    metadata.strings.get(&datatype.name.name)?.as_str(),
                ))
            })();
            let handle_mapping = matches!(
                names,
                Some(("physx3", "String", "physx3", "StringHandle"))
                    | Some(("physx3", "VoidPtr", "physx3", "ObjectRef"))
            ) && entry.size == 4
                && incoming.has_pointers
                && datatype.has_pointers;
            if !handle_mapping
                && !message_alias::is_bit_copy(metadata, entry.datatype_id, property.datatype_id)
                && !bool_alias
            {
                return Err(error(
                    "property-message datatype conversion is not verified",
                ));
            }
        }
        let end = entry
            .offset
            .checked_add(entry.size)
            .filter(|&end| end <= message.size)
            .ok_or_else(|| error("message field offset overflow"))?;
        let field = data
            .get(entry.offset..end)
            .ok_or_else(|| error("message field exceeds payload"))?;
        if bool_alias && !matches!(field, [0] | [1]) {
            return Err(error(
                "property-message boolean alias requires canonical 0 or 1",
            ));
        }
        if !has_tail {
            continue;
        }
        for value in field.chunks_exact(datatype.size) {
            for &offset in &datatype.string_offsets {
                check(proceed)?;
                let end = offset
                    .checked_add(4)
                    .ok_or_else(|| error("message string offset overflow"))?;
                let handle = u32::from_le_bytes(
                    value
                        .get(offset..end)
                        .ok_or_else(|| error("message string exceeds datatype"))?
                        .try_into()?,
                );
                let string = metadata
                    .strings
                    .get(&handle)
                    .ok_or_else(|| error("undefined message string handle"))?;
                tail = tail
                    .strip_prefix(string.as_bytes())
                    .and_then(|tail| tail.strip_prefix(&[0]))
                    .ok_or_else(|| error("message string tail disagrees with captured handles"))?;
            }
        }
    }
    if !tail.is_empty() {
        return Err(error("unrecognized property-message trailing data"));
    }
    Ok(())
}

fn properties(metadata: &Metadata, class: u32) -> Result<Vec<u32>, Error> {
    let mut ancestors = Vec::new();
    let mut current = class;
    while current != u32::MAX {
        if ancestors.len() == 128 || ancestors.contains(&current) {
            return Err(error("invalid snapshot class inheritance"));
        }
        ancestors.push(current);
        current = metadata.class(current)?.base;
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for class in ancestors.into_iter().rev() {
        for &id in &metadata.class(class)?.properties {
            if !seen.insert(id) {
                return Err(error("duplicate snapshot property"));
            }
            let property = metadata.property(id)?;
            let datatype = metadata.class(property.datatype_id)?;
            if datatype.dynamic || datatype.size == 0 {
                return Err(error(
                    "middle trim does not support nested dynamic datatypes",
                ));
            }
            result.push(id);
        }
    }
    Ok(result)
}

impl RawInstance {
    fn restore(metadata: &Metadata, record: &Saved<'_>) -> Result<Self, Error> {
        let mut result = Self::create(metadata, record.class)?;
        let mut reader = Reader::new(&record.body);
        let fixed = reader.blob()?;
        if fixed.len() < metadata.class(record.class)?.size {
            return Err(error("saved fixed data is shorter than its class"));
        }
        for id in properties(metadata, record.class)? {
            let p = metadata.property(id)?;
            let data = if p.kind == 2 {
                reader.blob()?
            } else {
                let end = p
                    .offset32
                    .checked_add(metadata.class(p.datatype_id)?.size)
                    .ok_or_else(|| error("saved property offset overflow"))?;
                fixed
                    .get(p.offset32..end)
                    .ok_or_else(|| error("saved property exceeds fixed data"))?
            };
            result.set(metadata, id, data)?;
        }
        reader.end()?;
        Ok(result)
    }

    fn create(metadata: &Metadata, class: u32) -> Result<Self, Error> {
        Self::create_bounded(metadata, class, 0, &mut || true)
    }

    fn create_bounded(
        metadata: &Metadata,
        class: u32,
        mut allocated: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        let properties = properties(metadata, class)?;
        metadata_budget(&mut allocated, 1, 128)?;
        for &id in &properties {
            check(proceed)?;
            let property = metadata.property(id)?;
            let width = if property.kind == 2 {
                0
            } else {
                metadata.class(property.datatype_id)?.size
            };
            metadata_budget(&mut allocated, width, 1)?;
            metadata_budget(&mut allocated, 1, 96)?;
        }
        let mut values = BTreeMap::new();
        let mut defined = BTreeSet::new();
        for id in properties {
            check(proceed)?;
            let p = metadata.property(id)?;
            let size = if p.kind == 2 {
                0
            } else {
                metadata.class(p.datatype_id)?.size
            };
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(size)?;
            metadata_budget(&mut allocated, bytes.capacity().saturating_sub(size), 1)?;
            resize_raw(&mut bytes, size, proceed)?;
            values.insert(id, bytes);
            if snapshot::initial_value(metadata, p)?.is_some() {
                defined.insert(id);
            }
        }
        Ok(Self {
            class,
            values,
            defined,
        })
    }

    fn bytes(&self) -> usize {
        128 + self
            .values
            .values()
            .map(|value| value.capacity() + 96)
            .sum::<usize>()
    }

    fn set(&mut self, metadata: &Metadata, id: u32, data: &[u8]) -> Result<(), Error> {
        let mut allocated = self.bytes();
        self.set_bounded(metadata, id, data, &mut allocated, &mut || true)
    }

    fn set_bounded(
        &mut self,
        metadata: &Metadata,
        id: u32,
        data: &[u8],
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check(proceed)?;
        let property = metadata.property(id)?;
        let width = metadata.class(property.datatype_id)?.size;
        if width == 0
            || !data.len().is_multiple_of(width)
            || (property.kind != 2 && data.len() != width)
        {
            return Err(error(
                "middle trim property payload does not match datatype",
            ));
        }
        let value = self
            .values
            .get_mut(&id)
            .ok_or_else(|| error("middle trim property does not belong to instance"))?;
        reserve_raw(value, data.len(), allocated)?;
        value.clear();
        for chunk in data.chunks(64 << 10) {
            check(proceed)?;
            value.extend_from_slice(chunk);
        }
        self.defined.insert(id);
        Ok(())
    }

    fn body_size(&self, metadata: &Metadata) -> Result<usize, Error> {
        let class = metadata.class(self.class)?;
        let mut size = 4;
        metadata_budget(&mut size, class.size, 1)?;
        for id in properties(metadata, self.class)? {
            let p = metadata.property(id)?;
            if !self.defined.contains(&id) {
                return Err(error(format!(
                    "middle trim cannot preserve an unrecorded property: {}.{}",
                    metadata
                        .strings
                        .get(&class.name.name)
                        .map(String::as_str)
                        .unwrap_or("?"),
                    metadata
                        .strings
                        .get(&p.name)
                        .map(String::as_str)
                        .unwrap_or("?"),
                )));
            }
            if p.kind == 2 {
                metadata_budget(&mut size, self.values[&id].len() + 4, 1)?;
            }
        }
        Ok(size)
    }

    fn body(&self, metadata: &Metadata) -> Result<Vec<u8>, Error> {
        let size = self.body_size(metadata)?;
        let mut fixed = vec![0; metadata.class(self.class)?.size];
        let mut arrays = Vec::new();
        for id in properties(metadata, self.class)? {
            let p = metadata.property(id)?;
            let data = &self.values[&id];
            let width = if p.kind == 2 { 12 } else { data.len() };
            let end = p
                .offset32
                .checked_add(width)
                .ok_or_else(|| error("snapshot property offset overflow"))?;
            let target = fixed
                .get_mut(p.offset32..end)
                .ok_or_else(|| error("snapshot property exceeds fixed layout"))?;
            if p.kind == 2 {
                arrays.push(id);
            } else {
                target.copy_from_slice(data);
            }
        }
        let mut body = Vec::with_capacity(size);
        blob(&mut body, &fixed)?;
        for id in arrays {
            blob(&mut body, &self.values[&id])?;
        }
        Ok(body)
    }
}

fn reserve_raw(value: &mut Vec<u8>, len: usize, allocated: &mut usize) -> Result<(), Error> {
    if len > value.capacity() {
        let previous = value.capacity();
        let mut projected = *allocated;
        metadata_budget(&mut projected, len - previous, 1)?;
        value.try_reserve_exact(len - value.len())?;
        metadata_budget(allocated, value.capacity() - previous, 1)?;
    }
    Ok(())
}

fn resize_raw(
    value: &mut Vec<u8>,
    len: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    while value.len() < len {
        check(proceed)?;
        value.resize(len.min(value.len() + (64 << 10)), 0);
    }
    Ok(())
}

fn copy_raw(
    target: &mut [u8],
    source: &[u8],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    if target.len() != source.len() {
        return Err(error("raw property copy length mismatch"));
    }
    for (target, source) in target.chunks_mut(64 << 10).zip(source.chunks(64 << 10)) {
        check(proceed)?;
        target.copy_from_slice(source);
    }
    Ok(())
}

fn raw_apply(
    metadata: &Metadata,
    state: &mut RawState,
    event: &HistoryEvent<'_>,
    allocated: &mut usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    check(proceed)?;
    if matches!(event, HistoryEvent::OriginShift(_)) {
        return Err(error(
            "OriginShift raw replay requires a verified middle scope",
        ));
    }
    let id = match event {
        HistoryEvent::Create(id, _)
        | HistoryEvent::Destroy(id)
        | HistoryEvent::Set { id, .. }
        | HistoryEvent::Array { id, .. } => *id,
        _ => return Ok(()),
    };
    let before = state.get(&id).map_or(0, RawInstance::bytes);
    match event {
        HistoryEvent::Create(_, class) => {
            if state.contains_key(&id) {
                return Err(error("middle trim instance ID reuse"));
            }
            let instance = RawInstance::create_bounded(metadata, *class, *allocated, proceed)?;
            state.insert(id, instance);
        }
        HistoryEvent::Destroy(_) => {
            state
                .remove(&id)
                .ok_or_else(|| error("destroy targets missing instance"))?;
        }
        HistoryEvent::Set {
            kind,
            property,
            data,
            ..
        } => {
            let instance = state
                .get_mut(&id)
                .ok_or_else(|| error("property targets missing instance"))?;
            if *kind == 3 {
                let mut budget = *allocated;
                instance.set_bounded(metadata, *property, data, &mut budget, proceed)?;
            } else {
                let message = metadata
                    .messages
                    .get(*property as usize)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| error("undefined property message"))?;
                validate_message_instance(metadata, *property, instance.class, proceed)?;
                validate_message(metadata, *property, data, proceed)?;
                let mut budget = *allocated;
                for entry in &message.entries {
                    check(proceed)?;
                    let end = entry
                        .offset
                        .checked_add(entry.size)
                        .ok_or_else(|| error("message field offset overflow"))?;
                    let data = data
                        .get(entry.offset..end)
                        .ok_or_else(|| error("message field exceeds payload"))?;
                    instance.set_bounded(metadata, entry.property, data, &mut budget, proceed)?;
                }
            }
        }
        HistoryEvent::Array {
            kind,
            property,
            index,
            data,
            ..
        } => {
            let p = metadata.property(*property)?;
            let width = metadata.class(p.datatype_id)?.size;
            let value = state
                .get_mut(&id)
                .ok_or_else(|| error("array targets missing instance"))?;
            let bytes = value
                .values
                .get_mut(property)
                .ok_or_else(|| error("array does not belong to instance"))?;
            let offset = (*index as usize)
                .checked_mul(width)
                .ok_or_else(|| error("array index overflow"))?;
            match *kind {
                5 => {
                    let data = data.ok_or_else(|| error("missing array element"))?;
                    if data.len() != width || offset > bytes.len() {
                        return Err(error("invalid array insertion"));
                    }
                    let mut budget = *allocated;
                    let previous = bytes.len();
                    let len = previous
                        .checked_add(width)
                        .ok_or_else(|| error("array insertion length overflow"))?;
                    reserve_raw(bytes, len, &mut budget)?;
                    resize_raw(bytes, len, proceed)?;
                    let mut end = previous;
                    while end > offset {
                        check(proceed)?;
                        let begin = offset.max(end.saturating_sub(64 << 10));
                        bytes.copy_within(begin..end, begin + width);
                        end = begin;
                    }
                    copy_raw(&mut bytes[offset..offset + width], data, proceed)?;
                }
                6 | 7 => {
                    let end = offset
                        .checked_add(width)
                        .filter(|end| *end <= bytes.len())
                        .ok_or_else(|| error("array index exceeds property"))?;
                    if *kind == 6 {
                        let previous = bytes.len();
                        let mut begin = end;
                        while begin < previous {
                            check(proceed)?;
                            let stop = previous.min(begin + (64 << 10));
                            bytes.copy_within(begin..stop, begin - width);
                            begin = stop;
                        }
                        bytes.truncate(previous - width);
                    } else {
                        let data = data.ok_or_else(|| error("missing array element"))?;
                        if data.len() != width {
                            return Err(error("invalid array element width"));
                        }
                        copy_raw(&mut bytes[offset..end], data, proceed)?;
                    }
                }
                _ => return Err(error("unsupported array event")),
            }
            value.defined.insert(*property);
        }
        _ => unreachable!(),
    }
    *allocated = allocated.saturating_sub(before);
    metadata_budget(allocated, state.get(&id).map_or(0, RawInstance::bytes), 1)
}

fn raw_apply_with_origin(
    metadata: &Metadata,
    state: &mut RawState,
    event: &HistoryEvent<'_>,
    allocated: &mut usize,
    origin: &mut Option<origin_history::Scope>,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    if let HistoryEvent::OriginShift(event) = event {
        origin
            .as_mut()
            .ok_or_else(|| error("OriginShift raw replay requires a verified middle scope"))?
            .apply(metadata, state, event, *allocated, false, proceed)
    } else {
        raw_apply(metadata, state, event, allocated, proceed)
    }
}

fn rebase_undo(
    events: &mut [HistoryEvent<'_>],
    start: usize,
    leading: &[HistoryEvent<'_>],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    let mut starts = BTreeMap::new();
    let mut allocated = 0;
    for (ordinal, event) in leading.iter().enumerate() {
        check(proceed)?;
        if let HistoryEvent::Set {
            kind: 3,
            id,
            property,
            ..
        } = event
        {
            metadata_budget(&mut allocated, 1, 64)?;
            if starts.insert((*id, *property), ordinal).is_some() {
                return Err(error("duplicate bootstrap property"));
            }
        }
    }
    for (offset, event) in events.iter_mut().enumerate() {
        check(proceed)?;
        let current = start + offset;
        let new_current = offset + leading.len();
        let initial = match event {
            HistoryEvent::Set {
                kind: 3,
                id,
                property,
                ..
            } => starts.get(&(*id, *property)).copied().unwrap_or(0),
            _ => 0,
        };
        let undo = match event {
            HistoryEvent::Set { undo, .. } => undo,
            HistoryEvent::Array {
                kind: 6 | 7, undo, ..
            } => undo,
            _ => continue,
        };
        let mut previous = None;
        let mut crosses = undo.is_empty();
        let mut retained = Vec::new();
        for &distance in undo.iter() {
            check(proceed)?;
            let target = current
                .checked_sub(distance as usize)
                .filter(|target| *target < current)
                .ok_or_else(|| error("middle trim requires strictly preceding undo targets"))?;
            if previous.is_some_and(|last| target <= last) {
                return Err(error("middle trim requires ordered distinct undo targets"));
            }
            previous = Some(target);
            if target < start {
                crosses = true;
            } else {
                retained.push(distance);
            }
        }
        if crosses {
            retained.insert(0, u32_len(new_current - initial)?);
        }
        *undo = retained;
    }
    Ok(())
}

fn affected(metadata: &Metadata, event: &HistoryEvent<'_>) -> Result<(u32, BTreeSet<u32>), Error> {
    match event {
        HistoryEvent::Set {
            kind: 3,
            id,
            property,
            ..
        }
        | HistoryEvent::Array { id, property, .. } => Ok((*id, BTreeSet::from([*property]))),
        HistoryEvent::Set {
            kind: 4,
            id,
            property,
            ..
        } => Ok((
            *id,
            metadata
                .messages
                .get(*property as usize)
                .and_then(Option::as_ref)
                .ok_or_else(|| error("undefined property message"))?
                .entries
                .iter()
                .map(|entry| entry.property)
                .collect(),
        )),
        _ => Err(error("event has no property undo")),
    }
}

#[cfg(test)]
fn verify_undo(
    metadata: &Metadata,
    baseline: &RawState,
    events: &[HistoryEvent<'_>],
    saved: &[Saved<'_>],
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    verify_undo_with_origin(metadata, baseline, events, saved, &mut None, proceed)
}

fn verify_undo_with_origin(
    metadata: &Metadata,
    baseline: &RawState,
    events: &[HistoryEvent<'_>],
    saved: &[Saved<'_>],
    origin: &mut Option<origin_history::Scope>,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    check(proceed)?;
    let mut state = clone_raw_state(baseline, proceed)?;
    let mut allocated = state.values().map(RawInstance::bytes).sum();
    let mut array_budget = array_undo::Budget::default();
    for (current, event) in events.iter().enumerate() {
        check(proceed)?;
        match event {
            HistoryEvent::OriginShift(event) => {
                origin
                    .as_mut()
                    .ok_or_else(|| error("OriginShift undo requires a verified middle scope"))?
                    .apply(metadata, &mut state, event, allocated, true, proceed)?;
                continue;
            }
            HistoryEvent::Array { id, .. } | HistoryEvent::Set { kind: 3, id, .. }
                if matches!(event, HistoryEvent::Array { .. })
                    || matches!(event, HistoryEvent::Set { property, .. }
                        if metadata.property(*property)?.kind == 2) =>
            {
                let expected = state
                    .get(id)
                    .ok_or_else(|| error("array undo targets missing instance"))?;
                array_undo::verify(
                    metadata,
                    baseline,
                    events,
                    current,
                    expected,
                    &mut array_budget,
                    proceed,
                )?;
            }
            HistoryEvent::Set {
                kind,
                property,
                data,
                undo,
                ..
            } => {
                let (id, targets) = affected(metadata, event)?;
                let expected = state
                    .get(&id)
                    .ok_or_else(|| error("undo targets missing instance"))?;
                let mut restored = RawInstance {
                    class: expected.class,
                    values: targets.iter().map(|id| (*id, Vec::new())).collect(),
                    defined: BTreeSet::new(),
                };
                if *kind == 3 {
                    restored.set(metadata, *property, data)?;
                } else {
                    validate_message_instance(metadata, *property, expected.class, proceed)?;
                    validate_message(metadata, *property, data, proceed)?;
                    let message = metadata
                        .messages
                        .get(*property as usize)
                        .and_then(Option::as_ref)
                        .ok_or_else(|| error("undefined property message"))?;
                    for entry in &message.entries {
                        check(proceed)?;
                        restored.set(
                            metadata,
                            entry.property,
                            &data[entry.offset..entry.offset + entry.size],
                        )?;
                    }
                }
                let references = if undo.is_empty() {
                    vec![current as u32]
                } else {
                    undo.clone()
                };
                for distance in references {
                    check(proceed)?;
                    let target = current
                        .checked_sub(distance as usize)
                        .filter(|target| *target < current)
                        .ok_or_else(|| error("invalid rebased undo reference"))?;
                    match &events[target] {
                        HistoryEvent::Marker if target == 0 => {
                            let initial = baseline
                                .get(&id)
                                .filter(|instance| instance.class == expected.class)
                                .ok_or_else(|| error("undo baseline has no instance"))?;
                            for property in &targets {
                                let value = initial.values.get(property).ok_or_else(|| {
                                    error("undo baseline has no matching property")
                                })?;
                                restored.values.insert(*property, value.clone());
                            }
                        }
                        HistoryEvent::Create(previous_id, class)
                            if *previous_id == id && *class == expected.class =>
                        {
                            let initial = RawInstance::create(metadata, *class)?;
                            for property in &targets {
                                let value = initial.values.get(property).ok_or_else(|| {
                                    error("undo creation has no matching property")
                                })?;
                                restored.values.insert(*property, value.clone());
                            }
                        }
                        HistoryEvent::Set {
                            kind: 3,
                            id: previous_id,
                            property,
                            data,
                            ..
                        } if *previous_id == id && targets.contains(property) => {
                            restored.set(metadata, *property, data)?;
                        }
                        HistoryEvent::Set {
                            kind: 4,
                            id: previous_id,
                            property,
                            data,
                            ..
                        } if *previous_id == id => {
                            let message = metadata
                                .messages
                                .get(*property as usize)
                                .and_then(Option::as_ref)
                                .ok_or_else(|| error("undefined undo message"))?;
                            validate_message_instance(
                                metadata,
                                *property,
                                expected.class,
                                proceed,
                            )?;
                            validate_message(metadata, *property, data, proceed)?;
                            for entry in &message.entries {
                                check(proceed)?;
                                if targets.contains(&entry.property) {
                                    restored.set(
                                        metadata,
                                        entry.property,
                                        &data[entry.offset..entry.offset + entry.size],
                                    )?;
                                }
                            }
                        }
                        HistoryEvent::Array { .. } if *kind == 4 => {}
                        _ => {
                            return Err(error(
                                "middle trim undo reconstruction uses an unsupported event",
                            ));
                        }
                    }
                }
                if targets
                    .iter()
                    .any(|id| restored.values.get(id) != expected.values.get(id))
                {
                    return Err(error(
                        "rebased native undo disagrees with preceding raw state",
                    ));
                }
            }
            HistoryEvent::Destroy(id) => {
                let record = saved
                    .iter()
                    .find(|record| record.id == *id && record.deleted)
                    .ok_or_else(|| error("destroy has no retained saved instance"))?;
                let expected = state
                    .get(id)
                    .ok_or_else(|| error("destroy targets missing instance"))?;
                let restored = RawInstance::restore(metadata, record)?;
                if expected.class != restored.class || expected.values != restored.values {
                    return Err(error("destroyed record disagrees with native undo state"));
                }
            }
            _ => {}
        }
        raw_apply(metadata, &mut state, event, &mut allocated, proceed)?;
    }
    Ok(())
}

fn clone_raw_state(
    source: &RawState,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<RawState, Error> {
    let mut allocated = 0;
    for instance in source.values() {
        check(proceed)?;
        metadata_budget(&mut allocated, instance.bytes(), 1)?;
    }
    let mut state = RawState::new();
    for (&id, instance) in source {
        check(proceed)?;
        let mut values = BTreeMap::new();
        for (&property, value) in &instance.values {
            check(proceed)?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(value.len())?;
            if bytes.capacity() > value.capacity() {
                metadata_budget(&mut allocated, bytes.capacity() - value.capacity(), 1)?;
            }
            for chunk in value.chunks(64 << 10) {
                check(proceed)?;
                bytes.extend_from_slice(chunk);
            }
            values.insert(property, bytes);
        }
        let mut defined = BTreeSet::new();
        for &property in &instance.defined {
            check(proceed)?;
            defined.insert(property);
        }
        state.insert(
            id,
            RawInstance {
                class: instance.class,
                values,
                defined,
            },
        );
    }
    Ok(state)
}

impl Layout<'_> {
    pub(super) fn trim_middle(
        &mut self,
        first: usize,
        last: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<history::Proof, Error> {
        let cameras = camera::Scope::prepare(self, first, last, proceed)?;
        let history = self.trim_middle_with_cameras(first, last, cameras.as_ref(), proceed)?;
        if let Some(scope) = cameras {
            scope.verify_layout(self, history.leading_count(), proceed)?;
        }
        Ok(history)
    }

    pub(super) fn trim_middle_with_cameras(
        &mut self,
        first: usize,
        last: usize,
        cameras: Option<&camera::Scope>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<history::Proof, Error> {
        if first > last {
            return Err(error("first frame exceeds last frame"));
        }
        if !self.communication.cameras.is_empty()
            && cameras.is_none_or(|scope| !scope.authorizes(self, first, last))
        {
            return Err(error(
                "native camera middle history requires a verified scope",
            ));
        }
        let camera_bytes = cameras.map_or(0, camera::Scope::bytes);
        self.validate_sections()?;
        let interval = self.middle_frame_interval(first, last, proceed)?;
        let initial_source =
            initial_source::Scope::prepare_with_cameras(self, first, last, cameras, proceed)?;
        if self.snapshots.len() != 1
            || self.snapshots[0].event != 0
            || (initial_source.is_none() && self.snapshot_bytes[0].as_ref() != [0, 0, 0, 0])
        {
            return Err(error(
                "middle trim requires one empty event-zero source snapshot",
            ));
        }
        let end = interval.end;
        let mut origin = origin_history::Scope::prepare(self, end, proceed)?;
        self.verify_trim_events_mode(end, origin.is_some(), proceed)?;
        let mut history = match &initial_source {
            Some(scope) => history::prepare_with_initial(
                self,
                first,
                last,
                scope.classes(),
                scope
                    .index_bytes()
                    .checked_add(camera_bytes)
                    .ok_or_else(|| error("camera index budget overflow"))?,
                proceed,
            )?,
            None if cameras.is_some() => history::prepare_with_initial(
                self,
                first,
                last,
                &BTreeMap::new(),
                camera_bytes,
                proceed,
            )?,
            None => history::prepare(self, first, last, proceed)?,
        };
        let start = history.first_event;
        if let Some(scope) = &mut origin {
            history.origin = Some(scope.prove_range(
                self,
                origin_history::Range {
                    start,
                    end,
                    first_end: interval.first_end,
                    leading: history.bootstrap.len() + 1,
                },
                proceed,
            )?);
        }
        let mut allocated = camera_bytes;
        let mut baseline = match &initial_source {
            Some(scope) => scope.restore(self, &mut allocated, proceed)?,
            None => RawState::new(),
        };
        let mut source = read_source::Source::new(std::io::Cursor::new(self.backing))?;
        let pages = self
            .pages
            .iter()
            .map(|page| (page.first, page.position, page.count, page.len))
            .collect::<Vec<_>>();
        let saved = self
            .saved
            .iter()
            .filter(|record| record.deleted)
            .map(|record| {
                Ok((
                    record.position,
                    (record.id, u32_len(record.body.len() + 12)?),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, Error>>()?;
        let proven = snapshot::proven_initial_properties(
            &self.metadata,
            &mut source,
            0..self.backing.len() as u64,
            &pages,
            &saved,
            proceed,
        )?;
        for event in self
            .events
            .get(..start)
            .ok_or_else(|| error("first frame event out of range"))?
        {
            check(proceed)?;
            raw_apply_with_origin(
                &self.metadata,
                &mut baseline,
                event,
                &mut allocated,
                &mut origin,
                proceed,
            )?;
            if let HistoryEvent::Create(id, class) = event
                && let Some(fields) = proven.fields(*id, *class)
            {
                let instance = baseline.get_mut(id).unwrap();
                for (&property, data) in fields {
                    let before = instance.bytes();
                    metadata_budget(&mut allocated, data.len(), 1)?;
                    instance.set(&self.metadata, property, data)?;
                    allocated = allocated - data.len() - before + instance.bytes();
                }
            }
        }
        let mut initial = Vec::new();
        let mut snapshot_size = 4usize;
        let mut snapshot_allocation = camera_bytes;
        metadata_budget(&mut snapshot_allocation, 4, 1)?;
        metadata_budget(
            &mut snapshot_allocation,
            baseline.len(),
            std::mem::size_of::<Saved<'_>>(),
        )?;
        initial.try_reserve_exact(baseline.len())?;
        metadata_budget(
            &mut snapshot_allocation,
            initial.capacity() - baseline.len(),
            std::mem::size_of::<Saved<'_>>(),
        )?;
        for (&id, instance) in &baseline {
            check(proceed)?;
            metadata_budget(
                &mut snapshot_allocation,
                instance.body_size(&self.metadata)? + 24,
                1,
            )?;
            let body = match &initial_source {
                Some(scope) => scope.body(self, id, instance, proceed)?,
                None => instance.body(&self.metadata)?,
            };
            metadata_budget(&mut snapshot_allocation, body.capacity() - body.len(), 1)?;
            snapshot_size = output_size(snapshot_size, 12)?;
            initial.push(Saved {
                id,
                position: 0,
                class: instance.class,
                body: body.into(),
                deleted: false,
            });
        }
        if let Some(scope) = &initial_source {
            history.initial = Some(
                scope.prove(
                    self,
                    start..end,
                    &baseline,
                    &initial,
                    initial_source::ProofContext {
                        index_bytes: history
                            .initial_index_bytes()
                            .checked_add(camera_bytes)
                            .ok_or_else(|| error("camera proof budget overflow"))?,
                        bootstrap: &history.bootstrap,
                    },
                    proceed,
                )?,
            );
        }
        self.trim_prefix_with_analysis(
            last,
            initial_source.as_ref().and_then(|scope| scope.analysis()),
            cameras,
            proceed,
        )?;
        drop(initial_source);
        let mut events = self.events.split_off(start);
        let mut leading = vec![HistoryEvent::Marker];
        for seed in std::mem::take(&mut history.bootstrap) {
            let current = u32_len(leading.len())?;
            leading.push(HistoryEvent::Set {
                kind: 3,
                id: seed.id,
                property: seed.property,
                data: seed.bytes.into(),
                undo: vec![current],
            });
        }
        let leading_count = leading.len() as u64;
        rebase_undo(&mut events, start, &leading, proceed)?;
        leading.append(&mut events);
        let events = leading;
        let retained: BTreeSet<_> = baseline
            .keys()
            .copied()
            .chain(events.iter().filter_map(|event| {
                if let HistoryEvent::Create(id, _) = event {
                    Some(*id)
                } else {
                    None
                }
            }))
            .collect();
        self.events = events;
        self.verify_event_pointers(self.events.len(), &retained, proceed)?;
        for record in &initial {
            self.verify_saved_pointers(record, &retained, proceed)?;
        }
        let mut positions = BTreeSet::new();
        let mut destroyed = BTreeSet::new();
        for event in &self.events {
            match event {
                HistoryEvent::Saved { position, .. } => {
                    positions.insert(*position);
                }
                HistoryEvent::Destroy(id) => {
                    destroyed.insert(*id);
                }
                _ => {}
            }
        }
        self.saved.retain(|record| {
            positions.contains(&record.position)
                || (record.deleted && destroyed.contains(&record.id))
        });
        for record in &mut self.saved {
            record.deleted &= destroyed.contains(&record.id);
        }
        verify_undo_with_origin(
            &self.metadata,
            &baseline,
            &self.events,
            &self.saved,
            &mut origin,
            proceed,
        )?;
        let mut position = snapshot_size as u64;
        let mut directory = Vec::new();
        directory.try_reserve_exact(snapshot_size)?;
        w32(&mut directory, u32_len(initial.len())?);
        for record in &mut initial {
            record.position = position;
            w32(&mut directory, record.id);
            w64(&mut directory, position);
            position = output_size(position as usize, record.body.len() + 12)? as u64;
        }
        let mut relocated = BTreeMap::new();
        for record in &mut self.saved {
            relocated.insert(record.position, position);
            record.position = position;
            position = output_size(position as usize, record.body.len() + 12)? as u64;
        }
        initial.append(&mut self.saved);
        self.saved = initial;
        for event in &mut self.events {
            if let HistoryEvent::Saved { position, .. } = event {
                *position = *relocated
                    .get(position)
                    .ok_or_else(|| error("missing retained saved record"))?;
            }
        }
        self.snapshots[0].position = 0;
        self.snapshots[0].len = u32_len(directory.len())?;
        self.snapshot_bytes = vec![directory.into()];
        for section in &mut self.communication.sections {
            section.rows.retain(|row| row[1] >= start as u64);
            for row in &mut section.rows {
                check(proceed)?;
                row[1] = row[1] - start as u64 + leading_count;
                row[3] = row[3] - start as u64 + leading_count;
            }
        }
        self.communication
            .sections
            .retain(|section| !section.rows.is_empty());
        self.communication
            .instances
            .retain(|instance| retained.contains(&instance.id));
        self.communication
            .reverse
            .retain(|(_, id)| retained.contains(id));
        let mut camera_count = 0;
        for index in 0..self.communication.cameras.len() {
            check(proceed)?;
            let row = self.communication.cameras[index];
            if retained.contains(&row.1) {
                self.communication.cameras[camera_count] = row;
                camera_count += 1;
            }
        }
        self.communication.cameras.truncate(camera_count);
        self.pages.clear();
        for first in (0..self.events.len()).step_by(16_384) {
            check(proceed)?;
            let end = (first + 16_384).min(self.events.len());
            let bytes = encode_page(&self.events[first..end], proceed)?;
            position = output_size(0, next_page(position as usize)?)? as u64;
            self.pages.push(Page {
                first: first as u64,
                position,
                count: u32_len(end - first)?,
                len: u32_len(bytes.len())?,
            });
            position = output_size(position as usize, bytes.len())? as u64;
        }
        self.validate_sections()?;
        Ok(history)
    }
}

#[cfg(test)]
pub(super) mod tests;
