use super::*;

#[derive(Clone, Debug)]
pub(in crate::live) struct BlobDescription {
    pub id: u64,
    pub len: usize,
    pub hash: [u8; 32],
    pub blocks: Vec<u64>,
    pub dependencies: Vec<u64>,
}

#[derive(Clone, Debug)]
pub(in crate::live) struct BlockDescription {
    pub id: u64,
    pub len: usize,
    pub hash: [u8; 32],
    pub offset: u64,
}

#[derive(Debug)]
pub(in crate::live) struct CaptureBacking {
    pub(in crate::live) format: crate::live::capture::Format,
    pub file: File,
    pub path: PathBuf,
    pub bytes: u64,
}

impl CaptureBacking {
    pub(in crate::live) fn read_exact_at(
        &self,
        mut offset: u64,
        mut bytes: &mut [u8],
    ) -> Result<(), Error> {
        if offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.bytes)
        {
            return Err(error("capture read lies outside its committed file"));
        }
        while !bytes.is_empty() {
            #[cfg(windows)]
            let count = {
                use std::os::windows::fs::FileExt;
                self.file.seek_read(bytes, offset)?
            };
            #[cfg(unix)]
            let count = {
                use std::os::unix::fs::FileExt;
                self.file.read_at(bytes, offset)?
            };
            #[cfg(not(any(windows, unix)))]
            let count = return Err(error(
                "capture random reads are unsupported on this platform",
            ));
            if count == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            offset += count as u64;
            bytes = &mut bytes[count..];
        }
        Ok(())
    }
}

impl Resources {
    pub(in crate::live) fn describe_blob(
        &self,
        id: u64,
        maximum: usize,
    ) -> Result<BlobDescription, Error> {
        let handle = self.pin(id)?;
        if !handle.info().2 {
            return Err(error("capture cannot export an unpublished resource"));
        }
        if handle
            .0
            .blocks
            .len()
            .checked_add(handle.0.dependencies.len())
            .and_then(|n| n.checked_mul(16))
            .and_then(|n| n.checked_add(256))
            .is_none_or(|bytes| bytes > maximum)
        {
            return Err(error(
                "capture resource description exceeds its metadata budget",
            ));
        }
        Ok(BlobDescription {
            id,
            len: handle.0.len,
            hash: handle.0.hash,
            blocks: handle.0.blocks.iter().map(|block| block.id).collect(),
            dependencies: handle.dependency_ids(),
        })
    }

