use std::{collections::HashMap, ops::Range, time::Instant};

use pvd_core::analysis::view::{AllocationView, MemoryAction, MemorySource};

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub rows: usize,
    pub addresses: usize,
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            rows: 16_384,
            addresses: 262_144,
            bytes: 32 * 1024 * 1024,
        }
    }
}

#[derive(Default)]
pub(super) struct Group {
    pub label: String,
    pub file: String,
    pub multiple_files: bool,
    pub allocated_bytes: u64,
    pub allocations: u64,
    pub freed_bytes: u64,
    pub frees: u64,
    pub surviving_bytes: u64,
    pub survivors: u64,
}

#[derive(Clone, Copy)]
struct Allocation {
    group: usize,
    size: u64,
}

struct Build {
    range: Range<usize>,
    next: usize,
    groups: Vec<Group>,
    labels: HashMap<String, usize>,
    live: HashMap<(u64, u64), Allocation>,
    peak_addresses: usize,
    bytes: usize,
    limits: Limits,
}

#[derive(Default)]
pub(super) struct Cache {
    key: Option<(u64, usize)>,
    build: Option<Build>,
    pub rows: Vec<Group>,
    pub error: Option<&'static str>,
    pub cancelled: bool,
}

impl Cache {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn cancel(&mut self) {
        self.build = None;
        self.cancelled = true;
    }

    pub fn progress(&self) -> Option<(usize, usize)> {
        self.build
            .as_ref()
            .map(|build| (build.next - build.range.start, build.range.len()))
    }

    pub fn ready(&self) -> Result<bool, &'static str> {
        if let Some(error) = self.error {
            Err(error)
        } else if self.cancelled {
            Err("Memory grouping was cancelled.")
        } else {
            Ok(self.key.is_some() && self.build.is_none())
        }
    }

    pub fn update(&mut self, source: &impl MemorySource, frame: usize) {
        let started = Instant::now();
        self.advance(source, frame, Limits::default(), 2048, || {
            started.elapsed().as_millis() < 2
        });
    }

    pub fn advance(
        &mut self,
        source: &impl MemorySource,
        frame: usize,
        limits: Limits,
        count: usize,
        mut keep_running: impl FnMut() -> bool,
    ) {
        let key = (source.revision(), frame);
        if self.key != Some(key) {
            self.reset();
            self.key = Some(key);
            let range = source.frame_events(frame);
            self.build = Some(Build {
                next: range.start,
                range,
                groups: Vec::new(),
                labels: HashMap::new(),
                live: HashMap::new(),
                peak_addresses: 0,
                bytes: 0,
                limits,
            });
        }
        let Some(build) = &mut self.build else {
            return;
        };
        let mut failure = None;
        for _ in 0..count {
            if build.next == build.range.end || !keep_running() {
                break;
            }
            let Some(event) = source.event_at(build.next) else {
                failure = Some("Memory event is unavailable.");
                break;
            };
            if event.received_frame != frame {
                failure = Some("Memory event frame does not match its index.");
                break;
            }
            let key = (event.buffer, event.address);
            if let Some(previous) = build.live.remove(&key) {
                let group = &mut build.groups[previous.group];
                group.surviving_bytes -= previous.size;
                group.survivors -= 1;
                if event.action == MemoryAction::Free {
                    group.freed_bytes = group.freed_bytes.saturating_add(previous.size);
                    group.frees = group.frees.saturating_add(1);
                }
            }
            if event.action != MemoryAction::Free {
                let Some(allocation) = event.allocation else {
                    failure = Some("Allocation details are unavailable.");
                    break;
                };
                if let Err(error) = build.allocate(key, allocation) {
                    failure = Some(error);
                    break;
                }
            }
            build.next += 1;
        }
        if let Some(error) = failure {
            self.build = None;
            self.error = Some(error);
        } else if build.next == build.range.end {
            let mut build = self.build.take().unwrap();
            build.groups.sort_unstable_by(|a, b| a.label.cmp(&b.label));
            self.rows = build.groups;
        }
    }
}

impl Build {
    fn allocate(
        &mut self,
        key: (u64, u64),
        allocation: AllocationView<'_>,
    ) -> Result<(), &'static str> {
        if self.live.len() >= self.limits.addresses {
            return Err("Too many allocations in this frame to group.");
        }
        let file = allocation.file.rsplit(['/', '\\']).next().unwrap_or("");
        let file = if file.is_empty() { "<unknown>" } else { file };
        let label_bytes = file
            .len()
            .saturating_add(allocation.type_name.len())
            .saturating_add(16);
        let reserve = label_bytes
            .saturating_mul(4)
            .saturating_add(allocation.file.len())
            .saturating_add(512);
        if self.bytes.saturating_add(reserve) > self.limits.bytes {
            return Err("Memory grouping exceeds its memory limit.");
        }
        let mut label = format!("{file}:{}", allocation.line);
        if !allocation.type_name.is_empty() {
            label.push_str(" (");
            label.push_str(allocation.type_name);
            label.push(')');
        }
        let index = if let Some(&index) = self.labels.get(&label) {
            self.groups[index].multiple_files |= self.groups[index].file != allocation.file;
            index
        } else {
            if self.groups.len() >= self.limits.rows {
                return Err("Too many source locations in this frame to group.");
            }
            let index = self.groups.len();
            self.bytes += reserve;
            self.labels.insert(label.clone(), index);
            self.groups.push(Group {
                label,
                file: allocation.file.to_owned(),
                ..Default::default()
            });
            index
        };
        // Hash tables retain buckets after removals, so account for their high-water mark.
        if self.live.len() == self.peak_addresses {
            self.peak_addresses += 1;
            self.bytes = self.bytes.saturating_add(96);
        }
        if self.bytes > self.limits.bytes {
            return Err("Memory grouping exceeds its memory limit.");
        }
        self.live.insert(
            key,
            Allocation {
                group: index,
                size: allocation.size,
            },
        );
        let group = &mut self.groups[index];
        group.allocated_bytes = group
            .allocated_bytes
            .checked_add(allocation.size)
            .ok_or("Grouped allocation sizes overflow.")?;
        group.allocations = group.allocations.saturating_add(1);
        group.surviving_bytes = group
            .surviving_bytes
            .checked_add(allocation.size)
            .ok_or("Grouped allocation sizes overflow.")?;
        group.survivors = group.survivors.saturating_add(1);
        Ok(())
    }
}
