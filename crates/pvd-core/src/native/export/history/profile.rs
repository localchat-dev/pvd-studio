use super::*;

type Key = (u64, u16, u32, u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Start {
    tick: u64,
    priority: u8,
    cpu: u8,
}

#[derive(Default)]
pub(in crate::native::export) struct Pending {
    stacks: BTreeMap<Key, Vec<Start>>,
    bytes: usize,
}

impl Pending {
    pub(in crate::native::export) fn ingest_bounded(
        &mut self,
        zone: u64,
        data: &[u8],
        shared_bytes: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut combined = self.bytes;
        metadata_budget(&mut combined, shared_bytes, 1)?;
        self.bytes = combined;
        let result = self.ingest(zone, data, proceed);
        self.bytes -= shared_bytes;
        result
    }

    pub(super) fn ingest(
        &mut self,
        zone: u64,
        data: &[u8],
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut r = Reader::new(data);
        let mut time = 0u64;
        let mut context = None;
        let uint = |r: &mut Reader<'_>, width: usize| -> Result<u64, Error> {
            let mut value = [0; 8];
            value[..width].copy_from_slice(r.take(width)?);
            Ok(u64::from_le_bytes(value))
        };
        while r.pos < data.len() {
            check(proceed)?;
            let kind = r.take(1)?[0];
            let options = r.take(1)?[0];
            let event = r.u16()?;
            let time_width = 1 << (options & 3);
            let context_width = 1 << ((options >> 2) & 3);
            match kind {
                1..=4 => {
                    if kind <= 2 {
                        context = Some((
                            r.u32()?,
                            uint(&mut r, context_width)?,
                            r.take(1)?[0],
                            r.take(1)?[0],
                        ));
                    }
                    let (thread, id, priority, cpu) = context.ok_or_else(|| {
                        error("profile bootstrap requires an explicit buffer context")
                    })?;
                    let encoded = uint(&mut r, time_width)?;
                    time = if time_width == 8 {
                        encoded
                    } else {
                        time.checked_add(encoded)
                            .ok_or_else(|| error("profile bootstrap timestamp overflow"))?
                    };
                    let key = (zone, event, thread, id);
                    if kind == 1 || kind == 3 {
                        if !self.stacks.contains_key(&key) {
                            metadata_budget(&mut self.bytes, 160, 1)?;
                        }
                        let values = self.stacks.entry(key).or_default();
                        if values.len() == values.capacity() {
                            let capacity = values
                                .capacity()
                                .max(1)
                                .checked_mul(2)
                                .ok_or_else(|| error("profile bootstrap capacity overflow"))?;
                            let additional = capacity - values.capacity();
                            metadata_budget(&mut self.bytes, additional, size_of::<Start>())?;
                            values.try_reserve_exact(capacity - values.len())?;
                            metadata_budget(
                                &mut self.bytes,
                                values.capacity() - capacity,
                                size_of::<Start>(),
                            )?;
                        }
                        values.push(Start {
                            tick: time,
                            priority,
                            cpu,
                        });
                    } else if let Some(values) = self.stacks.get_mut(&key) {
                        let start = values.pop().unwrap();
                        if time < start.tick {
                            return Err(error("profile bootstrap stop precedes start"));
                        }
                        if values.is_empty() {
                            self.bytes -= 160 + values.capacity() * size_of::<Start>();
                            self.stacks.remove(&key);
                        }
                    }
                }
                5 => {
                    r.take(time_width + context_width + 4)?;
                }
                6 => {
                    r.take(12)?;
                    let size = r.u32()? as usize;
                    r.take(size)?;
                    r.take(8)?;
                }
                _ => return Err(error("unsupported profile bootstrap event")),
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn seeds(
        &self,
        properties: &BTreeMap<u32, u32>,
        alive: &BTreeSet<u32>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<Seed>, Error> {
        self.seeds_bounded(properties, alive, &mut 0, proceed)
    }

    pub(in crate::native::export) fn seeds_bounded(
        &self,
        properties: &BTreeMap<u32, u32>,
        alive: &BTreeSet<u32>,
        allocated: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Vec<Seed>, Error> {
        let mut groups = BTreeMap::<u32, Vec<u8>>::new();
        metadata_budget(allocated, self.bytes, 1)?;
        for (&(zone, event, thread, context), starts) in &self.stacks {
            check(proceed)?;
            let zone = u32::try_from(zone)?;
            if !alive.contains(&zone) {
                return Err(error(
                    "profile bootstrap zone is no longer alive at the cut",
                ));
            }
            if !groups.contains_key(&zone) {
                metadata_budget(allocated, 160, 1)?;
            }
            let bytes = groups.entry(zone).or_default();
            for start in starts {
                check(proceed)?;
                let length = bytes
                    .len()
                    .checked_add(26)
                    .ok_or_else(|| error("profile seed size overflow"))?;
                if length > bytes.capacity() {
                    let capacity = length.max(
                        bytes
                            .capacity()
                            .checked_mul(2)
                            .ok_or_else(|| error("profile seed capacity overflow"))?,
                    );
                    metadata_budget(allocated, capacity - bytes.capacity(), 1)?;
                    bytes.try_reserve_exact(capacity - bytes.len())?;
                    metadata_budget(allocated, bytes.capacity() - capacity, 1)?;
                }
                bytes.extend([1, 15]);
                bytes.extend(event.to_le_bytes());
                bytes.extend(thread.to_le_bytes());
                bytes.extend(context.to_le_bytes());
                bytes.extend([start.priority, start.cpu]);
                bytes.extend(start.tick.to_le_bytes());
            }
        }
        metadata_budget(allocated, groups.len(), size_of::<Seed>())?;
        let mut result = Vec::new();
        result.try_reserve_exact(groups.len())?;
        metadata_budget(
            allocated,
            result.capacity() - groups.len(),
            size_of::<Seed>(),
        )?;
        for (id, bytes) in groups {
            check(proceed)?;
            result.push(Seed {
                id,
                property: *properties
                    .get(&id)
                    .ok_or_else(|| error("profile bootstrap property is missing"))?,
                bytes,
            });
        }
        Ok(result)
    }

    pub(super) fn verify(
        &self,
        analysis: &Analysis,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.verify_pending(analysis, proceed)?;
        if !analysis.spans.is_empty()
            || !analysis.values.is_empty()
            || analysis.unmatched_stops != 0
        {
            return Err(error("profile bootstrap introduced completed events"));
        }
        Ok(())
    }

    pub(super) fn verify_pending(
        &self,
        analysis: &Analysis,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let mut actual = analysis.open_event_starts();
        for (key, starts) in &self.stacks {
            check(proceed)?;
            let Some((actual_key, ticks)) = actual.next() else {
                return Err(error("profile bootstrap lost pending starts"));
            };
            if key != actual_key
                || !starts
                    .iter()
                    .map(|start| start.tick)
                    .eq(ticks.iter().copied())
            {
                return Err(error("profile bootstrap changed pending starts"));
            }
        }
        if actual.next().is_some() {
            return Err(error("profile bootstrap introduced pending starts"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u8, tick: u64) -> Vec<u8> {
        let mut bytes = vec![kind, 15];
        bytes.extend(7u16.to_le_bytes());
        bytes.extend(99u32.to_le_bytes());
        bytes.extend(0x1234567812345678u64.to_le_bytes());
        bytes.extend([13, 21]);
        bytes.extend(tick.to_le_bytes());
        bytes
    }

    #[test]
    fn pending_and_seed_capacity_share_the_resident_index_budget() {
        let bytes = event(1, 100);
        let mut probe = Pending::default();
        probe.ingest_bounded(4, &bytes, 0, &mut || true).unwrap();
        let required = probe.bytes;
        let mut exact = Pending::default();
        exact
            .ingest_bounded(4, &bytes, MAX_METADATA - required, &mut || true)
            .unwrap();
        assert_eq!(exact.bytes, required);
        assert!(
            Pending::default()
                .ingest_bounded(4, &bytes, MAX_METADATA - required + 1, &mut || true)
                .is_err()
        );
        let properties = BTreeMap::from([(4, 9)]);
        let alive = BTreeSet::from([4]);
        let mut seed_bytes = 0;
        probe
            .seeds_bounded(&properties, &alive, &mut seed_bytes, &mut || true)
            .unwrap();
        let mut exact = MAX_METADATA - seed_bytes;
        probe
            .seeds_bounded(&properties, &alive, &mut exact, &mut || true)
            .unwrap();
        assert_eq!(exact, MAX_METADATA);
        assert!(
            probe
                .seeds_bounded(
                    &properties,
                    &alive,
                    &mut (MAX_METADATA - seed_bytes + 1),
                    &mut || true
                )
                .is_err()
        );
    }

    #[test]
    fn relative_starts_keep_original_context_priority_cpu_and_full_ticks() {
        let tick = (1u64 << 62) + 17;
        let mut bytes = event(1, tick);
        bytes.extend([3, 0, 7, 0, 9]);
        let mut pending = Pending::default();
        pending.ingest(4, &bytes, &mut || true).unwrap();
        let seeds = pending
            .seeds(&BTreeMap::from([(4, 9)]), &BTreeSet::from([4]), &mut || {
                true
            })
            .unwrap();
        assert_eq!(seeds.len(), 1);
        assert_eq!(
            seeds[0].bytes,
            [event(1, tick), event(1, tick + 9)].concat()
        );
        let mut decoded = Analysis::default();
        decoded
            .ingest(4, "PxProfileZone", &seeds[0].bytes, Endian::Little, 0)
            .unwrap();
        pending.verify(&decoded, &mut || true).unwrap();
        pending
            .ingest(4, &event(2, tick + 30), &mut || true)
            .unwrap();
        let seeds = pending
            .seeds(&BTreeMap::from([(4, 9)]), &BTreeSet::from([4]), &mut || {
                true
            })
            .unwrap();
        assert_eq!(seeds[0].bytes, event(1, tick));
    }

    #[test]
    fn pending_starts_reject_unknown_context_missing_zone_budget_and_cancel() {
        let mut pending = Pending::default();
        assert!(
            pending
                .ingest(4, &[3, 0, 7, 0, 9], &mut || true)
                .unwrap_err()
                .to_string()
                .contains("explicit buffer context")
        );
        let error = pending
            .ingest(4, &event(1, 100), &mut || false)
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::Interrupted
        );
        assert!(pending.stacks.is_empty());
        pending.bytes = MAX_METADATA;
        assert!(pending.ingest(4, &event(1, 100), &mut || true).is_err());
        assert!(pending.stacks.is_empty());
        let mut pending = Pending::default();
        pending.ingest(4, &event(1, 100), &mut || true).unwrap();
        assert!(
            pending
                .seeds(&BTreeMap::from([(4, 9)]), &BTreeSet::new(), &mut || true)
                .unwrap_err()
                .to_string()
                .contains("no longer alive")
        );
    }

    #[test]
    fn legacy_cuda_payload_and_tail_do_not_become_profile_events() {
        let mut bytes = vec![6, 0, 0, 0];
        bytes.extend([0; 12]);
        bytes.extend(3u32.to_le_bytes());
        bytes.extend([0xff, 0xfe, 0xfd]);
        bytes.extend([0xff; 8]);
        bytes.extend(event(1, 100));
        let mut pending = Pending::default();
        pending.ingest(4, &bytes, &mut || true).unwrap();
        let mut source = Analysis::default();
        source
            .ingest(4, "PxProfileZone", &bytes, Endian::Little, 0)
            .unwrap();
        assert_eq!(source.legacy_cuda_buffers, 1);
        pending.verify_pending(&source, &mut || true).unwrap();
        let seeds = pending
            .seeds(&BTreeMap::from([(4, 9)]), &BTreeSet::from([4]), &mut || {
                true
            })
            .unwrap();
        assert_eq!(seeds[0].bytes, event(1, 100));
    }
}
