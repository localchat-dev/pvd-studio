use super::*;

pub(crate) struct FullAnalysisWriter<I: PageIo> {
    store: I::Store,
    domain: Domain,
    limits: FullLimits,
    maps: ResidentMemos,
    rows_budget: Arc<identity_table::MetadataBudget>,
    memos: FullMemos<I::Store>,
    identity: Arc<()>,
    prepared: Weak<()>,
    reservation: Arc<I::Reservation>,
    reader: Option<super::read::ReadSession<I>>,
    workspace: root::WriteWorkspace,
    pub statistics: FullStatistics,
}
pub(crate) struct PreparedAnalysis<I: PageIo> {
    snapshot: Snapshot<I>,
    maps: ResidentMemos,
    memos: FullMemos<I::Store>,
    identity: Arc<()>,
    transaction: Arc<()>,
    _reservation: Arc<I::Reservation>,
    statistics: FullStatistics,
    source: Owned<Continuation>,
}
impl<I: PageIo> PreparedAnalysis<I> {
    pub(crate) fn handle(&self) -> I::Handle {
        self.snapshot.handle()
    }
    pub(crate) fn descriptor(&self, io: &I) -> Descriptor<I::Store> {
        self.snapshot.descriptor(io)
    }
}
impl<I: PageIo> FullAnalysisWriter<I> {
    pub(crate) fn new(io: &I, domain: Domain, limits: FullLimits) -> Result<Self, Error> {
        let tree_scratch_bound = crate::analysis::resident::write_maps::tree_scratch_bound::<I>()?;
        let sequence_scratch = 12_555 + 512 * (size_of::<ResidentValue>() + size_of::<RowRef>());
        if add(tree_scratch_bound, sequence_scratch)? > BUILD_TREE_BYTES {
            return Err(error(
                "analysis temporary types exceed the encoding scratch partition",
            ));
        }
        if limits.writer_memo_bytes > 64 << 20 {
            return Err(error("analysis writer memo exceeds its limit"));
        }
        let reservation =
            Arc::new(io.reserve_metadata(add(limits.writer_memo_bytes, BUILD_SCRATCH_BYTES)?)?);
        let workspace = root::WriteWorkspace::new(&domain)?;
        let memos = FullMemos::new(&domain, io.store(), &limits)?;
        let rows_budget = identity_table::MetadataBudget::new(
            limits.writer_memo_bytes - limits.writer_memo_bytes / 8,
        );
        Ok(Self {
            store: io.store(),
            domain,
            maps: ResidentMemos::new(limits.writer_memo_bytes / 2, &rows_budget),
            rows_budget,
            limits,
            memos,
            identity: Arc::new(()),
            prepared: Weak::new(),
            reservation,
            reader: None,
            workspace,
            statistics: FullStatistics::default(),
        })
    }
    pub(crate) fn with_reader(
        io: &I,
        domain: Domain,
        limits: FullLimits,
        reader: &FullAnalysisReader<I>,
    ) -> Result<Self, Error> {
        if !reader.session.matches(io, &domain) {
            return Err(error(
                "analysis reader registration belongs to another store or domain",
            ));
        }
        let mut writer = Self::new(io, domain, limits)?;
        writer.reader = Some(reader.session.clone());
        Ok(writer)
    }
    pub(crate) fn clear(&mut self) {
        self.memos.clear();
        self.maps = ResidentMemos::new(self.limits.writer_memo_bytes / 2, &self.rows_budget);
    }
    pub(crate) fn prepare(
        &mut self,
        analysis: &ResidentAnalysis,
        io: &mut I,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<PreparedAnalysis<I>, Error> {
        check_replay_cancelled(cancelled)?;
        if !io.same(&self.store) || !analysis.domain.same(&self.domain) {
            return Err(error("analysis writer belongs to another store or domain"));
        }
        if self.prepared.upgrade().is_some() {
            return Err(error("analysis writer has an unpublished transaction"));
        }
        let transaction = Arc::new(());
        io.begin(&transaction)?;
        self.prepared = Arc::downgrade(&transaction);
        let build = ResidentBuild {
            identified: true,
            registry: self.reader.as_ref().map(|reader| {
                reader as &dyn crate::analysis::resident::write_maps::ImmutableRegistry
            }),
            io,
            old: &self.maps,
            staged: ResidentMemos::new(self.limits.writer_memo_bytes / 2, &self.rows_budget),
            pins: BTreeMap::new(),
            statistics: Statistics::default(),
            cancelled,
        };
        let mut cx = WriteContext {
            maps: build,
            domain: &self.domain,
        };
        let mut staged = FullMemos::new(&self.domain, cx.store(), &self.limits)?;
        let previous = self
            .memos
            .roots
            .get(analysis.data.identity())
            .filter(|(alive, id)| alive.same(&analysis.data) && cx.usable(*id, false));
        let (id, cached) = if let Some((_, id)) = previous {
            (id, true)
        } else {
            (
                root::write(analysis, &self.memos, &mut staged, &mut cx, &self.workspace)?,
                false,
            )
        };
        cx.check()?;
        let handle = cx.maps.io.pin(id)?;
        let mut statistics = staged.statistics();
        statistics.cached_roots = usize::from(cached);
        statistics.map_nodes = cx.maps.statistics.visited_nodes;
        statistics.map_new_values = cx.maps.statistics.new_values;
        statistics.map_new_strings = cx.maps.statistics.new_strings;
        statistics.map_new_pages = cx.maps.statistics.new_tree_pages;
        statistics.map_reused_pages = cx.maps.statistics.reused_tree_pages;
        statistics.writer_pin_capacity_misses = cx.maps.statistics.writer_pin_capacity_misses;
        statistics.logical_bytes = cx.maps.statistics.logical_bytes;
        statistics.old_rows = self.maps.rows_statistics();
        statistics.staged_rows = cx.maps.staged.rows_statistics();
        let maps = cx.maps.staged;
        Ok(PreparedAnalysis {
            snapshot: Snapshot {
                root: handle,
                store: self.store.clone(),
            },
            maps,
            memos: staged,
            identity: self.identity.clone(),
            transaction,
            _reservation: self.reservation.clone(),
            statistics,
            source: analysis.data.clone(),
        })
    }
    pub(crate) fn publish(
        &mut self,
        prepared: PreparedAnalysis<I>,
        io: &I,
    ) -> Result<Snapshot<I>, Error> {
        self.publish_with_cancel(prepared, io, &mut || false)
    }
    pub(crate) fn publish_with_cancel(
        &mut self,
        prepared: PreparedAnalysis<I>,
        io: &I,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Snapshot<I>, Error> {
        check_replay_cancelled(cancelled)?;
        if !io.same(&self.store)
            || !io.same(&prepared.snapshot.store)
            || !io.committed(&prepared.snapshot.root)
            || !Arc::ptr_eq(&self.identity, &prepared.identity)
            || !self.prepared.ptr_eq(&Arc::downgrade(&prepared.transaction))
        {
            return Err(error(
                "analysis snapshot must commit in its originating transaction before publication",
            ));
        }
        if let Some(reader) = &self.reader {
            reader.register(
                Some(&prepared.source),
                io.id(&prepared.snapshot.root),
                &prepared.memos,
                &prepared.maps,
                io,
                cancelled,
            )?;
        }
        check_replay_cancelled(cancelled)?;
        self.maps.absorb(prepared.maps, cancelled)?;
        self.memos.absorb(prepared.memos, io)?;
        self.statistics = prepared.statistics;
        self.statistics.published_rows = self.maps.rows_statistics();
        self.statistics.cache_capacity_bytes = self.memos.capacity_bytes();
        Ok(prepared.snapshot)
    }
}
