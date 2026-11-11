use super::*;

pub(super) struct Budget {
    remaining: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self { remaining: 1 << 30 }
    }
}

impl Budget {
    fn work(&mut self, bytes: usize) -> Result<(), Error> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| error("array undo verification exceeds its work budget"))?;
        Ok(())
    }

    fn parts(&mut self, count: usize) -> Result<(), Error> {
        self.work(
            count
                .checked_mul(std::mem::size_of::<&[u8]>())
                .ok_or_else(|| error("array undo fragment work overflow"))?,
        )
    }
}

struct View<'a> {
    parts: Vec<&'a [u8]>,
    len: usize,
}

impl<'a> View<'a> {
    fn new(value: &'a [u8]) -> Result<Self, Error> {
        if value.len() > MAX_METADATA {
            return Err(error("array undo value exceeds 64 MiB"));
        }
        Ok(Self {
            parts: if value.is_empty() {
                vec![]
            } else {
                vec![value]
            },
            len: value.len(),
        })
    }

    fn reserve(&mut self, count: usize) -> Result<(), Error> {
        let maximum = MAX_METADATA / std::mem::size_of::<&[u8]>();
        if count > maximum {
            return Err(error("array undo fragment index exceeds 64 MiB"));
        }
        if count > self.parts.capacity() {
            let capacity = count.next_power_of_two().min(maximum);
            self.parts.try_reserve_exact(capacity - self.parts.len())?;
        }
        Ok(())
    }

    fn reset(&mut self, value: &'a [u8], budget: &mut Budget) -> Result<(), Error> {
        if value.len() > MAX_METADATA {
            return Err(error("array undo value exceeds 64 MiB"));
        }
        budget.parts(self.parts.len() + 1)?;
        self.reserve(usize::from(!value.is_empty()))?;
        self.parts.clear();
        if !value.is_empty() {
            self.parts.push(value);
        }
        self.len = value.len();
        Ok(())
    }

