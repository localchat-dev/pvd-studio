use super::*;

pub(super) struct WriteContext<'a, I: PageIo, C> {
    pub maps: ResidentBuild<'a, I, C>,
    pub domain: &'a Domain,
}
impl<I: PageIo, C: FnMut() -> bool> PageWrite for WriteContext<'_, I, C> {
    type Store = I::Store;
    fn store(&self) -> Self::Store {
        self.maps.io.store()
    }
    fn same(&self, store: &Self::Store) -> bool {
        self.maps.io.same(store)
    }
    fn domain(&self) -> &Domain {
        self.domain
    }
    fn check(&mut self) -> Result<(), Error> {
        check_replay_cancelled(self.maps.cancelled)
    }
    fn page(&mut self, bytes: &[u8], dependencies: &[u64]) -> Result<u64, Error> {
        self.check()?;
        if bytes.len() > MAX_PAGE_BYTES || dependencies.len() > 512 {
            return Err(error(
                "analysis page exceeds its bounded encoding workspace",
            ));
        }
        let page = self.maps.page(bytes, dependencies.iter().copied())?;
        Ok(self.maps.io.id(&page))
    }
    fn usable(&mut self, id: u64, staged: bool) -> bool {
        self.maps.valid_ref(id, staged)
    }
    fn values(&mut self, values: &[ResidentValue]) -> Result<Vec<RowRef>, Error> {
        self.check()?;
        if values.len() > 512
            || values
                .iter()
                .any(|value| !value.0.domain().same(self.domain))
        {
            return Err(error(
                "analysis values exceed workspace or belong to another domain",
            ));
        }
        let refs = self.maps.values(values)?;
        if refs.capacity() > 512 {
            return Err(error("analysis value references exceed bounded workspace"));
        }
        Ok(refs)
    }
}
impl<I: PageIo, C: FnMut() -> bool> WriteContext<'_, I, C> {
    pub(super) fn text(&mut self, source: &Text) -> Result<RowRef, Error> {
        self.check()?;
        if !source.0.domain().same(self.domain) {
            return Err(error("analysis text belongs to another domain"));
        }
        self.maps
            .strings(std::slice::from_ref(source))?
            .into_iter()
            .next()
            .ok_or_else(|| error("analysis text reference missing"))
    }
}

pub(super) struct ReadContext<'a, I: PageIo, C> {
    pub maps: &'a mut ResidentReader<I>,
    pub domain: &'a Domain,
    pub cancelled: &'a mut C,
}
impl<I: PageIo, C: FnMut() -> bool> PageRead for ReadContext<'_, I, C> {
    type Store = I::Store;
    fn store(&self) -> Self::Store {
        self.maps.raw.io.store()
    }
    fn same(&self, store: &Self::Store) -> bool {
        self.maps.raw.io.same(store)
    }
    fn domain(&self) -> &Domain {
        self.domain
    }
    fn check(&mut self) -> Result<(), Error> {
        check_replay_cancelled(self.cancelled)
    }
    fn load(&mut self, id: u64, maximum: usize) -> Result<ReadPage, Error> {
        self.check()?;
        let handle = self.maps.raw.io.pin(id)?;
        if !self.maps.raw.io.committed(&handle) {
            return Err(error("analysis page is not committed"));
        }
        let (loaded, memory) = self.maps.raw.loaded(id, maximum, self.cancelled)?;
        Ok(ReadPage::new(loaded, memory))
    }
    fn values(&mut self, references: &[RowRef]) -> Result<Owned<Vec<ResidentValue>>, Error> {
        self.check()?;
        if references.len() > 512 {
            return Err(error("analysis value reference limit exceeded"));
        }
        let payload = references
            .len()
            .checked_mul(size_of::<ResidentValue>())
            .ok_or_else(|| error("analysis values allocation overflow"))?;
        let memory = self
            .domain
            .0
            .reserve(add(Owned::<Vec<ResidentValue>>::fixed_bytes(), payload)?)?;
        let mut values = Vec::new();
        values.try_reserve_exact(references.len())?;
        if values.capacity() != references.len() {
            return Err(error("analysis values capacity exceeds reservation"));
        }
        for reference in references {
            self.check()?;
            let mut remaining = usize::MAX;
            values.push(
                self.maps
                    .value(*reference, &mut remaining, self.cancelled)?,
            );
        }
        Owned::from_reserved(values, memory, payload)
    }
}
impl<I: PageIo, C: FnMut() -> bool> ReadContext<'_, I, C> {
    pub(super) fn text(&mut self, reference: RowRef) -> Result<Text, Error> {
        let mut remaining = usize::MAX;
        self.maps.string(reference, &mut remaining, self.cancelled)
    }
    pub(super) fn map(&mut self, reference: MapRef) -> Result<ResidentMap, Error> {
        let mut remaining = reference.charge.try_into()?;
        let root = self.maps.tree(
            reference,
            None,
            None,
            &mut crate::analysis::resident::Traversal::new(reference.len)?,
            &mut remaining,
            self.cancelled,
        )?;
        if remaining != 0 {
            return Err(error("analysis allocation charge disagrees"));
        }
        Ok(ResidentMap {
            root,
            len: reference.len.try_into()?,
            domain: self.domain.clone(),
        })
    }
}
