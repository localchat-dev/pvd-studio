use super::*;

impl Layout<'_> {
    pub(super) fn prefix_cut(
        &self,
        last: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<usize, Error> {
        if self.communication.sections.is_empty()
            || self.communication.sections.iter().any(|section| {
                self.metadata.strings.get(&section.name).map(String::as_str) != Some("frame")
            })
        {
            return Err(error("prefix trim requires frame sections"));
        }
        let mut sources = BTreeSet::new();
        let mut endings = Vec::new();
        let mut budget = 0;
        for section in &self.communication.sections {
            check(proceed)?;
            if !sources.insert(section.source) {
                return Err(error("duplicate frame section source"));
            }
            metadata_budget(&mut budget, section.rows.len(), 8)?;
            endings.try_reserve_exact(section.rows.len())?;
            endings.extend(section.rows.iter().map(|row| row[3]));
        }
        endings.sort_unstable();
        let end = *endings
            .get(last)
            .ok_or_else(|| error("last frame out of range"))?;
        for section in &self.communication.sections {
            check(proceed)?;
            if section.rows.iter().any(|row| row[1] <= end && row[3] > end) {
                return Err(error("prefix ends inside another Scene frame"));
            }
        }
        usize::try_from(end)?
            .checked_add(1)
            .ok_or_else(|| error("section event overflow"))
    }

    pub(super) fn trim_prefix(
        &mut self,
        last: usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let cameras = camera::Scope::prepare(self, 0, last, proceed)?;
        self.trim_prefix_with_analysis(last, None, cameras.as_ref(), proceed)?;
        if let Some(scope) = cameras {
            scope.verify_layout(self, 0, proceed)?;
        }
        Ok(())
    }

    pub(super) fn trim_prefix_with_analysis(
        &mut self,
        last: usize,
        analysis: Option<&initial::analysis::Scope>,
        cameras: Option<&camera::Scope>,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        let held = cameras.map_or(0, camera::Scope::bytes);
        let camera_permit = cameras.map(|scope| scope.permit(self)).transpose()?;
        let initial = initial::State::read_with_held(self, held, proceed)?;
        let nonempty = !initial.entries.is_empty();
        self.validate_sections()?;
        let cut = self.prefix_cut(last, proceed)?;
        let owned_analysis = if analysis.is_none() {
            initial.prefix_analysis_with_cameras(self, cut, camera_permit.as_ref(), proceed)?
        } else {
            None
        };
        let analysis = analysis.or(owned_analysis.as_ref());
        let mut budget = initial::Budget::new(&initial)?;
        if let Some(scope) = cameras {
            budget.add(scope.bytes(), 1)?;
        }
        if let Some(scope) = analysis {
            budget.add(scope.bytes(), 1)?;
        }
        budget.add(initial.classes.len(), 64)?;
        let initial_ids = initial.classes.keys().copied().collect::<BTreeSet<_>>();
        initial.validate_with_analysis(self, cut, analysis, held, proceed)?;
        if !self.communication.cameras.is_empty()
            && (cameras.is_none_or(|scope| !scope.prefix_end(self, cut))
                || origin_prefix::contains_shift(self, cut, proceed)?)
        {
            return Err(error(
                "native camera history requires a verified scope without OriginShift",
            ));
        }
        self.verify_prefix_trim_events(cut, proceed)?;
        let mut camera_ids = BTreeSet::new();
        for &(_, id) in &self.communication.cameras {
            check(proceed)?;
            budget.add(1, 64)?;
            if !camera_ids.insert(id) {
                return Err(error("native camera history has duplicate instance IDs"));
            }
        }
        if self
            .gaps
            .iter()
            .any(|(_, bytes)| bytes.iter().any(|v| *v != 0))
        {
            return Err(error("unrecognized nonzero backing data"));
        }
        budget.add(self.communication.instances.len(), 64)?;
        let source_ids = self
            .communication
            .instances
            .iter()
            .map(|i| (i.id, i.source))
            .collect::<BTreeMap<_, _>>();
        let mut all_scenes = BTreeMap::new();
        let mut scene_lifetimes = BTreeMap::new();
        budget.add(initial_ids.len(), 64)?;
        let mut local_instances = initial_ids.clone();
        for (&id, &class) in &initial.classes {
            check(proceed)?;
            if self
                .metadata
                .strings
                .get(&self.metadata.class(class)?.name.name)
                .is_some_and(|name| name == "PxScene")
                && self
                    .metadata
                    .strings
                    .get(&self.metadata.class(class)?.name.namespace)
                    .map(String::as_str)
                    == Some("physx3")
            {
                let source = *source_ids
                    .get(&id)
                    .ok_or_else(|| error("initial Scene is missing its source identity"))?;
                budget.add(1, 128)?;
                if all_scenes.insert(source, id).is_some() {
                    return Err(error("multiple Scene lifecycles share a source address"));
                }
                scene_lifetimes.insert(id, (None, self.events.len()));
            }
        }
        for (ordinal, event) in self.events.iter().enumerate() {
            check(proceed)?;
            if let HistoryEvent::Create(id, class) = event {
                budget.add(1, 64)?;
                if !local_instances.insert(*id) {
                    return Err(error("unsupported container local ID reuse"));
                }
                if camera_ids.contains(id) {
                    let class = self.metadata.class(*class)?;
                    if self
                        .metadata
                        .strings
                        .get(&class.name.namespace)
                        .map(String::as_str)
                        != Some("")
                        || self
                            .metadata
                            .strings
                            .get(&class.name.name)
                            .map(String::as_str)
                            != Some("Camera")
                    {
                        return Err(error(
                            "native camera history references a non-Camera instance",
                        ));
                    }
                }
                if self
                    .metadata
                    .strings
                    .get(&self.metadata.class(*class)?.name.name)
                    .is_some_and(|name| name == "PxScene")
                    && (!nonempty
                        || self
                            .metadata
                            .strings
                            .get(&self.metadata.class(*class)?.name.namespace)
                            .map(String::as_str)
                            == Some("physx3"))
                {
                    let source = *source_ids
                        .get(id)
                        .ok_or_else(|| error("Scene is missing its source identity"))?;
                    budget.add(1, 128)?;
                    if all_scenes.insert(source, *id).is_some() {
                        return Err(error("multiple Scene lifecycles share a source address"));
                    }
                    scene_lifetimes.insert(*id, (Some(ordinal), self.events.len()));
                }
            } else if let HistoryEvent::Destroy(id) = event
                && let Some((_, end)) = scene_lifetimes.get_mut(id)
            {
                *end = (*end).min(ordinal);
            }
        }
        for id in camera_ids {
            check(proceed)?;
            if !local_instances.contains(&id) {
                return Err(error("native camera history references a missing instance"));
            }
        }
        if nonempty && all_scenes.len() != 1 && analysis.is_some() {
            return Err(error("initial analysis requires one Scene lifecycle"));
        }
        if self
            .communication
            .sections
            .iter()
            .any(|section| !all_scenes.contains_key(&section.source))
        {
            return Err(error("frame section does not identify a captured Scene"));
        }
        for section in &self.communication.sections {
            let (begin, end) = scene_lifetimes[&all_scenes[&section.source]];
            for row in &section.rows {
                check(proceed)?;
                if begin.is_some_and(|begin| row[1] <= begin as u64) || row[3] >= end as u64 {
                    return Err(error("frame section exceeds its Scene lifecycle"));
                }
            }
        }
        budget.add(initial_ids.len(), 64)?;
        budget.add(scene_lifetimes.len(), 64)?;
        let mut created = initial_ids.clone();
        let mut alive = initial_ids;
        let mut scene_ids = scene_lifetimes
            .keys()
            .filter(|id| initial.classes.contains_key(*id))
            .copied()
            .collect::<BTreeSet<_>>();
        let mut saved_refs = BTreeMap::new();
        let mut destroyed = BTreeSet::new();
        for event in &self.events[..cut] {
            check(proceed)?;
            match event {
                HistoryEvent::Create(id, class) => {
                    budget.add(1, 128)?;
                    if !created.insert(*id) || !alive.insert(*id) {
                        return Err(error("unsupported instance ID reuse"));
                    }
                    let class = self.metadata.class(*class)?;
                    if self
                        .metadata
                        .strings
                        .get(&class.name.name)
                        .map(String::as_str)
                        == Some("PxScene")
                        && (!nonempty
                            || self
                                .metadata
                                .strings
                                .get(&class.name.namespace)
                                .map(String::as_str)
                                == Some("physx3"))
                    {
                        scene_ids.insert(*id);
                    }
                }
                HistoryEvent::Destroy(id) => {
                    if !alive.remove(id) {
                        return Err(error("destroy targets missing instance"));
                    }
                    if nonempty && !saved_refs.contains_key(id) {
                        return Err(error(
                            "initial-prefix destroy requires a captured saved witness",
                        ));
                    }
                    budget.add(1, 64)?;
                    destroyed.insert(*id);
                }
                HistoryEvent::Saved { id, position } => {
                    budget.add(1, 64)?;
                    if !alive.contains(id) || saved_refs.insert(*id, *position).is_some() {
                        return Err(error("unsupported saved instance lifecycle"));
                    }
                }
                _ => {}
            }
        }
        for section in &self.communication.sections {
            if section.rows.iter().any(|row| row[3] < cut as u64)
                && !scene_ids.contains(&all_scenes[&section.source])
            {
                return Err(error("frame section precedes its Scene lifecycle"));
            }
        }
        self.verify_event_pointers(cut, &created, proceed)?;
        self.verify_saved_prefix(cut, &created, &saved_refs, proceed)?;
        self.events.truncate(cut);
        for section in &mut self.communication.sections {
            section.rows.retain(|row| row[3] < cut as u64);
        }
        self.communication
            .sections
            .retain(|section| !section.rows.is_empty());
        self.communication
            .instances
            .retain(|i| created.contains(&i.id));
        self.communication.zones.retain(|z| created.contains(&z.id));
        let mut retained_cameras = 0;
        for index in 0..self.communication.cameras.len() {
            check(proceed)?;
            let row = self.communication.cameras[index];
            if created.contains(&row.1) {
                self.communication.cameras[retained_cameras] = row;
                retained_cameras += 1;
            }
        }
        self.communication.cameras.truncate(retained_cameras);
        if self
            .communication
            .memory_instances
            .iter()
            .any(|id| *id != u32::MAX && !created.contains(id))
        {
            return Err(error("memory catalog precedes its instance"));
        }
        let mut reverse = BTreeMap::new();
        budget.add(alive.len(), 80)?;
        for id in alive {
            if let Some(&source) = source_ids.get(&id)
                && reverse.insert(source, id).is_some()
            {
                return Err(error("multiple live IDs share a source address"));
            }
        }
        self.communication.reverse = reverse.into_iter().collect();
        budget.add(initial.entries.len(), 64)?;
        let initial_positions = initial
            .entries
            .iter()
            .map(|entry| entry.position)
            .collect::<BTreeSet<_>>();
        self.saved.retain(|saved| {
            initial_positions.contains(&saved.position)
                || saved_refs.get(&saved.id) == Some(&saved.position)
        });
        if nonempty {
            for saved in &mut self.saved {
                saved.deleted &= destroyed.contains(&saved.id)
                    && saved_refs.get(&saved.id) == Some(&saved.position);
            }
        }
        self.saved.sort_by_key(|s| s.position);
        let directory_size = initial
            .entries
            .len()
            .checked_mul(12)
            .and_then(|len| len.checked_add(4))
            .ok_or_else(|| error("initial directory size overflow"))?;
        let mut position = directory_size as u64;
        budget.add(self.saved.len(), 64)?;
        budget.add(directory_size, 1)?;
        let mut relocated = BTreeMap::new();
        for saved in &mut self.saved {
            relocated.insert(saved.position, position);
            saved.position = position;
            position = output_size(position as usize, saved.body.len() + 12)? as u64;
        }
        for event in &mut self.events {
            if let HistoryEvent::Saved { position, .. } = event {
                *position = *relocated
                    .get(position)
                    .ok_or_else(|| error("missing retained saved instance"))?;
            }
        }
        let mut directory = Vec::new();
        w32(&mut directory, u32_len(initial.entries.len())?);
        for entry in &initial.entries {
            check(proceed)?;
            w32(&mut directory, entry.id);
            w64(
                &mut directory,
                *relocated
                    .get(&entry.position)
                    .ok_or_else(|| error("missing retained initial instance"))?,
            );
        }
        self.snapshots[0].position = 0;
        self.snapshots[0].len = u32_len(directory.len())?;
        self.snapshot_bytes = vec![directory.into()];
        self.pages.retain(|p| p.first < cut as u64);
        for page in &mut self.pages {
            page.count = page.count.min((cut - page.first as usize) as u32);
            position = output_size(0, next_page(position as usize)?)? as u64;
            page.position = position;
            page.len = u32_len(
                encode_page(
                    &self.events[page.first as usize..page.first as usize + page.count as usize],
                    proceed,
                )?
                .len(),
            )?;
            position = output_size(position as usize, page.len as usize)? as u64;
        }
        self.validate_sections()
    }
}

#[cfg(test)]
mod tests;
