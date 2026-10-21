use super::*;

#[derive(Clone)]
pub(crate) struct ResidentCursor {
    state: StateRef,
    position: usize,
    anchor: Option<EventRef>,
    base: StateRef,
}
impl ResidentCursor {
    pub(crate) fn new(memory: &MemoryData) -> Self {
        Self {
            state: memory.base.clone(),
            position: 0,
            anchor: None,
            base: memory.base.clone(),
        }
    }
    pub(crate) fn state(&self) -> &StateRef {
        &self.state
    }
    #[cfg(test)]
    pub(crate) fn position(&self) -> usize {
        self.position
    }
}

impl StateData {
    pub(super) fn apply(
        &mut self,
        event: &EventData,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let key = (event.buffer, event.address);
        match &event.kind {
            EventKind::Allocate {
                allocation,
                replaced,
            } => {
                if let Some(replaced) = replaced {
                    self.live_bytes = self
                        .live_bytes
                        .checked_sub(replaced.size)
                        .ok_or_else(|| error("memory state live byte underflow"))?;
                    self.replaced_addresses = self
                        .replaced_addresses
                        .checked_add(1)
                        .ok_or_else(|| error("memory state replacement overflow"))?;
                }
                self.live
                    .try_insert(key, allocation.clone(), &mut || cancelled())?;
                self.live_bytes = self
                    .live_bytes
                    .checked_add(allocation.size)
                    .ok_or_else(|| error("memory state live byte overflow"))?;
                self.total_allocated_bytes = self
                    .total_allocated_bytes
                    .checked_add(allocation.size)
                    .ok_or_else(|| error("memory state allocation byte overflow"))?;
                self.allocations = self
                    .allocations
                    .checked_add(1)
                    .ok_or_else(|| error("memory state allocation count overflow"))?;
            }
            EventKind::Free { allocation } => {
                if let Some(allocation) = allocation {
                    self.live.try_remove(&key, &mut || cancelled())?;
                    self.live_bytes = self
                        .live_bytes
                        .checked_sub(allocation.size)
                        .ok_or_else(|| error("memory state live byte underflow"))?;
                    self.total_freed_bytes = self
                        .total_freed_bytes
                        .checked_add(allocation.size)
                        .ok_or_else(|| error("memory state freed byte overflow"))?;
                } else {
                    self.unmatched_frees = self
                        .unmatched_frees
                        .checked_add(1)
                        .ok_or_else(|| error("memory state unmatched count overflow"))?;
                }
                self.frees = self
                    .frees
                    .checked_add(1)
                    .ok_or_else(|| error("memory state free count overflow"))?;
            }
        }
        Ok(())
    }
    fn revert(
        &mut self,
        event: &EventData,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let key = (event.buffer, event.address);
        match &event.kind {
            EventKind::Allocate {
                allocation,
                replaced,
            } => {
                self.live_bytes = self
                    .live_bytes
                    .checked_sub(allocation.size)
                    .ok_or_else(|| error("memory state reverse live byte underflow"))?;
                self.total_allocated_bytes = self
                    .total_allocated_bytes
                    .checked_sub(allocation.size)
                    .ok_or_else(|| error("memory state reverse allocation underflow"))?;
                self.allocations = self
                    .allocations
                    .checked_sub(1)
                    .ok_or_else(|| error("memory state reverse count underflow"))?;
                if let Some(replaced) = replaced {
                    self.live
                        .try_insert(key, replaced.clone(), &mut || cancelled())?;
                    self.live_bytes = self
                        .live_bytes
                        .checked_add(replaced.size)
                        .ok_or_else(|| error("memory state reverse byte overflow"))?;
                    self.replaced_addresses = self
                        .replaced_addresses
                        .checked_sub(1)
                        .ok_or_else(|| error("memory state reverse replacement underflow"))?;
                } else {
                    self.live.try_remove(&key, &mut || cancelled())?;
                }
            }
            EventKind::Free { allocation } => {
                if let Some(allocation) = allocation {
                    self.live
                        .try_insert(key, allocation.clone(), &mut || cancelled())?;
                    self.live_bytes = self
                        .live_bytes
                        .checked_add(allocation.size)
                        .ok_or_else(|| error("memory state reverse byte overflow"))?;
                    self.total_freed_bytes = self
                        .total_freed_bytes
                        .checked_sub(allocation.size)
                        .ok_or_else(|| error("memory state reverse free byte underflow"))?;
                } else {
                    self.unmatched_frees = self
                        .unmatched_frees
                        .checked_sub(1)
                        .ok_or_else(|| error("memory state reverse unmatched underflow"))?;
                }
                self.frees = self
                    .frees
                    .checked_sub(1)
                    .ok_or_else(|| error("memory state reverse free underflow"))?;
            }
        }
        Ok(())
    }
}

impl MemoryData {
    pub(crate) fn try_seek(
        &self,
        cursor: &mut ResidentCursor,
        received_frame: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let domain = self.base.0.domain();
        if !cursor.state.0.domain().same(&domain) || !cursor.base.0.domain().same(&domain) {
            return Err(error("memory cursor belongs to another domain"));
        }
        let received_frame = received_frame.max(self.first_retained_frame);
        let end = self
            .events
            .view()
            .partition_point(|event| event.received_frame <= received_frame);
        let same_anchor = match (&cursor.anchor, cursor.position.checked_sub(1)) {
            (None, None) => true,
            (Some(anchor), Some(index)) => self
                .events
                .view()
                .get(index)
                .is_some_and(|event| anchor.same(event)),
            _ => false,
        };
        let mut draft = if !cursor.base.0.same(&self.base.0)
            || !same_anchor
            || end < cursor.position.saturating_sub(end)
        {
            ResidentCursor::new(self)
        } else {
            cursor.clone()
        };
        let mut state = (*draft.state).clone();
        while draft.position > end {
            check_replay_cancelled(cancelled)?;
            draft.position -= 1;
            state.revert(
                self.events
                    .view()
                    .get(draft.position)
                    .ok_or_else(|| error("memory cursor event is unavailable"))?,
                cancelled,
            )?;
        }
        for event in self.events.view().range(draft.position..end)? {
            check_replay_cancelled(cancelled)?;
            state.apply(event, cancelled)?;
        }
        draft.state = StateRef(Owned::new(state, &domain, 0)?);
        draft.position = end;
        draft.anchor = end
            .checked_sub(1)
            .map(|index| self.events.view().get(index).unwrap().clone());
        check_replay_cancelled(cancelled)?;
        *cursor = draft;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn try_state_at_frame(
        &self,
        received_frame: usize,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<StateRef, Error> {
        let mut cursor = ResidentCursor::new(self);
        self.try_seek(&mut cursor, received_frame, cancelled)?;
        Ok(cursor.state)
    }
}
