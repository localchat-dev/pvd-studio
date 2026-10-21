use super::*;
mod file;
use file::{SegmentFile, segment_file};

#[derive(Default)]
pub(super) struct WriteState {
    pub index: StateIndex,
    pub object_cache: BTreeMap<u64, ObjectCache>,
    pub cache_bytes: usize,
    pub roots: BTreeMap<u64, ResourceHandle>,
    pub last_frame: Option<usize>,
    pub key_lists: object_keys::KeyLists,
}

pub(super) struct PreparedFrame {
    resources: ResourcePlan,
    handles: Vec<ResourceHandle>,
    update: IndexUpdate,
    caches: BTreeMap<u64, Option<ObjectCache>>,
    key_lists: object_keys::KeyLists,
    frame: Vec<u8>,
    undo: Option<Vec<u8>>,
    checkpoint: Option<u64>,
    analysis_base: Option<u64>,
    scene_generation: Option<u32>,
    analysis_publication: Option<analysis_pages::PreparedPage>,
}

impl PreparedFrame {
    pub fn take_analysis(&mut self) -> Option<analysis_pages::PreparedPage> {
        self.analysis_publication.take()
    }
    pub fn reservation(&self, resources: &Resources) -> u64 {
        resources
            .planned_bytes(&self.resources)
            .saturating_add(self.frame.len() as u64 + 36)
            .saturating_add(
                self.undo
                    .as_ref()
                    .map_or(0, |bytes| bytes.len() as u64 + 36),
            )
    }

    pub fn persist_resources(&self, resources: &Resources) -> Result<(), Error> {
        resources.commit(&self.resources)
    }
}

pub(super) struct SegmentWriter {
    pub view: SegmentView,
    pub file: BufWriter<SegmentFile>,
    pub failed_write: bool,
    pub state: WriteState,
    pub limit: u64,
    pub baseline_bytes: u64,
    pub resource_bytes: u64,
    pub record_buffer: Vec<u8>,
    pub metrics: Metrics,
}

impl SegmentWriter {
    #[cfg(all(test, feature = "resource-test-write-faults"))]
    pub(super) fn fail_segment_write_after(&mut self, bytes: usize) {
        self.file.get_mut().remaining = Some(bytes);
    }

    #[cfg(all(test, feature = "resource-test-write-faults"))]
    pub(super) fn fail_segment_flush(&mut self) {
        self.file.get_mut().fail_flush = true;
    }

    #[cfg(test)]
    pub fn new(owner: &Arc<CacheDirectory>, sequence: usize) -> Result<Self, Error> {
        Self::with_state(
            owner,
            sequence,
            Resources::new(owner.clone(), u64::MAX),
            None,
            false,
        )
    }