    fn position(
        &self,
        mut offset: usize,
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(usize, usize), Error> {
        for (index, part) in self.parts.iter().enumerate() {
            if index.is_multiple_of(256) {
                check(proceed)?;
            }
            budget.parts(1)?;
            if offset < part.len() {
                return Ok((index, offset));
            }
            offset -= part.len();
        }
        if offset != 0 {
            return Err(error("array undo offset exceeds property"));
        }
        Ok((self.parts.len(), 0))
    }

    fn splice(
        &mut self,
        offset: usize,
        removed: usize,
        data: &'a [u8],
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check(proceed)?;
        let end = offset
            .checked_add(removed)
            .filter(|&end| end <= self.len)
            .ok_or_else(|| error("array undo range exceeds property"))?;
        let len = self
            .len
            .checked_sub(removed)
            .and_then(|len| len.checked_add(data.len()))
            .filter(|&len| len <= MAX_METADATA)
            .ok_or_else(|| error("array undo value exceeds 64 MiB"))?;
        let (first, left) = self.position(offset, budget, proceed)?;
        let (last, right) = self.position(end, budget, proceed)?;
        let prefix = if left == 0 {
            &[]
        } else {
            let part = self.parts[first];
            &part[..left]
        };
        let suffix = if right == 0 {
            &[]
        } else {
            let part = self.parts[last];
            &part[right..]
        };
        let end = last + usize::from(right != 0);
        let replacement = [prefix, data, suffix];
        let count = self.parts.len() - (end - first)
            + replacement.iter().filter(|part| !part.is_empty()).count();
        budget.parts(self.parts.len() + count)?;
        self.reserve(count)?;
        self.parts.splice(
            first..end,
            replacement.into_iter().filter(|part| !part.is_empty()),
        );
        self.len = len;
        Ok(())
    }

    fn insert(
        &mut self,
        index: u32,
        width: usize,
        data: &'a [u8],
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let index = if index > i32::MAX as u32 {
            self.len / width
        } else {
            (index as usize).min(self.len / width)
        };
        self.splice(index * width, 0, data, budget, proceed)
    }

    fn set(
        &mut self,
        index: u32,
        width: usize,
        data: &'a [u8],
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if index <= i32::MAX as u32 && (index as usize) < self.len / width {
            self.splice(index as usize * width, width, data, budget, proceed)?;
        }
        Ok(())
    }

    fn verify(
        &self,
        expected: &[u8],
        budget: &mut Budget,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check(proceed)?;
        if self.len != expected.len() {
            return Err(error(
                "rebased native undo disagrees with preceding raw state",
            ));
        }
        let mut offset = 0;
        for part in &self.parts {
            check(proceed)?;
            let other = &expected[offset..offset + part.len()];
            offset += part.len();
            if std::ptr::eq(part.as_ptr(), other.as_ptr()) {
                continue;
            }
            budget.work(part.len())?;
            for (left, right) in part.chunks(64 << 10).zip(other.chunks(64 << 10)) {
                check(proceed)?;
                if left != right {
                    return Err(error(
                        "rebased native undo disagrees with preceding raw state",
                    ));
                }
            }
        }
        Ok(())
    }
}

pub(super) fn verify(
    metadata: &Metadata,
    baseline: &RawState,
    events: &[HistoryEvent<'_>],
    current: usize,
    expected: &RawInstance,
    budget: &mut Budget,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<(), Error> {
    check(proceed)?;
    let event = events
        .get(current)
        .ok_or_else(|| error("array undo event exceeds history"))?;
    let (id, property, index, kind, data, undo) = match event {
        HistoryEvent::Array {
            id,
            property,
            index,
            kind,
            data,
            undo,
        } => (*id, *property, Some(*index), *kind, *data, undo.as_slice()),
        HistoryEvent::Set {
            kind: 3,
            id,
            property,
            data,
            undo,
        } => (
            *id,
            *property,
            None,
            3,
            Some(data.as_ref()),
            undo.as_slice(),
        ),
        _ => return Err(error("event has no supported array undo")),
    };
    let description = metadata.property(property)?;
    let width = metadata.class(description.datatype_id)?.size;
    if description.kind != 2 || width == 0 {
        return Err(error("array undo requires a fixed element layout"));
    }
    let before = expected
        .values
        .get(&property)
        .ok_or_else(|| error("array undo does not belong to instance"))?;
    if !before.len().is_multiple_of(width) {
        return Err(error("array undo property does not match datatype"));
    }
    let mut value = View::new(before)?;
    if let Some(index) = index {
        let offset = (index as usize)
            .checked_mul(width)
            .ok_or_else(|| error("array undo index overflow"))?;
        let valid = if kind == 5 {
            offset <= before.len()
        } else {
            offset < before.len()
        };
        if !valid || index > i32::MAX as u32 {
            return Err(error("array index exceeds property"));
        }
        let element = match (kind, data) {
            (5 | 7, Some(data)) if data.len() == width => data,
            (6, None) => &[],
            _ => return Err(error("invalid array element width")),
        };
        budget.work(match kind {
            5 => before.len() - offset,
            6 => before.len() - offset - width,
            _ => width,
        })?;
        value.splice(
            offset,
            if kind == 5 { 0 } else { width },
            element,
            budget,
            proceed,
        )?;
        if kind == 5 {
            value.splice(offset, width, &[], budget, proceed)?;
            return value.verify(before, budget, proceed);
        }
    } else {
        let data = data.ok_or_else(|| error("missing array property value"))?;
        if !data.len().is_multiple_of(width) {
            return Err(error("array property value does not match datatype"));
        }
        budget.work(data.len())?;
        value.reset(data, budget)?;
    }
    let fallback = [u32_len(current)?];
    let undo = if undo.is_empty() {
        fallback.as_slice()
    } else {
        undo
    };
    let mut previous = None;
    for &distance in undo {
        check(proceed)?;
        budget.parts(1)?;
        let target = current
            .checked_sub(distance as usize)
            .filter(|&target| target < current)
            .ok_or_else(|| error("invalid rebased undo reference"))?;
        if previous.is_some_and(|last| last >= target) {
            return Err(error("array undo requires ordered distinct references"));
        }
        previous = Some(target);
        match &events[target] {
            HistoryEvent::Marker if target == 0 => {
                let initial = baseline
                    .get(&id)
                    .filter(|instance| instance.class == expected.class)
                    .and_then(|instance| instance.values.get(&property))
                    .ok_or_else(|| error("array undo baseline has no matching property"))?;
                value.reset(initial, budget)?;
            }
            HistoryEvent::Create(previous_id, class)
                if *previous_id == id && *class == expected.class =>
            {
                value.reset(&[], budget)?;
            }
            HistoryEvent::Set {
                kind: 3,
                id: previous_id,
                property: previous_property,
                data,
                ..
            } if *previous_id == id && *previous_property == property => {
                if !data.len().is_multiple_of(width) {
                    return Err(error("array undo source does not match datatype"));
                }
                value.reset(data, budget)?;
            }
            HistoryEvent::Array {
                kind: previous_kind,
                id: previous_id,
                property: previous_property,
                index: previous_index,
                data,
                ..
            } if *previous_id == id && *previous_property == property => {
                let source_index = index.unwrap_or(*previous_index);
                match (*previous_kind, data) {
                    (5 | 7, Some(data)) if data.len() == width => {
                        if *previous_kind == 5 && kind != 7 {
                            value.insert(source_index, width, data, budget, proceed)?;
                        } else {
                            value.set(source_index, width, data, budget, proceed)?;
                        }
                    }
                    (6, None) => {}
                    _ => return Err(error("invalid array undo element source")),
                }
            }
            HistoryEvent::Set {
                kind: 4,
                id: previous_id,
                property: message,
                ..
            } if *previous_id == id => {
                let message = metadata
                    .messages
                    .get(*message as usize)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| error("undefined array undo property message"))?;
                for entry in &message.entries {
                    check(proceed)?;
                    budget.work(std::mem::size_of_val(entry))?;
                    if entry.property == property {
                        return Err(error(
                            "array undo property-message conversions are not verified",
                        ));
                    }
                }
            }
            _ => {
                return Err(error(
                    "array undo source does not match its instance and property",
                ));
            }
        }
    }
    value.verify(before, budget, proceed)
}

#[cfg(test)]
mod tests;
