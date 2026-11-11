use super::*;

struct ErrorRecord<'a> {
    code: u32,
    message: &'a str,
    file: &'a str,
    line: u32,
    index: u32,
}

impl Metadata {
    fn error_record(&self, data: &[u8]) -> Result<ErrorRecord<'_>, Error> {
        if data.len() != 20 {
            return Err(error("PVD3 error record must contain exactly 20 bytes"));
        }
        let mut record = Reader::new(data);
        let code = record.u32()?;
        let message_handle = record.u32()?;
        let file_handle = record.u32()?;
        let line = record.u32()?;
        let index = record.u32()?;
        let resolve = |handle| {
            self.strings
                .get(&handle)
                .map(String::as_str)
                .ok_or_else(|| error(format!("undefined PVD3 error string handle {handle}")))
        };
        Ok(ErrorRecord {
            code,
            message: resolve(message_handle)?,
            file: resolve(file_handle)?,
            line,
            index,
        })
    }

    pub(super) fn append_error(
        &self,
        builder: &mut RecordingBuilder,
        id: u64,
        data: &[u8],
        history_event: u64,
    ) -> Result<(), Error> {
        let record = self.error_record(data)?;
        let first = builder.recording.untimed_errors.is_empty();
        let generation = reserve_error::<UntimedErrorEvent>(builder, id, &record, first)?;
        builder.recording.untimed_errors.push(UntimedErrorEvent {
            code: record.code,
            message: record.message.to_owned(),
            file: record.file.to_owned(),
            line: record.line,
            index: record.index,
            history_event,
            received_frame: received_frame(builder),
            container_instance: id,
            container_generation: generation,
        });
        Ok(())
    }

    pub(super) fn restore_initial_errors(
        &self,
        builder: &mut RecordingBuilder,
        id: u64,
        data: &[u8],
        progress: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        if !data.len().is_multiple_of(20) {
            return Err(error("invalid PVD3 snapshot error array length"));
        }
        check_error_count(&builder.recording, data.len() / 20)?;
        for data in data.chunks_exact(20) {
            if !progress() {
                return Err(cancelled());
            }
            let record = self.error_record(data)?;
            let first = builder.recording.initial_untimed_errors.is_empty();
            let generation = reserve_error::<InitialUntimedError>(builder, id, &record, first)?;
            builder
                .recording
                .initial_untimed_errors
                .push(InitialUntimedError {
                    code: record.code,
                    message: record.message.to_owned(),
                    file: record.file.to_owned(),
                    line: record.line,
                    index: record.index,
                    container_instance: id,
                    container_generation: generation,
                });
        }
        Ok(())
    }
}

fn check_error_count(recording: &Recording, additional: usize) -> Result<(), Error> {
    recording
        .untimed_errors
        .len()
        .checked_add(recording.initial_untimed_errors.len())
        .and_then(|count| count.checked_add(additional))
        .filter(|count| *count <= MAX_ITEMS)
        .ok_or_else(|| error("PVD3 error list exceeds one million records"))?;
    Ok(())
}

fn reserve_error<T>(
    builder: &mut RecordingBuilder,
    id: u64,
    record: &ErrorRecord<'_>,
    first: bool,
) -> Result<u32, Error> {
    let generation = builder
        .state
        .objects
        .get(&id)
        .ok_or_else(|| error("PVD3 error container instance is missing"))?
        .generation;
    check_error_count(&builder.recording, 1)?;
    let bytes = record
        .message
        .len()
        .checked_add(record.file.len())
        .and_then(|bytes| bytes.checked_add((if first { 4 } else { 2 }) * size_of::<T>()))
        .ok_or_else(|| error("PVD3 error allocation overflow"))?;
    value_budget(builder, bytes)?;
    builder.reserve_history(bytes)?;
    Ok(generation)
}

#[cfg(test)]
mod tests;