    pub fn with_state(
        owner: &Arc<CacheDirectory>,
        sequence: usize,
        resources: Resources,
        previous: Option<WriteState>,
        analysis_continuity: bool,
    ) -> Result<Self, Error> {
        let path = owner.path.join(format!("{sequence:08}.pvdhistory"));
        let mut file = BufWriter::with_capacity(
            256 << 10,
            segment_file(
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)?,
            ),
        );
        owner.files.lock().unwrap().insert(path.clone());
        file.write_all(MAGIC)?;
        let continued = previous.as_ref().and_then(|state| state.last_frame);
        let state = previous.unwrap_or_default();
        let lease = ResourceLease::new(state.roots.values().cloned().collect());
        Ok(Self {
            view: SegmentView {
                committed_end: 8,
                path,
                frames: Default::default(),
                checkpoints: Default::default(),
                analysis_base: 0,
                bytes: 8,
                resources,
                lease,
                readers: SegmentReaders::default(),
                previous_frame: continued,
                analysis_continuity: analysis_continuity && continued.is_some(),
            },
            file,
            failed_write: false,
            state,
            limit: u64::MAX,
            baseline_bytes: 0,
            resource_bytes: 0,
            record_buffer: Vec::new(),
            metrics: Metrics::default(),
        })
    }

    pub fn payload_bytes(&self) -> u64 {
        self.view.bytes + self.resource_bytes
    }

    fn encode(&mut self, record: &impl Serialize) -> Result<Vec<u8>, Error> {
        let mut bytes = std::mem::take(&mut self.record_buffer);
        bytes.clear();
        let _timer = self.metrics.measure(Stage::HistorySerialize);
        postcard::to_io(record, LimitedBuffer { bytes: &mut bytes }).map_err(|error| {
            super::error(format!(
                "history serialization failed within the 64 MiB record limit: {error}"
            ))
        })?;
        Ok(bytes)
    }

    fn recycle(&mut self, mut bytes: Vec<u8>) {
        if bytes.capacity() <= SERIALIZATION_CACHE_BYTES {
            bytes.clear();
            self.record_buffer = bytes;
        }
    }

    fn plan_record(
        &mut self,
        record: &impl Serialize,
        dependencies: &[ResourceHandle],
        plan: &mut PreparedFrame,
    ) -> Result<u64, Error> {
        let bytes = self.encode(record)?;
        let result = self
            .view
            .resources
            .plan(&bytes, dependencies, &mut plan.resources);
        self.recycle(bytes);
        let handle = result?;
        let id = handle.id();
        plan.handles.push(handle);
        Ok(id)
    }

    fn write_encoded(&mut self, bytes: &[u8]) -> Result<u64, Error> {
        if bytes.len() > MAX_RECORD
            || self.view.bytes.saturating_add(36 + bytes.len() as u64) > self.limit
        {
            return Err(error(
                "current scene cannot fit within the disk history budget",
            ));
        }
        let checksum = {
            let _timer = self.metrics.measure(Stage::HistoryChecksum);
            blake3::hash(bytes)
        };
        let offset = self.view.bytes;
        let _timer = self.metrics.measure(Stage::HistoryWrite);
        self.file.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.file.write_all(checksum.as_bytes())?;
        self.file.write_all(bytes)?;
        self.view.bytes += 36 + bytes.len() as u64;
        Ok(offset)
    }

    #[cfg(test)]
    pub fn write(&mut self, record: &impl Serialize) -> Result<u64, Error> {
        let bytes = self.encode(record)?;
        let result = self.write_encoded(&bytes);
        self.recycle(bytes);
        result
    }

    fn plan_object(
        &mut self,
        object: &Object,
        references: ReferenceEntries<'_>,
        plan: &mut PreparedFrame,
    ) -> Result<u64, Error> {
        let id = object.id;
        let mut cache = self
            .state
            .object_cache
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if cache.generation != object.generation {
            cache = ObjectCache {
                generation: object.generation,
                ..Default::default()
            };
        }
        let mut properties = BTreeMap::new();
        for (key, value) in &object.properties {
            if value.retained_bytes() >= LARGE_PROPERTY_BYTES {
                let offset = if let Some(previous) = cache
                    .properties
                    .get(key)
                    .filter(|previous| &previous.value == value)
                {
                    previous.offset
                } else {
                    let offset = self.plan_record(&RecordRef::Value(value), &[], plan)?;
                    cache.properties.insert(
                        key.clone(),
                        CachedValue {
                            value: value.clone(),
                            offset,
                        },
                    );
                    offset
                };
                properties.insert(key.as_str(), offset);
            }
        }
        cache
            .properties
            .retain(|key, _| properties.contains_key(key.as_str()));
        let mut bindings = BTreeMap::new();
        for (key, value) in references.clone() {
            if value.len().saturating_mul(80) >= LARGE_PROPERTY_BYTES {
                let offset = if let Some(previous) = cache
                    .references
                    .get(key)
                    .filter(|previous| previous.value.as_ref() == value)
                {
                    previous.offset
                } else {
                    let offset = self.plan_record(&RecordRef::References(value), &[], plan)?;
                    cache.references.insert(
                        key.clone(),
                        CachedReferences {
                            value: Arc::new(value.clone()),
                            offset,
                        },
                    );
                    offset
                };
                bindings.insert(key.as_str(), offset);
            }
        }
        cache
            .references
            .retain(|key, _| bindings.contains_key(key.as_str()));
        cache.bytes = 128
            + cache
                .properties
                .iter()
                .map(|(key, cached)| 128 + key.len() + cached.value.retained_bytes())
                .sum::<usize>()
            + cache
                .references
                .iter()
                .map(|(key, cached)| 128 + key.len() + cached.value.len() * 80)
                .sum::<usize>();
        let schema = self.state.key_lists.prepare(
            &mut plan.key_lists,
            &self.view.resources,
            &mut plan.resources,
            &object.properties,
            &properties,
        )?;
        let mut dependencies = properties
            .values()
            .chain(bindings.values())
            .map(|&id| self.view.resources.pin(id))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(schema) = schema {
            let keys = schema.id();
            dependencies.push(schema);
            let offset = self.plan_record(
                &RecordRef::KeyedObject(KeyedObjectRef {
                    id,
                    generation: object.generation,
                    type_name: &object.type_name,
                    keys,
                    values: SmallValues(SmallProperties {
                        values: &object.properties,
                        external: &properties,
                    }),
                    references: SmallReferences {
                        entries: references,
                        external: &bindings,
                    },
                    large_properties: &properties,
                    large_references: &bindings,
                }),
                &dependencies,
                plan,
            )?;
            plan.caches.insert(
                id,
                (!cache.properties.is_empty() || !cache.references.is_empty()).then_some(cache),
            );
            return Ok(offset);
        }
        let offset = self.plan_record(
            &RecordRef::Object(StoredObjectRef {
                object: ObjectRef {
                    id,
                    generation: object.generation,
                    type_name: &object.type_name,
                    properties: SmallProperties {
                        values: &object.properties,
                        external: &properties,
                    },
                },
                references: SmallReferences {
                    entries: references,
                    external: &bindings,
                },
                large_properties: &properties,
                large_references: &bindings,
            }),
            &dependencies,
            plan,
        )?;
        plan.caches.insert(
            id,
            (!cache.properties.is_empty() || !cache.references.is_empty()).then_some(cache),
        );
        Ok(offset)
    }

    pub fn prepare(
        &mut self,
        info: &FrameInfo,
        scene: &SceneState,
        patch: &SceneUpdate,
        analysis: &Analysis,
        inputs: &[AnalysisInput],
    ) -> Result<PreparedFrame, Error> {
        self.prepare_inner(info, scene, patch, Some(analysis), inputs, None)
    }

    pub fn prepare_paged(
        &mut self,
        info: &FrameInfo,
        scene: &SceneState,
        patch: &SceneUpdate,
        inputs: &[AnalysisInput],
        pages: &mut analysis_pages::HistoryPages,
    ) -> Result<PreparedFrame, Error> {
        self.prepare_inner(info, scene, patch, None, inputs, Some(pages))
    }

    fn prepare_inner(
        &mut self,
        info: &FrameInfo,
        scene: &SceneState,
        patch: &SceneUpdate,
        analysis: Option<&Analysis>,
        inputs: &[AnalysisInput],
        pages: Option<&mut analysis_pages::HistoryPages>,
    ) -> Result<PreparedFrame, Error> {
        let first = self.view.frames.is_empty();
        let initial = first && self.view.previous_frame.is_none();
        let mut plan = PreparedFrame {
            resources: ResourcePlan::default(),
            handles: Vec::new(),
            update: IndexUpdate::default(),
            caches: BTreeMap::new(),
            key_lists: self.state.key_lists.staged(),
            frame: Vec::new(),
            undo: None,
            checkpoint: None,
            analysis_base: None,
            analysis_publication: None,
            scene_generation: scene
                .objects
                .get(&info.scene_id)
                .filter(|object| object.type_name == "physx3::PxScene")
                .map(|object| object.generation),
        };
        if initial {
            for (&id, object) in &scene.objects {
                let references = ReferenceEntries::Scene {
                    owner: id,
                    entries: scene.reference_lifetimes.range((id, String::new())..),
                };
                let offset = self.plan_object(object, references, &mut plan)?;
                plan.update.objects.push((id, Some(offset)));
            }
            plan.update.generations = scene.generations.clone();
            for (&id, draw) in &scene.debug_draw {
                let offset = self.plan_record(&RecordRef::Debug(id, draw), &[], &mut plan)?;
                plan.update.debug.push((id, Some(offset)));
            }
            for (&id, contacts) in &scene.contacts {
                let offset =
                    self.plan_record(&RecordRef::Contacts(id, contacts), &[], &mut plan)?;
                plan.update.contacts.push((id, Some(offset)));
            }
            plan.update.errors = Some(scene.errors.clone());
            plan.update.cameras = scene
                .cameras
                .iter()
                .map(|(name, camera)| (name.clone(), Some(*camera)))
                .collect();
        } else {
            for (&id, object) in &patch.objects {
                let offset = if let Some(object) = object {
                    Some(self.plan_object(
                        &object.object,
                        ReferenceEntries::Object(object.references.iter()),
                        &mut plan,
                    )?)
                } else {
                    plan.caches.insert(id, None);
                    None
                };
                plan.update.objects.push((id, offset));
            }
            plan.update.generations = patch.generations.clone();
            for (&id, draw) in &patch.debug {
                let offset = draw
                    .as_ref()
                    .map(|draw| self.plan_record(&RecordRef::Debug(id, draw), &[], &mut plan))
                    .transpose()?;
                plan.update.debug.push((id, offset));
            }
            for (&id, contacts) in &patch.contacts {
                let offset = contacts
                    .as_ref()
                    .map(|contacts| {
                        self.plan_record(&RecordRef::Contacts(id, contacts), &[], &mut plan)
                    })
                    .transpose()?;
                plan.update.contacts.push((id, offset));
            }
            plan.update.errors = patch.errors.clone();
            plan.update.cameras = patch.cameras.clone();
        }
        if first {
            plan.analysis_base = Some(if let Some(pages) = pages {
                let prepared = {
                    let _progress = self.metrics.measure(Stage::HistoryAnalysisPrepare);
                    pages.prepare(&mut plan.resources)?
                };
                let offset = self.plan_record(
                    &RecordRef::AnalysisPages(prepared.id()),
                    &[prepared.handle()],
                    &mut plan,
                )?;
                plan.analysis_publication = Some(prepared);
                offset
            } else {
                self.plan_record(
                    &RecordRef::Analysis(
                        analysis.ok_or_else(|| error("legacy analysis baseline is unavailable"))?,
                    ),
                    &[],
                    &mut plan,
                )?
            });
        }
        if first || self.view.frames.len().is_multiple_of(120) {
            let _progress = self.metrics.measure(Stage::HistorySceneCheckpoint);
            let mut index = self.state.index.clone();
            plan.update.apply(&mut index);
            // The segment lease owns every version in this index; the blob owns only its blocks.
            plan.checkpoint =
                Some(self.plan_record(&RecordRef::Checkpoint(&index), &[], &mut plan)?);
        }
        plan.undo = undo::prepare(info, self.state.last_frame, &self.state.index, &plan.update)?;
        if plan.frame.is_empty() {
            plan.frame = self.encode(&RecordRef::Frame(StoredFrameRef {
                info,
                update: &plan.update,
                analysis: inputs,
            }))?;
        }
        Ok(plan)
    }

    pub fn commit(
        &mut self,
        plan: PreparedFrame,
        info: &FrameInfo,
        resource_bytes: u64,
    ) -> Result<(), Error> {
        let first = self.view.frames.is_empty();
        let pair_bytes = (plan.frame.len() as u64)
            .checked_add(36)
            .and_then(|bytes| {
                bytes.checked_add(plan.undo.as_ref().map_or(0, |undo| undo.len() as u64 + 36))
            })
            .and_then(|bytes| self.view.bytes.checked_add(bytes))
            .filter(|end| *end <= self.limit)
            .ok_or_else(|| error("scene frame and undo exceed the reserved segment limit"))?;
        let written = (|| -> Result<_, Error> {
            let offset = self.write_encoded(&plan.frame)?;
            let undo = plan
                .undo
                .as_ref()
                .map(|bytes| self.write_encoded(bytes))
                .transpose()?;
            let _timer = self.metrics.measure(Stage::HistoryFlush);
            self.file.flush()?;
            Ok((offset, undo.and_then(NonZeroU64::new)))
        })();
        let (offset, undo) = match written {
            Ok(value) => value,
            Err(failure) => {
                // An uncommitted buffered suffix must not be retried while closing the segment.
                self.failed_write = true;
                return Err(failure);
            }
        };
        debug_assert_eq!(self.view.bytes, pair_bytes);
        for (values, changes) in [
            (&self.state.index.objects, &plan.update.objects),
            (&self.state.index.debug, &plan.update.debug),
            (&self.state.index.contacts, &plan.update.contacts),
        ] {
            for &(id, next) in changes {
                let previous = values.get(&id);
                if let Some(previous) = previous {
                    self.state.roots.remove(previous);
                }
                if let Some(next) = next {
                    self.state
                        .roots
                        .insert(next, self.view.resources.pin(next)?);
                }
            }
        }
        plan.update.apply(&mut self.state.index);
        for (id, cache) in plan.caches {
            if let Some(previous) = self.state.object_cache.remove(&id) {
                self.state.cache_bytes -= previous.bytes;
            }
            if let Some(cache) = cache {
                self.state.cache_bytes += cache.bytes;
                self.state.object_cache.insert(id, cache);
            }
        }
        self.view.lease.extend(plan.handles)?;
        self.state
            .key_lists
            .publish(plan.key_lists, &self.view.resources)?;
        if let Some(base) = plan.analysis_base {
            self.view.analysis_base = base;
        }
        if let Some(checkpoint) = plan.checkpoint {
            Arc::make_mut(&mut self.view.checkpoints).insert(info.index, checkpoint);
        }
        self.view.frames.push(FrameLocation {
            undo,
            info: info.clone(),
            offset,
            scene_generation: plan.scene_generation,
        });
        self.view.committed_end = self.view.bytes;
        self.state.last_frame = Some(info.index);
        self.resource_bytes += resource_bytes;
        if first {
            self.baseline_bytes = self.payload_bytes();
        }
        Ok(())
    }
}

#[cfg(test)]
mod checkpoint_tests;
