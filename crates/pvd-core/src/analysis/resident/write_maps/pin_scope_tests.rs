use super::*;
use std::sync::{Mutex, atomic::AtomicBool};

struct Blob {
    id: u64,
    bytes: Vec<u8>,
    dependencies: Vec<Arc<Blob>>,
    ready: AtomicBool,
}
struct State {
    next: u64,
    blobs: BTreeMap<u64, Weak<Blob>>,
    plan: Vec<Arc<Blob>>,
    fail: bool,
}
struct Io(Arc<Mutex<State>>);
impl Io {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(State {
            next: 100_000,
            blobs: BTreeMap::new(),
            plan: Vec::new(),
            fail: false,
        })))
    }
    fn external(&self, id: u64) -> Arc<Blob> {
        let blob = Arc::new(Blob {
            id,
            bytes: vec![42],
            dependencies: Vec::new(),
            ready: AtomicBool::new(true),
        });
        self.0
            .lock()
            .unwrap()
            .blobs
            .insert(id, Arc::downgrade(&blob));
        blob
    }
}
impl PageIo for Io {
    type Handle = Arc<Blob>;
    type Store = Arc<Mutex<State>>;
    type Reservation = ();
    fn store(&self) -> Self::Store {
        self.0.clone()
    }
    fn same(&self, store: &Self::Store) -> bool {
        Arc::ptr_eq(&self.0, store)
    }
    fn pin(&self, id: u64) -> Result<Self::Handle, Error> {
        self.0
            .lock()
            .unwrap()
            .blobs
            .get(&id)
            .and_then(Weak::upgrade)
            .ok_or_else(|| error("expired test blob"))
    }
    fn id(&self, handle: &Self::Handle) -> u64 {
        handle.id
    }
    fn committed(&self, handle: &Self::Handle) -> bool {
        handle.ready.load(Ordering::Acquire)
    }
    fn begin(&mut self, _: &Arc<()>) -> Result<(), Error> {
        Ok(())
    }
    fn plan(&mut self, bytes: &[u8], dependencies: &[Self::Handle]) -> Result<Self::Handle, Error> {
        let mut state = self.0.lock().unwrap();
        if state.fail {
            return Err(error("injected dependency publication failure"));
        }
        let id = state.next;
        state.next += 1;
        let blob = Arc::new(Blob {
            id,
            bytes: bytes.to_vec(),
            dependencies: dependencies.to_vec(),
            ready: AtomicBool::new(false),
        });
        state.blobs.insert(id, Arc::downgrade(&blob));
        state.plan.push(blob.clone());
        Ok(blob)
    }
    fn commit(&mut self) -> Result<(), Error> {
        for blob in &self.0.lock().unwrap().plan {
            blob.ready.store(true, Ordering::Release);
        }
        Ok(())
    }
    fn rollback(&mut self) {
        self.0.lock().unwrap().plan.clear();
    }
    fn invalidate_reader(&mut self) {}
    fn info(&self, id: u64) -> Result<(usize, usize), Error> {
        let blob = self.pin(id)?;
        Ok((blob.bytes.len(), blob.dependencies.len()))
    }
    fn read(
        &self,
        id: u64,
        maximum: usize,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<LoadedPage, Error> {
        check_replay_cancelled(cancelled)?;
        let blob = self.pin(id)?;
        if blob.bytes.len() > maximum {
            return Err(error("test blob exceeds read limit"));
        }
        Ok(LoadedPage {
            bytes: blob.bytes.clone(),
            dependencies: dependency_ids(blob.dependencies.iter().map(|blob| blob.id)),
            _release_probe: None,
        })
    }
    fn reserve_metadata(&self, _: usize) -> Result<(), Error> {
        Ok(())
    }
}

#[test]
fn planned_dependencies_replace_only_their_temporary_pins_and_rollback_releases_them() {
    let mut io = Io::new();
    let sources: Vec<_> = (1..=3_001).map(|id| io.external(id)).collect();
    let retained = sources[0].clone();
    let budget = identity_table::MetadataBudget::new(0);
    let old = ResidentMemos::new(0, &budget);
    let mut cancelled = || false;
    let mut build = ResidentBuild {
        identified: true,
        registry: None,
        io: &mut io,
        old: &old,
        staged: ResidentMemos::new(0, &budget),
        pins: BTreeMap::new(),
        statistics: Statistics::default(),
        cancelled: &mut cancelled,
    };
    assert!(build.valid_ref(3_001, false));
    let mut roots = Vec::new();
    for first in (1..=3_000).step_by(300) {
        let ids: Vec<_> = (first..first + 300).collect();
        for &id in &ids {
            assert!(build.valid_ref(id, false));
        }
        let root = build.page(&[11], ids).unwrap();
        roots.push(root.id);
        assert_eq!(build.pins.len(), 1);
        assert!(build.pins.contains_key(&3_001));
    }
    assert_eq!(build.statistics.writer_pin_capacity_misses, 0);
    assert!(build.statistics.writer_pin_bytes_peak <= 301 * 512);
    drop(sources);
    for id in 1..=3_000 {
        assert!(build.io.pin(id).is_ok());
    }
    let root = build.page(&[12], roots).unwrap();
    assert!(!build.io.committed(&root));
    drop(root);
    drop(build);
    io.rollback();
    assert!(io.pin(1).is_ok());
    for id in 2..=3_001 {
        assert!(io.pin(id).is_err());
    }
    drop(retained);
    assert!(io.pin(1).is_err());
}

#[test]
fn failed_or_cancelled_plan_keeps_all_untransferred_pins() {
    let mut io = Io::new();
    let sources: Vec<_> = (1..=3).map(|id| io.external(id)).collect();
    let budget = identity_table::MetadataBudget::new(0);
    let old = ResidentMemos::new(0, &budget);
    let cancel = AtomicBool::new(false);
    let mut cancelled = || cancel.load(Ordering::Acquire);
    let mut build = ResidentBuild {
        identified: true,
        registry: None,
        io: &mut io,
        old: &old,
        staged: ResidentMemos::new(0, &budget),
        pins: BTreeMap::new(),
        statistics: Statistics::default(),
        cancelled: &mut cancelled,
    };
    for id in 1..=3 {
        assert!(build.valid_ref(id, false));
    }
    build.io.0.lock().unwrap().fail = true;
    assert!(build.page(&[1], [1, 2]).is_err());
    assert_eq!(build.pins.len(), 3);
    build.io.0.lock().unwrap().fail = false;
    cancel.store(true, Ordering::Release);
    assert!(build.page(&[1], [1, 2]).is_err());
    assert_eq!(build.pins.len(), 3);
    cancel.store(false, Ordering::Release);
    let transferred = build.page(&[2], [1, 2]).unwrap();
    assert_eq!(build.pins.len(), 1);
    assert!(build.pins.contains_key(&3));
    drop(sources);
    drop(transferred);
    assert!(build.io.pin(1).is_ok());
    assert!(build.io.pin(2).is_ok());
    assert!(build.io.pin(3).is_ok());
    let again = build.page(&[3], [1, 2]).unwrap();
    drop(again);
    drop(build);
    io.rollback();
    for id in 1..=3 {
        assert!(io.pin(id).is_err());
    }
}
