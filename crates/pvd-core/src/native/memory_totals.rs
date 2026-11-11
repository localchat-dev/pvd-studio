use super::*;
use crate::SceneState;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MemoryHistoryScope {
    #[default]
    CapturedEvents,
    AfterInitialSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeMemoryTotal<'a> {
    pub index: u32,
    pub type_name: &'a str,
    pub file: &'a str,
    pub line: u32,
    pub bytes: i32,
}

#[derive(Clone, Debug)]
pub struct NativeMemoryTotals {
    catalog: Arc<Catalog>,
    values: Arc<Vec<Value>>,
    generations: [Option<u32>; 2],
    nonnegative_bytes: Option<u64>,
}

impl NativeMemoryTotals {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn row(&self, index: usize) -> Option<NativeMemoryTotal<'_>> {
        let Value::I64(bytes) = self.values.get(index)? else {
            return None;
        };
        let site = self.catalog.sites.get(*self.catalog.indices.get(index)?)?;
        Some(NativeMemoryTotal {
            index: index as u32,
            type_name: &site.type_name,
            file: &site.file,
            line: site.line,
            bytes: i32::try_from(*bytes).ok()?,
        })
    }

    pub fn nonnegative_bytes(&self) -> Option<u64> {
        self.nonnegative_bytes
    }

    pub fn matches_state(&self, recording: &Recording, state: &SceneState) -> bool {
        let Some(memory) = &recording.native_memory else {
            return false;
        };
        if memory.problem.is_some()
            || !Arc::ptr_eq(&self.catalog, &memory.catalog)
            || self.generations != memory.generations
            || !memory.matches_instances(state)
        {
            return false;
        }
        matches!(
            state.objects[&u64::from(self.catalog.ids[1])].properties.get("totals"),
            Some(Value::Array(values)) if Arc::ptr_eq(&self.values, values)
        )
    }
}

impl Recording {
    pub fn memory_history_scope(&self) -> MemoryHistoryScope {
        self.native_memory
            .as_ref()
            .map_or(MemoryHistoryScope::CapturedEvents, |memory| memory.scope)
    }

    pub fn native_initial_memory_totals(&self) -> Result<Option<NativeMemoryTotals>, Error> {
        self.native_memory
            .as_ref()
            .map_or(Ok(None), |memory| memory.saved(&memory.initial))
    }

    pub fn native_end_memory_totals(&self) -> Result<Option<NativeMemoryTotals>, Error> {
        self.native_memory
            .as_ref()
            .map_or(Ok(None), |memory| memory.saved(&memory.end))
    }

    pub fn native_memory_totals_at(
        &self,
        state: &SceneState,
    ) -> Result<Option<NativeMemoryTotals>, Error> {
        self.native_memory_totals_at_with_cancel(state, || false)
    }

