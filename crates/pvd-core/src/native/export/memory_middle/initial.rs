use super::*;

pub(super) struct Values<'a> {
    pub(super) fixed: [&'a [u8]; 2],
    pub(super) payloads: [&'a [u8]; 2],
}

pub(super) fn read<'a>(
    layout: &'a Layout<'_>,
    ids: [u32; 2],
    classes: [u32; 2],
    allocation: &mut usize,
    work: &mut Work,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<Option<Values<'a>>, Error> {
    check(proceed)?;
    work.charge(8)?;
    if layout.snapshots.len() != 1 || layout.snapshots[0].event != 0 {
        return Err(error(
            "middle memory requires one event-zero source snapshot",
        ));
    }
    let mut reader = Reader::new(
        layout
            .snapshot_bytes
            .first()
            .ok_or_else(|| error("initial middle memory has no snapshot directory"))?,
    );
    let count = reader.count()?;
    if count == 0 {
        reader.end()?;
        return Ok(None);
    }
    if !matches!(layout.events.first(), Some(HistoryEvent::Marker))
        || ids.contains(&u32::MAX)
        || ids[0] == ids[1]
    {
        return Err(error(
            "initial middle memory requires a captured pair and Marker zero",
        ));
    }
    metadata_budget(allocation, 1, std::mem::size_of::<Values<'_>>())?;
    let mut positions = [None; 2];
    for _ in 0..count {
        check(proceed)?;
        work.charge(12)?;
        let id = reader.u32()?;
        let position = reader.u64()?;
        if let Some(slot) = ids.iter().position(|candidate| *candidate == id)
            && positions[slot].replace(position).is_some()
        {
            return Err(error("duplicate initial middle memory identity"));
        }
    }
    reader.end()?;
    let positions = [
        positions[0].ok_or_else(|| error("initial middle memory buffer is absent"))?,
        positions[1].ok_or_else(|| error("initial middle memory totals are absent"))?,
    ];
    let mut captured = [None; 2];
    for saved in &layout.saved {
        check(proceed)?;
        work.charge(16)?;
        let Some(slot) = positions
            .iter()
            .position(|position| *position == saved.position)
        else {
            continue;
        };
        if saved.id != ids[slot]
            || saved.class != classes[slot]
            || saved.deleted
            || captured[slot].is_some()
        {
            return Err(error("initial middle memory has no exact live saved body"));
        }
        work.charge(saved.body.len())?;
        let mut body = Reader::new(&saved.body);
        let fixed = body.blob()?;
        if fixed.len() != 24 {
            return Err(error(
                "initial middle memory fixed body must contain 24 bytes",
            ));
        }
        let payload = body.blob()?;
        values(payload, slot, layout.communication.memory_names.len())?;
        body.end()?;
        captured[slot] = Some((fixed, payload));
    }
    let buffer = captured[0].ok_or_else(|| error("initial middle memory buffer body is absent"))?;
    let totals = captured[1].ok_or_else(|| error("initial middle memory totals body is absent"))?;
    Ok(Some(Values {
        fixed: [buffer.0, totals.0],
        payloads: [buffer.1, totals.1],
    }))
}

#[cfg(test)]
#[path = "initial/tests.rs"]
mod tests;