    pub(in crate::live) fn export_block(
        &self,
        id: u64,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(BlockDescription, Vec<u8>), Error> {
        check_replay_cancelled(cancelled)?;
        let block = self
            .shared
            .state
            .lock()
            .map_err(|_| lock_error())?
            .blocks
            .get(&id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| error("capture resource block expired while exporting"))?;
        let bytes = self.shared.block_bytes(&block, cancelled)?;
        Ok((
            BlockDescription {
                id,
                len: block.len,
                hash: block.hash,
                offset: 0,
            },
            bytes,
        ))
    }

    pub(in crate::live) fn capture_backing(&self) -> Option<&Arc<CaptureBacking>> {
        self.shared.capture.as_ref()
    }

    pub(in crate::live) fn from_capture(
        backing: Arc<CaptureBacking>,
        blocks: Vec<BlockDescription>,
        blobs: Vec<BlobDescription>,
        metadata_limit: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Self, Error> {
        let resources = Self {
            shared: Arc::new(Shared {
                owner: None,
                capture: Some(backing.clone()),
                retained: Mutex::new(Vec::new()),
                target: 0,
                read_buffer_bytes: READ_BUFFER_BYTES,
                state: Mutex::new(Store {
                    metadata_limit,
                    ..Default::default()
                }),
                blobs: Mutex::new(BTreeMap::new()),
                reader: Mutex::new(None),
            }),
        };
        let chunk = Arc::new(Chunk {
            id: 0,
            path: backing.path.clone(),
            bytes: AtomicU64::new(backing.bytes),
            _owner: None,
        });
        let mut block_map = BTreeMap::new();
        let mut blob_map: BTreeMap<u64, ResourceHandle> = BTreeMap::new();
        let mut metadata = 0usize;
        let mut charge = |bytes: usize| -> Result<(), Error> {
            metadata = metadata
                .checked_add(bytes)
                .filter(|bytes| *bytes <= metadata_limit)
                .ok_or_else(|| error("capture resource directory exceeds its metadata budget"))?;
            Ok(())
        };
        charge(size_of::<Shared>() + size_of::<Store>() + READ_BUFFER_BYTES + MAP_ENTRY_BYTES)?;
        for block in blocks {
            check_replay_cancelled(cancelled)?;
            charge(2 * MAP_ENTRY_BYTES + size_of::<Block>() + 64)?;
            if block.id == 0
                || block.id >= RESOURCE_ID
                || block.len > MAX_BLOCK
                || block_map.contains_key(&block.id)
            {
                return Err(error("invalid capture content block directory"));
            }
            block_map.insert(
                block.id,
                Arc::new(Block {
                    id: block.id,
                    len: block.len,
                    hash: block.hash,
                    location: Mutex::new(BlockLocation::Stored(Placement {
                        chunk: chunk.clone(),
                        offset: block.offset,
                    })),
                }),
            );
        }
        for blob in blobs {
            check_replay_cancelled(cancelled)?;
            let refs = blob
                .blocks
                .len()
                .checked_mul(size_of::<Arc<Block>>())
                .and_then(|n| {
                    blob.dependencies
                        .len()
                        .checked_mul(size_of::<ResourceHandle>())
                        .and_then(|m| n.checked_add(m))
                })
                .and_then(|n| n.checked_add(2 * MAP_ENTRY_BYTES + size_of::<Blob>() + 64))
                .ok_or_else(|| error("capture resource metadata overflow"))?;
            charge(refs)?;
            if blob.id < RESOURCE_ID || blob.len > MAX_RECORD || blob_map.contains_key(&blob.id) {
                return Err(error("invalid capture resource directory"));
            }
            let contents = blob
                .blocks
                .iter()
                .map(|id| {
                    block_map
                        .get(id)
                        .cloned()
                        .ok_or_else(|| error("capture resource references a missing block"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if contents
                .iter()
                .try_fold(0usize, |n, block| n.checked_add(block.len))
                != Some(blob.len)
            {
                return Err(error("capture resource block lengths disagree"));
            }
            let dependencies = blob
                .dependencies
                .iter()
                .map(|id| {
                    if *id >= blob.id {
                        return Err(error("capture dependency does not precede its owner"));
                    }
                    blob_map
                        .get(id)
                        .cloned()
                        .ok_or_else(|| error("capture resource dependency is missing"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            charge(
                (contents.capacity() - blob.blocks.len()) * size_of::<Arc<Block>>()
                    + (dependencies.capacity() - blob.dependencies.len())
                        * size_of::<ResourceHandle>(),
            )?;
            let depth = dependencies
                .iter()
                .map(|h| h.0.depth)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .filter(|depth| *depth <= MAX_DEPENDENCY_DEPTH)
                .ok_or_else(|| error("capture dependency graph exceeds its depth limit"))?;
            let handle = ResourceHandle(Arc::new(Blob {
                id: blob.id,
                len: blob.len,
                hash: blob.hash,
                blocks: contents,
                dependencies,
                depth,
                ready: AtomicBool::new(true),
                store: Arc::downgrade(&resources.shared),
            }));
            blob_map.insert(blob.id, handle);
        }
        let mut retained = Vec::new();
        charge(
            blob_map
                .len()
                .checked_mul(size_of::<ResourceHandle>())
                .ok_or_else(|| error("capture retained metadata overflow"))?,
        )?;
        retained.try_reserve_exact(blob_map.len())?;
        charge((retained.capacity() - blob_map.len()) * size_of::<ResourceHandle>())?;
        let mut state = resources.shared.state.lock().map_err(|_| lock_error())?;
        state.metadata = metadata;
        state.blocks = block_map
            .iter()
            .map(|(&id, block)| (id, Arc::downgrade(block)))
            .collect();
        *resources.shared.blobs.lock().map_err(|_| lock_error())? = blob_map
            .iter()
            .map(|(&id, blob)| (id, Arc::downgrade(&blob.0)))
            .collect();
        state.chunks.insert(0, chunk);
        for handle in blob_map.into_values() {
            check_replay_cancelled(cancelled)?;
            retained.push(handle);
        }
        *resources.shared.retained.lock().map_err(|_| lock_error())? = retained;
        drop(state);
        Ok(resources)
    }
}
