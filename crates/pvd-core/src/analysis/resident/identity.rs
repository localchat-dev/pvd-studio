use super::*;
use std::{
    hash::{BuildHasher, Hasher},
    sync::{Mutex, OnceLock, atomic::AtomicU64},
};

const SLOT_BITS: u32 = 6;
const SEQUENCE_BITS: u32 = usize::BITS - SLOT_BITS;
const SEQUENCE_MASK: usize = (1usize << SEQUENCE_BITS) - 1;
const IMPORT_SLOTS: usize = (1 << SLOT_BITS) - 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WireIdentity {
    pub origin: [u8; 16],
    pub sequence: u64,
}
impl WireIdentity {
    pub fn validate(&self) -> Result<(), Error> {
        if self.origin == [0; 16] || self.sequence == 0 || self.sequence > SEQUENCE_MASK as u64 {
            return Err(error("resident identity is out of range"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Slot {
    origin: [u8; 16],
    references: usize,
}
pub(super) struct Identities {
    origin: [u8; 16],
    next: AtomicUsize,
    imports: Mutex<[Slot; IMPORT_SLOTS]>,
}
pub(super) struct Permit<'a> {
    manager: &'a Identities,
    id: usize,
    armed: bool,
}
impl Permit<'_> {
    pub fn take(mut self) -> usize {
        self.armed = false;
        self.id
    }
}
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.manager.release(self.id);
        }
    }
}
impl Identities {
    pub fn new() -> Self {
        static PROCESS: OnceLock<u64> = OnceLock::new();
        static DOMAINS: AtomicU64 = AtomicU64::new(1);
        let process = *PROCESS.get_or_init(|| {
            let mut hash = std::collections::hash_map::RandomState::new().build_hasher();
            hash.write_u32(std::process::id());
            hash.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            );
            hash.finish()
        });
        let domain = DOMAINS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .expect("resident domain identity space exhausted");
        let mut origin = [0; 16];
        origin[..8].copy_from_slice(&process.to_le_bytes());
        origin[8..].copy_from_slice(&domain.to_le_bytes());
        Self {
            origin,
            next: AtomicUsize::new(1),
            imports: Mutex::new(
                [Slot {
                    origin: [0; 16],
                    references: 0,
                }; IMPORT_SLOTS],
            ),
        }
    }
    pub fn local(&self) -> Result<Permit<'_>, Error> {
        let id = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(1).filter(|next| *next <= SEQUENCE_MASK)
            })
            .map_err(|_| error("resident local identity space exhausted"))?;
        Ok(Permit {
            manager: self,
            id,
            armed: true,
        })
    }
    pub fn import(&self, wire: WireIdentity) -> Result<Permit<'_>, Error> {
        wire.validate()?;
        if wire.origin == self.origin {
            if wire.sequence as usize >= self.next.load(Ordering::Acquire) {
                return Err(error(
                    "resident identity was not issued by its local domain",
                ));
            }
            return Ok(Permit {
                manager: self,
                id: wire.sequence as usize,
                armed: true,
            });
        }
        let mut slots = self
            .imports
            .lock()
            .map_err(|_| error("resident identity directory is poisoned"))?;
        let index = slots
            .iter()
            .position(|slot| slot.references != 0 && slot.origin == wire.origin)
            .or_else(|| slots.iter().position(|slot| slot.references == 0))
            .ok_or_else(|| error("resident imported identity origin limit exceeded"))?;
        let slot = &mut slots[index];
        slot.references = slot
            .references
            .checked_add(1)
            .ok_or_else(|| error("resident identity reference count overflow"))?;
        slot.origin = wire.origin;
        Ok(Permit {
            manager: self,
            id: ((index + 1) << SEQUENCE_BITS) | wire.sequence as usize,
            armed: true,
        })
    }
    pub fn wire(&self, id: usize) -> Result<WireIdentity, Error> {
        let index = id >> SEQUENCE_BITS;
        let origin = if index == 0 {
            self.origin
        } else {
            let slots = self
                .imports
                .lock()
                .map_err(|_| error("resident identity directory is poisoned"))?;
            let slot = &slots[index - 1];
            if slot.references == 0 {
                return Err(error("resident imported identity is no longer live"));
            }
            slot.origin
        };
        let wire = WireIdentity {
            origin,
            sequence: (id & SEQUENCE_MASK) as u64,
        };
        wire.validate()?;
        Ok(wire)
    }
    pub fn release(&self, id: usize) {
        let index = id >> SEQUENCE_BITS;
        if index == 0 {
            return;
        }
        // A poisoned directory is still available for destruction of issued permits.
        let mut slots = self.imports.lock().unwrap_or_else(|e| e.into_inner());
        let slot = &mut slots[index - 1];
        slot.references -= 1;
        if slot.references == 0 {
            slot.origin = [0; 16];
        }
    }
}
