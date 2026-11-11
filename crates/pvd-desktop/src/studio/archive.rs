use super::*;

pub(super) fn read(
    file: &mut File,
    archive_length: u64,
    cancelled: &AtomicBool,
    progress: &AtomicU64,
    history: pvd_core::live::HistoryConfig,
) -> LoadResult {
    let archive =
        pvd_core::pvd_archive::read_with_progress(file, &Default::default(), |done, total| {
            let completed = if total == 0 {
                0
            } else {
                (u128::from(done) * u128::from(archive_length) / u128::from(total)) as u64
            };
            progress.store(completed, Ordering::Relaxed);
            !cancelled.load(Ordering::Relaxed)
        })
        .map_err(|error| {
            Message::detail("Cannot open PVD archive", "无法打开 PVD 压缩录制", error)
        })?;
    let bytes = Arc::new(archive.bytes);
    let mut loaded = if bytes.starts_with(&pvd_core::native::MAGIC.to_le_bytes()) {
        let (recording, _) =
            pvd_core::native::read_bytes_with_history(&bytes, history, |completed, total| {
                let indexed = if total == 0 {
                    0
                } else {
                    (u128::from(completed) * u128::from(archive_length) / u128::from(total)) as u64
                };
                progress.store(indexed, Ordering::Relaxed);
                !cancelled.load(Ordering::Relaxed)
            })
            .map_err(|error| {
                Message::detail("Cannot import native capture", "无法导入原版录制", error)
            })?;
        validate_complete_disk_history(&recording)?;
        let identity = crate::clip_store::identity(&bytes, recording.frame_count(), cancelled)
            .map_err(|error| error.message())?;
        LoadedCapture {
            recording,
            bytes: None,
            source_path: None,
            source_identity: Some(identity),
            clips: None,
        }
    } else {
        read_sdk_reader(
            std::io::Cursor::new(bytes.as_slice()),
            cancelled,
            progress,
            history,
        )?
    };
    loaded.bytes = Some(bytes);
    progress.store(archive_length, Ordering::Relaxed);
    Ok(loaded)
}

#[cfg(test)]
mod tests;
