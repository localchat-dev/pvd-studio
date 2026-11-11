use super::*;

#[derive(Debug)]
pub(super) struct Interval {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) first_end: usize,
    pub(super) previous_end: usize,
}

impl Layout<'_> {
    pub(super) fn middle_frame_interval(
        &self,
        first: usize,
        last: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Interval, Error> {
        if first == 0 || first > last {
            return Err(error(
                "middle trim requires a nonzero ordered frame interval",
            ));
        }
        let mut allocated = 0;
        let mut sources = BTreeSet::new();
        let mut count = 0usize;
        for section in &self.communication.sections {
            check(proceed)?;
            if self.metadata.strings.get(&section.name).map(String::as_str) != Some("frame") {
                return Err(error("middle trim requires frame sections"));
            }
            metadata_budget(&mut allocated, 1, 64)?;
            if !sources.insert(section.source) {
                return Err(error("duplicate frame section source"));
            }
            metadata_budget(&mut allocated, section.rows.len(), 16)?;
            count = count
                .checked_add(section.rows.len())
                .ok_or_else(|| error("frame count overflow"))?;
        }
        let mut frames = Vec::new();
        frames.try_reserve_exact(count)?;
        metadata_budget(&mut allocated, frames.capacity() - count, 16)?;
        for section in &self.communication.sections {
            for row in &section.rows {
                check(proceed)?;
                frames.push((row[1], row[3]));
            }
        }
        frames.sort_unstable_by_key(|row| row.1);
        let selected = frames
            .get(first..=last)
            .ok_or_else(|| error("middle frame interval out of range"))?;
        let start = selected
            .iter()
            .map(|row| row.0)
            .min()
            .ok_or_else(|| error("middle frame interval is empty"))?;
        let end = selected.last().unwrap().1;
        let previous_end = frames[first - 1].1;
        if previous_end >= start {
            return Err(error("middle trim starts inside an excluded Scene frame"));
        }
        for &(begin, finish) in &frames {
            check(proceed)?;
            if begin <= end && finish > end {
                return Err(error("middle trim ends inside another Scene frame"));
            }
        }
        Ok(Interval {
            start: usize::try_from(start)?,
            end: usize::try_from(end)?
                .checked_add(1)
                .ok_or_else(|| error("section event overflow"))?,
            first_end: usize::try_from(selected[0].1)?,
            previous_end: usize::try_from(previous_end)?,
        })
    }
}

#[cfg(test)]
mod tests;