    pub fn native_memory_totals_at_with_cancel(
        &self,
        state: &SceneState,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Option<NativeMemoryTotals>, Error> {
        crate::check_replay_cancelled(&mut cancelled)?;
        let Some(memory) = &self.native_memory else {
            return Ok(None);
        };
        match memory.current(state, &mut || !cancelled())? {
            Current::Missing => Ok(None),
            Current::Invalid(problem) => Err(problem.error()),
            Current::Present { values, sum } => Ok(Some(NativeMemoryTotals {
                catalog: memory.catalog.clone(),
                values: values.clone(),
                generations: memory.generations,
                nonnegative_bytes: sum,
            })),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Problem {
    MissingPair,
    InvalidCatalog,
    UnsupportedSchema,
    AmbiguousInstances,
    MissingBuffer,
    InvalidValues,
}

impl Problem {
    fn error(self) -> Error {
        error(match self {
            Self::MissingPair => "native memory totals require a captured buffer/totals pair",
            Self::InvalidCatalog => "native memory source indices are not a unique dense directory",
            Self::UnsupportedSchema => "native memory totals use an unsupported class layout",
            Self::AmbiguousInstances => "native memory totals have ambiguous instance lifetimes",
            Self::MissingBuffer => "native memory totals have no matching live buffer instance",
            Self::InvalidValues => "native memory totals do not match their signed source array",
        })
    }
}

#[derive(Debug)]
struct Site {
    type_name: String,
    file: String,
    line: u32,
}

#[derive(Debug)]
pub(super) struct Catalog {
    ids: [u32; 2],
    sites: Vec<Site>,
    indices: Vec<usize>,
    problem: Option<Problem>,
    bytes: usize,
}

impl Catalog {
    pub(super) fn read(
        reader: &mut Reader<'_>,
        sources: &BTreeMap<u64, u64>,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        check(proceed)?;
        let ids = [reader.u32()?, reader.u32()?];
        let count = reader.count()?;
        let mut bytes = 0;
        charge(
            allocation,
            &mut bytes,
            1,
            size_of::<CapturedMemory>() + size_of::<Catalog>() + 32,
        )?;
        charge(
            allocation,
            &mut bytes,
            count,
            size_of::<Site>() + size_of::<usize>(),
        )?;
        let mut sites = Vec::new();
        sites.try_reserve_exact(count)?;
        let mut indices = Vec::new();
        indices.try_reserve_exact(count)?;
        indices.resize(count, usize::MAX);
        let mut problem = None;
        for position in 0..count {
            check(proceed)?;
            let type_name = reader.string()?;
            let file = reader.string()?;
            let line = reader.u32()?;
            let index = reader.u32()? as usize;
            charge(allocation, &mut bytes, type_name.len(), 1)?;
            charge(allocation, &mut bytes, file.len(), 1)?;
            sites.push(Site {
                type_name: type_name.to_owned(),
                file: file.to_owned(),
                line,
            });
            match indices.get_mut(index) {
                Some(slot) if *slot == usize::MAX => *slot = position,
                _ => problem = Some(Problem::InvalidCatalog),
            }
        }
        if ids == [u32::MAX; 2] {
            if count != 0 {
                problem = Some(Problem::MissingPair);
            }
        } else if ids[0] == ids[1]
            || ids.contains(&u32::MAX)
            // Totals 是原版创建的本地实例，不要求具有 SDK 源指针。
            || !sources.contains_key(&u64::from(ids[0]))
        {
            problem = Some(Problem::MissingPair);
        }
        Ok(Self {
            ids,
            sites,
            indices,
            problem,
            bytes,
        })
    }

    fn has_pair(&self) -> bool {
        self.ids != [u32::MAX; 2]
    }
}

fn charge(
    allocation: &mut usize,
    retained: &mut usize,
    count: usize,
    width: usize,
) -> Result<(), Error> {
    metadata_budget(allocation, count, width)?;
    *retained = retained
        .checked_add(
            count
                .checked_mul(width)
                .ok_or_else(|| error("native memory metadata size overflow"))?,
        )
        .ok_or_else(|| error("native memory metadata size overflow"))?;
    Ok(())
}

#[derive(Debug, Default)]
enum SavedTotals {
    #[default]
    Missing,
    Invalid(Problem),
    Present {
        values: Arc<Vec<Value>>,
        sum: Option<u64>,
    },
}

enum Current<'a> {
    Missing,
    Invalid(Problem),
    Present {
        values: &'a Arc<Vec<Value>>,
        sum: Option<u64>,
    },
}

#[derive(Debug)]
pub(crate) struct CapturedMemory {
    catalog: Arc<Catalog>,
    generations: [Option<u32>; 2],
    problem: Option<Problem>,
    scope: MemoryHistoryScope,
    initial: SavedTotals,
    end: SavedTotals,
}

impl CapturedMemory {
    fn saved(&self, saved: &SavedTotals) -> Result<Option<NativeMemoryTotals>, Error> {
        if let Some(problem) = self.problem {
            return Err(problem.error());
        }
        match saved {
            SavedTotals::Missing => Ok(None),
            SavedTotals::Invalid(problem) => Err(problem.error()),
            SavedTotals::Present { values, sum } => Ok(Some(NativeMemoryTotals {
                catalog: self.catalog.clone(),
                values: values.clone(),
                generations: self.generations,
                nonnegative_bytes: *sum,
            })),
        }
    }

    fn matches_instances(&self, state: &SceneState) -> bool {
        let Some(totals) = state.objects.get(&u64::from(self.catalog.ids[1])) else {
            return false;
        };
        let Some(buffer) = state.objects.get(&u64::from(self.catalog.ids[0])) else {
            return false;
        };
        totals.type_name == "_debugger::MemoryEventTotals"
            && buffer.type_name == "_debugger_::PxProfileMemoryEventBuffer"
            && self.generations == [Some(buffer.generation), Some(totals.generation)]
    }

    fn current<'a>(
        &self,
        state: &'a SceneState,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Current<'a>, Error> {
        check(proceed)?;
        if let Some(problem) = self.problem {
            return Ok(Current::Invalid(problem));
        }
        if !self.catalog.has_pair() {
            return Ok(Current::Missing);
        }
        let Some(totals) = state.objects.get(&u64::from(self.catalog.ids[1])) else {
            return Ok(Current::Missing);
        };
        if !state.objects.contains_key(&u64::from(self.catalog.ids[0])) {
            return Ok(Current::Invalid(Problem::MissingBuffer));
        }
        if !self.matches_instances(state) {
            return Ok(Current::Invalid(Problem::AmbiguousInstances));
        }
        let Some(value) = totals.properties.get("totals") else {
            return Ok(Current::Missing);
        };
        let Value::Array(values) = value else {
            return Ok(Current::Invalid(Problem::InvalidValues));
        };
        if values.len() > self.catalog.indices.len() {
            return Ok(Current::Invalid(Problem::InvalidValues));
        }
        for saved in [&self.initial, &self.end] {
            if let SavedTotals::Present { values: old, sum } = saved
                && Arc::ptr_eq(old, values)
            {
                return Ok(Current::Present { values, sum: *sum });
            }
        }
        let mut sum = Some(0u64);
        for (index, value) in values.iter().enumerate() {
            if index.is_multiple_of(256) {
                check(proceed)?;
            }
            let Value::I64(value) = value else {
                return Ok(Current::Invalid(Problem::InvalidValues));
            };
            let Ok(value) = i32::try_from(*value) else {
                return Ok(Current::Invalid(Problem::InvalidValues));
            };
            sum = sum.and_then(|sum| {
                u64::try_from(value)
                    .ok()
                    .and_then(|value| sum.checked_add(value))
            });
        }
        check(proceed)?;
        Ok(Current::Present { values, sum })
    }
}

pub(super) struct Collector(CapturedMemory);

impl Collector {
    pub(super) fn new(
        catalog: Catalog,
        metadata: &Metadata,
        builder: &mut RecordingBuilder,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<Self, Error> {
        check(proceed)?;
        let mut problem = catalog.problem;
        if catalog.has_pair()
            && problem.is_none()
            && (!schema(
                metadata,
                "_debugger_",
                "PxProfileMemoryEventBuffer",
                "events",
                "PvdU8",
                1,
                proceed,
            )? || !schema(
                metadata,
                "_debugger",
                "MemoryEventTotals",
                "totals",
                "PvdI32",
                4,
                proceed,
            )?)
        {
            problem = Some(Problem::UnsupportedSchema);
        }
        value_budget(builder, catalog.bytes)?;
        builder.reserve_history(catalog.bytes)?;
        Ok(Self(CapturedMemory {
            catalog: Arc::new(catalog),
            generations: [None; 2],
            problem,
            scope: MemoryHistoryScope::CapturedEvents,
            initial: SavedTotals::Missing,
            end: SavedTotals::Missing,
        }))
    }

    pub(super) fn initialize(
        &mut self,
        builder: &mut RecordingBuilder,
        initial_instances: usize,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if initial_instances != 0 {
            // A snapshot can omit retired memory buffers and their earlier cumulative history.
            self.0.scope = MemoryHistoryScope::AfterInitialSnapshot;
        }
        for object in builder.state.objects.values() {
            check(proceed)?;
            self.observe(object);
        }
        self.0.initial = self.capture(builder, allocation, proceed)?;
        Ok(())
    }

    pub(super) fn observe_created(&mut self, builder: &RecordingBuilder, id: u32) {
        if let Some(object) = builder.state.objects.get(&u64::from(id)) {
            self.observe(object);
        }
    }

    fn observe(&mut self, object: &crate::Object) {
        let expected = match object.type_name.as_str() {
            "_debugger_::PxProfileMemoryEventBuffer" => Some(0),
            "_debugger::MemoryEventTotals" => Some(1),
            _ => None,
        };
        let slot = self
            .0
            .catalog
            .ids
            .iter()
            .position(|id| self.0.catalog.has_pair() && u64::from(*id) == object.id);
        if expected != slot && (expected.is_some() || slot.is_some()) {
            self.0.problem = Some(Problem::AmbiguousInstances);
        }
        if let Some(slot) = slot {
            if self.0.generations[slot].is_some_and(|generation| generation != object.generation) {
                self.0.problem = Some(Problem::AmbiguousInstances);
            }
            self.0.generations[slot] = Some(object.generation);
        }
    }

    fn capture(
        &self,
        builder: &mut RecordingBuilder,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<SavedTotals, Error> {
        match self.0.current(&builder.state, proceed)? {
            Current::Missing => Ok(SavedTotals::Missing),
            Current::Invalid(problem) => Ok(SavedTotals::Invalid(problem)),
            Current::Present { values, sum } => {
                let shared = matches!(&self.0.initial, SavedTotals::Present { values: initial, .. } if Arc::ptr_eq(initial, values));
                let bytes = if shared {
                    0
                } else {
                    values
                        .capacity()
                        .checked_mul(size_of::<Value>())
                        .and_then(|bytes| bytes.checked_add(size_of::<Vec<Value>>() + 16))
                        .ok_or_else(|| error("native memory totals size overflow"))?
                };
                value_budget(builder, bytes)?;
                metadata_budget(allocation, 1, bytes)?;
                let values = values.clone();
                builder.reserve_history(bytes)?;
                Ok(SavedTotals::Present { values, sum })
            }
        }
    }

    pub(super) fn finish(
        mut self,
        builder: &mut RecordingBuilder,
        allocation: &mut usize,
        proceed: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        self.0.end = self.capture(builder, allocation, proceed)?;
        check(proceed)?;
        builder.recording.native_memory = Some(Arc::new(self.0));
        Ok(())
    }
}

fn schema(
    metadata: &Metadata,
    namespace: &str,
    class_name: &str,
    property_name: &str,
    datatype: &str,
    width: usize,
    proceed: &mut dyn FnMut() -> bool,
) -> Result<bool, Error> {
    let label = |handle| metadata.strings.get(&handle).map(String::as_str);
    let mut found = None;
    for class in metadata.classes.iter().flatten() {
        check(proceed)?;
        if label(class.name.namespace) == Some(namespace)
            && label(class.name.name) == Some(class_name)
        {
            if found.is_some() {
                return Ok(false);
            }
            found = Some(class);
        }
    }
    let Some(class) = found else {
        return Ok(false);
    };
    if class.base != u32::MAX
        || class.size != 24
        || class.properties.len() != 1
        || class.has_pointers
        || !class.dynamic
    {
        return Ok(false);
    }
    let property = metadata.property(class.properties[0])?;
    let value = metadata.class(property.datatype_id)?;
    Ok(property.owner == class.name
        && label(property.name) == Some(property_name)
        && property.kind == 2
        && property.offset32 == 0
        && label(property.datatype.namespace) == Some("physx3")
        && label(property.datatype.name) == Some(datatype)
        && value.name == property.datatype
        && value.size == width
        && value.base == u32::MAX
        && value.properties.is_empty()
        && !value.dynamic
        && !value.has_pointers)
}

fn check(proceed: &mut dyn FnMut() -> bool) -> Result<(), Error> {
    if !proceed() {
        return Err(cancelled());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
