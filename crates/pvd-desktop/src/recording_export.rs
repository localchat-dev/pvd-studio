use std::{
    fs::File,
    io::{self, BufWriter, Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
};

use crate::{clip_store::Identity, i18n::Message};
use pvd_core::analysis_export::{AnalysisCsvKind, AnalysisExportPhase, AnalysisExportProgress};
use pvd_core::clip::{TrimPhase, TrimProgress};

#[derive(Clone, Copy)]
pub enum Kind {
    Clip { first: usize, last: usize },
    Native(pvd_core::native::export::Selection),
    Analysis(AnalysisCsvKind),
    Retained { first: usize, last: usize },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Inspect,
    Export,
    NativeRead,
    NativeExport,
    NativeRecheck,
    LoadedRows,
    CaptureInspect,
    CaptureCopy,
    CaptureValidate,
}

#[derive(Clone, Copy)]
pub struct Progress {
    pub phase: Phase,
    pub completed: u64,
    pub total: u64,
}

impl From<TrimProgress> for Progress {
    fn from(value: TrimProgress) -> Self {
        Self {
            phase: match value.phase {
                TrimPhase::Inspect => Phase::Inspect,
                TrimPhase::Export => Phase::Export,
            },
            completed: value.bytes_read,
            total: value.total_bytes,
        }
    }
}

impl From<AnalysisExportProgress> for Progress {
    fn from(value: AnalysisExportProgress) -> Self {
        Self {
            phase: match value.phase {
                AnalysisExportPhase::Inspect => Phase::Inspect,
                AnalysisExportPhase::Export => Phase::Export,
            },
            completed: value.bytes_read,
            total: value.total_bytes,
        }
    }
}

#[derive(Clone, Default)]
pub struct Workers(Arc<Mutex<Vec<Arc<AtomicBool>>>>);

impl Workers {
    pub(crate) fn acquire(&self, cancelled: Arc<AtomicBool>) -> Result<WorkerPermit, Message> {
        let mut workers = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if workers.len() >= 2 {
            return Err(Message::new(
                "Previous recording exports are still stopping; retry when they finish.",
                "先前录制导出仍在停止，请等待完成后重试。",
            ));
        }
        workers.push(cancelled.clone());
        Ok(WorkerPermit {
            workers: self.clone(),
            cancelled,
        })
    }

    pub fn cancel_all_and_check_finished(&self) -> bool {
        let workers = self.0.lock().unwrap_or_else(|error| error.into_inner());
        for cancelled in workers.iter() {
            cancelled.store(true, Ordering::Relaxed);
        }
        workers.is_empty()
    }
}

pub enum Source {
    Memory(Arc<Vec<u8>>),
    File(PathBuf, Identity),
    Loaded(Arc<pvd_core::Recording>),
    Retained {
        recording: Arc<pvd_core::Recording>,
        epoch: Arc<AtomicU64>,
        expected_epoch: u64,
    },
}

pub(crate) struct WorkerPermit {
    workers: Workers,
    cancelled: Arc<AtomicBool>,
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.workers
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|cancelled| !Arc::ptr_eq(cancelled, &self.cancelled));
    }
}

pub struct Job {
    kind: Kind,
    result: Mutex<mpsc::Receiver<(Message, Option<PathBuf>)>>,
    progress: Arc<Mutex<Progress>>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Job {
    pub fn start(
        source: Source,
        destination: PathBuf,
        kind: Kind,
        context: egui::Context,
        workers: &Workers,
    ) -> Result<Self, Message> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let permit = workers.acquire(cancelled.clone())?;
        let (sender, receiver) = mpsc::channel();
        let progress = Arc::new(Mutex::new(Progress {
            phase: Phase::Inspect,
            completed: 0,
            total: 0,
        }));
        let worker_progress = progress.clone();
        let worker_cancel = cancelled.clone();
        std::thread::Builder::new()
            .name("recording-export".into())
            .spawn(move || {
                let _permit = permit;
                let update = |value| {
                    if let Ok(mut progress) = worker_progress.lock() {
                        *progress = value;
                    }
                    !worker_cancel.load(Ordering::Relaxed)
                };
                let result = match source {
                    Source::Memory(bytes) => run_export(
                        &mut Cursor::new(bytes.as_slice()),
                        &destination,
                        kind,
                        None,
                        &worker_cancel,
                        update,
                    ),
                    Source::File(path, identity) => {
                        open_export_source(&path).and_then(|mut file| {
                            run_export(
                                &mut file,
                                &destination,
                                kind,
                                Some(&identity),
                                &worker_cancel,
                                update,
                            )
                        })
                    }
                    Source::Retained {
                        recording,
                        epoch,
                        expected_epoch,
                    } => export_retained(&recording, &destination, kind, |value| {
                        !worker_cancel.load(Ordering::Relaxed)
                            && epoch.load(Ordering::Relaxed) == expected_epoch
                            && update(value)
                    }),
                    Source::Loaded(recording) => {
                        export_loaded(&recording, &destination, kind, &worker_cancel, update)
                    }
                };
                let outcome = match result {
                    Ok(message) => (
                        message,
                        matches!(kind, Kind::Retained { .. }).then_some(destination),
                    ),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => (
                        Message::new("Recording export cancelled", "录制导出已取消"),
                        None,
                    ),
                    Err(error) => (
                        Message::detail("Recording export failed", "录制导出失败", error),
                        None,
                    ),
                };
                let _ = sender.send(outcome);
                context.request_repaint();
            })
            .map_err(|error| {
                Message::detail("Cannot start recording export", "无法启动录制导出", error)
            })?;
        Ok(Self {
            kind,
            result: Mutex::new(receiver),
            progress,
            cancelled,
        })
    }

    pub fn result(&self) -> Option<(Message, Option<PathBuf>)> {
        match self.result.lock().ok()?.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some((
                Message::new(
                    "The recording export worker stopped before returning a result.",
                    "录制导出线程未返回结果便已停止。",
                ),
                None,
            )),
        }
    }

    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().ok().map(|value| *value)
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

fn open_export_source(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1);
    }
    options.open(path)
}

fn run_export<R: Read + Seek>(
    source: &mut R,
    destination: &Path,
    kind: Kind,
    expected: Option<&Identity>,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress) -> bool,
) -> io::Result<Message> {
    match kind {
        Kind::Native(selection) => {
            if !destination
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pxd2"))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "PVD3 container export requires a .pxd2 output",
                ));
            }
            crate::atomic_output::create_parent_directory(destination)?;
            let mut report = None;
            crate::atomic_output::write_with(destination, |output| {
                let value =
                    pvd_core::native::export::write_reader(source, selection, output, |value| {
                        progress(Progress {
                            phase: match value.phase {
                                pvd_core::native::export::Phase::Read => Phase::NativeRead,
                                pvd_core::native::export::Phase::Export => Phase::NativeExport,
                                pvd_core::native::export::Phase::Recheck => Phase::NativeRecheck,
                            },
                            completed: value.completed,
                            total: value.total,
                        })
                    })
                    .map_err(export_error)?;
                validate_identity(
                    expected,
                    *value.source_blake3.as_bytes(),
                    value.source_bytes as u64,
                    value.source_frames,
                )?;
                output.sync_all()?;
                if cancelled.load(Ordering::Relaxed) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                report = Some(value);
                Ok(())
            })?;
            let report = report.unwrap();
            Ok(Message::new(
                format!(
                    "Saved PVD3 recording: {} frames, {} bytes to {}",
                    report.frames,
                    report.output_bytes,
                    destination.display()
                ),
                format!(
                    "已保存 PVD3 录制：{} 帧，{} 字节，路径 {}",
                    report.frames,
                    report.output_bytes,
                    destination.display()
                ),
            ))
        }
        Kind::Retained { .. } => Err(io::Error::other(
            "Retained capture export requires a disk history source",
        )),
        Kind::Clip { first, last } => {
            crate::atomic_output::create_parent_directory(destination)?;
            export(
                source,
                destination,
                (first, last),
                expected,
                cancelled,
                |value| progress(value.into()),
            )?;
            Ok(Message::new(
                format!(
                    "Saved scene frames {first}..={last} to {}; initialization and analysis preroll retained",
                    destination.display()
                ),
                format!(
                    "已将场景帧 {first}..={last} 保存到 {}；保留初始化和前置分析事件",
                    destination.display()
                ),
            ))
        }
        Kind::Analysis(kind) => {
            let mut report = None;
            crate::atomic_output::write_with(destination, |output| {
                let value = {
                    let mut buffered = BufWriter::with_capacity(64 * 1024, &mut *output);
                    let value = pvd_core::analysis_export::export_analysis_csv_streaming(
                        source,
                        &mut buffered,
                        kind,
                        |value| progress(value.into()),
                    )
                    .map_err(export_error)?;
                    buffered.flush()?;
                    value
                };
                validate_identity(
                    expected,
                    value.source_blake3,
                    value.source_bytes,
                    value.source_frames,
                )?;
                output.sync_all()?;
                if cancelled.load(Ordering::Relaxed) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                report = Some(value);
                Ok(())
            })?;
            let report = report.unwrap();
            Ok(analysis_message(
                kind,
                destination,
                report.exported_rows,
                &report.summary,
            ))
        }
    }
}

fn export_retained(
    recording: &pvd_core::Recording,
    destination: &Path,
    kind: Kind,
    mut progress: impl FnMut(Progress) -> bool,
) -> io::Result<Message> {
    let Kind::Retained { first, last } = kind else {
        return Err(io::Error::other(
            "Retained history requires project capture export",
        ));
    };
    crate::atomic_output::create_parent_directory(destination)?;
    let range = pvd_core::capture::CaptureRange { first, last };
    let report_progress = |value: pvd_core::capture::CaptureProgress| {
        progress(Progress {
            phase: match value.phase {
                pvd_core::capture::CapturePhase::Inspect => Phase::CaptureInspect,
                pvd_core::capture::CapturePhase::Copy => Phase::CaptureCopy,
                pvd_core::capture::CapturePhase::Validate => Phase::CaptureValidate,
            },
            completed: value.completed,
            total: value.total,
        })
    };
    // The byte limit is not an in-memory allocation budget. Complete captures
    // can exceed the hot-history budget; index/resource limits remain bounded.
    let limits = pvd_core::capture::CaptureLimits {
        output_bytes: (1 << 63) - 1,
        ..Default::default()
    };
    let report = pvd_core::capture::write_portable_new(
        recording,
        destination,
        range,
        &limits,
        512 << 20,
        report_progress,
    )
    .map_err(export_error)?;
    Ok(Message::new(
        format!(
            "Saved project capture frames {first}..={last} ({} bytes) to {}",
            report.bytes,
            destination.display()
        ),
        format!(
            "已保存项目捕获帧 {first}..={last}（{} 字节）到 {}",
            report.bytes,
            destination.display()
        ),
    ))
}

fn export_loaded(
    recording: &pvd_core::Recording,
    destination: &Path,
    kind: Kind,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(Progress) -> bool,
) -> io::Result<Message> {
    let Kind::Analysis(kind) = kind else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Native recordings cannot be exported as SDK clips.",
        ));
    };
    let mut report = None;
    crate::atomic_output::write_with(destination, |output| {
        let value = {
            let mut buffered = BufWriter::with_capacity(64 * 1024, &mut *output);
            let value = pvd_core::analysis_export::write_analysis_csv(
                recording,
                &mut buffered,
                kind,
                |value| {
                    progress(Progress {
                        phase: Phase::LoadedRows,
                        completed: value.rows_written,
                        total: value.total_rows,
                    }) && !cancelled.load(Ordering::Relaxed)
                },
            )
            .map_err(export_error)?;
            buffered.flush()?;
            value
        };
        output.sync_all()?;
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        report = Some(value);
        Ok(())
    })?;
    let report = report.unwrap();
    let message = analysis_message(kind, destination, report.exported_rows, &report.summary);
    Ok(if recording.diagnostics.is_empty() {
        message
    } else {
        Message::new(
            format!(
                "{}; native import limitations remain listed in Diagnostics",
                message.text(crate::i18n::Language::English)
            ),
            format!(
                "{}；原版导入的支持范围限制仍在诊断面板列出",
                message.text(crate::i18n::Language::Chinese)
            ),
        )
    })
}

fn analysis_message(
    kind: AnalysisCsvKind,
    destination: &Path,
    exported_rows: u64,
    summary: &pvd_core::analysis_export::AnalysisExportSummary,
) -> Message {
    let (en, zh) = match kind {
        AnalysisCsvKind::Profile => ("completed performance spans", "完成性能区间"),
        AnalysisCsvKind::Memory => ("memory samples", "内存变化汇总"),
    };
    let mut message = Message::new(
        format!(
            "Exported the complete recording's {en}: {} rows to {}",
            exported_rows,
            destination.display()
        ),
        format!(
            "已导出整份录制的{zh}：{} 行，保存到 {}",
            exported_rows,
            destination.display()
        ),
    );
    if summary.unmatched_stops != 0
        || summary.open_spans != 0
        || summary.unmatched_frees != 0
        || summary.replaced_addresses != 0
    {
        message = Message::new(
            format!(
                "{}; unmatched stops {}, open spans {}, unmatched frees {}, replaced addresses {}",
                message.text(crate::i18n::Language::English),
                summary.unmatched_stops,
                summary.open_spans,
                summary.unmatched_frees,
                summary.replaced_addresses
            ),
            format!(
                "{}；未匹配结束 {}，未结束区间 {}，未匹配释放 {}，重复分配地址 {}",
                message.text(crate::i18n::Language::Chinese),
                summary.unmatched_stops,
                summary.open_spans,
                summary.unmatched_frees,
                summary.replaced_addresses
            ),
        );
    }
    if !summary.ambiguous_zones.is_empty() || !summary.ambiguous_events.is_empty() {
        message = Message::new(
            format!(
                "{}; names changed during this recording; CSV uses final metadata ({} zones, {} events affected)",
                message.text(crate::i18n::Language::English),
                summary.ambiguous_zones.len(),
                summary.ambiguous_events.len()
            ),
            format!(
                "{}；录制期间名称发生变化，CSV 使用最终元数据（涉及 {} 个区域、{} 个事件）",
                message.text(crate::i18n::Language::Chinese),
                summary.ambiguous_zones.len(),
                summary.ambiguous_events.len()
            ),
        );
    }
    if matches!(kind, AnalysisCsvKind::Profile) && !summary.duplicate_profile_names.is_empty() {
        message = Message::new(
            format!(
                "{}; {} groups of distinct event IDs share names; names do not uniquely identify events in this CSV",
                message.text(crate::i18n::Language::English),
                summary.duplicate_profile_names.len()
            ),
            format!(
                "{}；{} 组不同事件 ID 使用相同名称，此 CSV 的名称不能唯一标识事件",
                message.text(crate::i18n::Language::Chinese),
                summary.duplicate_profile_names.len()
            ),
        );
    }
    message
}

fn export_error(error: pvd_core::Error) -> io::Error {
    match error.downcast::<io::Error>() {
        Ok(error) => *error,
        Err(error) => io::Error::other(error),
    }
}

fn validate_identity(
    expected: Option<&Identity>,
    digest: [u8; 32],
    bytes: u64,
    frames: usize,
) -> io::Result<()> {
    if expected.is_some_and(|identity| !identity.matches(digest, bytes, frames)) {
        return Err(io::Error::other(
            "The source recording changed since it was opened; reopen it before exporting.",
        ));
    }
    Ok(())
}

fn export<R: Read + Seek>(
    source: &mut R,
    destination: &Path,
    range: (usize, usize),
    expected: Option<&Identity>,
    cancelled: &AtomicBool,
    progress: impl FnMut(TrimProgress) -> bool,
) -> io::Result<()> {
    crate::atomic_output::write_with(destination, |output| {
        let report = {
            let mut buffered = BufWriter::with_capacity(64 * 1024, &mut *output);
            let report = pvd_core::clip::trim_scene_frames_streaming(
                source,
                &mut buffered,
                range.0,
                range.1,
                progress,
            )
            .map_err(export_error)?;
            buffered.flush()?;
            report
        };
        validate_identity(
            expected,
            report.source_blake3,
            report.source_bytes,
            report.source_frames,
        )?;
        output.sync_all()?;
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    })
}

pub fn show_analysis_controls(
    ui: &mut egui::Ui,
    studio: &mut crate::studio::Studio,
    kind: AnalysisCsvKind,
) {
    let language = studio.prefs.language;
    egui::CollapsingHeader::new(language.text("Export full recording CSV", "导出整份录制 CSV"))
        .id_salt(match kind {
            AnalysisCsvKind::Profile => "profile-csv-export",
            AnalysisCsvKind::Memory => "memory-csv-export",
        })
        .open(studio.recording_export.as_ref().map(|_| true))
        .show(ui, |ui| analysis_controls(ui, studio, kind, false));
    ui.separator();
}

pub fn show_analysis_menu(
    ui: &mut egui::Ui,
    studio: &mut crate::studio::Studio,
    kind: AnalysisCsvKind,
) {
    let language = studio.prefs.language;
    let label = if studio.recording_export.is_some() {
        language.text("Exporting…", "导出中…")
    } else {
        language.text("Export CSV", "导出 CSV")
    };
    crate::ui::settings_menu(ui, label, |ui| {
        ui.set_max_width(480.0);
        analysis_controls(ui, studio, kind, true);
    });
}

fn analysis_controls(
    ui: &mut egui::Ui,
    studio: &mut crate::studio::Studio,
    kind: AnalysisCsvKind,
    menu: bool,
) {
    let language = studio.prefs.language;
    let incomplete_memory = kind == AnalysisCsvKind::Memory
        && studio.recording.as_ref().is_some_and(|recording| {
            recording.memory_history_scope() == pvd_core::MemoryHistoryScope::AfterInitialSnapshot
        });
    let can_export = studio.can_export_analysis() && !incomplete_memory;
    let label = match kind {
        AnalysisCsvKind::Profile => language.text("Export all completed spans", "导出全部完成区间"),
        AnalysisCsvKind::Memory if incomplete_memory => {
            language.text("Full memory CSV unavailable", "完整内存统计不可用")
        }
        AnalysisCsvKind::Memory => language.text("Export all memory samples", "导出全部内存汇总"),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(language.text("Output", "输出"));
        ui.add(egui::TextEdit::singleline(&mut studio.analysis_export_path).desired_width(220.0));
        if ui
            .add_enabled(
                !studio.file_picker.is_busy(),
                egui::Button::new(language.text("Choose path…", "选择路径…")),
            )
            .clicked()
        {
            studio.pick_file(
                crate::file_picker::PickerKind::SaveAnalysisCsv,
                ui.ctx().clone(),
            );
            if menu {
                ui.close();
            }
        }
        if ui
            .add_enabled(can_export, egui::Button::new(label))
            .clicked()
        {
            studio.export_analysis(kind, ui.ctx().clone());
            if menu {
                ui.close();
            }
        }
    });
    if !can_export && studio.recording_export.is_none() {
        let (en, zh) = if incomplete_memory {
            (
                "Earlier memory events are unavailable; full allocation statistics cannot be exported.",
                "缺少起点前内存事件，不能导出完整分配统计。",
            )
        } else if studio.is_project_capture() {
            (
                "This project capture contains a retained interval; full-source analysis export is unavailable.",
                "本项目捕获仅包含保留区间，不能导出完整源分析。",
            )
        } else if studio.job.is_some() {
            (
                "Wait for the recording to finish opening.",
                "请等待录制打开完成。",
            )
        } else if studio.server.is_some() {
            (
                "Stop listening, save the completed raw recording and reopen it to export the full file.",
                "请停止监听、保存完整原始录制并重新打开，再导出整份文件。",
            )
        } else if studio.save_job.is_some() {
            ("Another export is running.", "另一项导出正在进行。")
        } else {
            (
                "Save the completed raw recording and reopen it to export the full file.",
                "请保存完整原始录制并重新打开，再导出整份文件。",
            )
        };
        ui.label(language.text(en, zh));
    }
    ui.label(egui::RichText::new(match kind {
        AnalysisCsvKind::Profile => language.text("Includes completed CPU spans across the full file; counter values and CUDA records are separate event types.", "包含完整文件的已完成 CPU 区间；计数值和 CUDA 记录属于其他事件类型。"),
        AnalysisCsvKind::Memory if incomplete_memory => language.text("Only memory events captured in this file are available.", "仅可分析文件内捕获的内存事件。"),
        AnalysisCsvKind::Memory => language.text("One cumulative sample per received memory buffer, covering the full file.", "覆盖完整文件，每个内存缓冲对应一条累计汇总。"),
    }).small());
    if studio
        .recording
        .as_ref()
        .is_some_and(|recording| recording.source == pvd_core::RecordingSource::Pvd3Database)
    {
        ui.label(language.text(
            "Exports the imported native recording. Names are saved metadata; unsupported native data remains listed in Diagnostics.",
            "导出已导入的原版录制数据。名称采用保存时元数据；未支持的数据仍在诊断面板列出。",
        ));
    }
    show_progress(ui, studio);
}

fn retained_render_requested(context: &egui::Context) -> bool {
    context.data(|data| {
        data.get_temp::<bool>(egui::Id::new("retained-capture-render-check"))
            .unwrap_or(false)
    })
}

pub(crate) fn prepare_retained_render(
    context: &egui::Context,
    studio: &mut crate::studio::Studio,
) -> Result<(), &'static str> {
    if !studio.can_export_retained() {
        return Err(
            "retained-capture render requires completed disk-backed SDK history or a project capture",
        );
    }
    if !retained_render_requested(context) {
        studio.use_retained_range();
        context.data_mut(|data| {
            data.insert_temp(egui::Id::new("retained-capture-render-check"), true);
        });
    }
    Ok(())
}

pub fn retained_controls(ui: &mut egui::Ui, studio: &mut crate::studio::Studio) {
    if studio.is_native_recording() {
        studio.native_export_controls(ui);
        return;
    }
    let language = studio.prefs.language;
    egui::CollapsingHeader::new(language.text("Save retained history", "保存保留历史"))
        .default_open(studio.live_connected)
        .open(retained_render_requested(ui.ctx()).then_some(true))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let available = studio.retained_range();
            ui.label(language.text(
                &format!("Available frames: {}..={}", available.map_or(0, |r| r.first), available.map_or(0, |r| r.last)),
                &format!("可用帧：{}..={}", available.map_or(0, |r| r.first), available.map_or(0, |r| r.last)),
            ));
            ui.horizontal_wrapped(|ui| {
                ui.label(language.text("First", "首帧"));
                ui.add(egui::DragValue::new(&mut studio.retained_first));
                ui.label(language.text("Last", "末帧"));
                ui.add(egui::DragValue::new(&mut studio.retained_last));
                if ui.button(language.text("Use available range", "使用可用范围")).clicked() {
                    studio.use_retained_range();
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut studio.retained_path)
                    .hint_text(language.text("Name by recording and range", "按录制和帧范围自动命名"))
                    .desired_width(ui.available_width()),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(!studio.file_picker.is_busy(), egui::Button::new(language.text("Choose path…", "选择路径…"))).clicked() {
                    studio.pick_file(crate::file_picker::PickerKind::SaveRetainedCapture, ui.ctx().clone());
                }
                if ui.add_enabled(studio.can_export_retained(), egui::Button::new(language.text("Save retained capture", "保存保留捕获"))).clicked() {
                    studio.export_retained(ui.ctx().clone());
                }
            });
            ui.label(egui::RichText::new(language.text(
                "Save the available scene and analysis history as one .pvdcapture file. Long recordings are split into parts inside the file.",
                "场景和分析历史保存为一个 .pvdcapture 文件。长录制会在文件内部自动分段。",
            )).small());
            if !studio.can_export_retained() && studio.recording_export.is_none() {
                ui.label(language.text("Requires completed disk-backed SDK frames or an opened project capture, with no open/save job running.", "需要磁盘历史中已完成的 SDK 帧或已打开的项目捕获，并等待打开、保存任务结束。"));
            }
            ui.label(language.text("The destination must be new. If history expires during saving, the export fails without stopping reception.", "目标文件必须不存在。若保存期间历史过期，导出会失败，接收不会停止。"));
        });
}

pub fn show_progress(ui: &mut egui::Ui, studio: &crate::studio::Studio) {
    let Some(job) = &studio.recording_export else {
        return;
    };
    let language = studio.prefs.language;
    ui.label(match job.kind {
        Kind::Native(_) => language.text("PVD3 recording export", "PVD3 录制导出"),
        Kind::Retained { .. } => language.text("Retained project capture", "保留范围的项目捕获"),
        Kind::Clip { .. } => language.text("Recording clip export", "录制片段导出"),
        Kind::Analysis(AnalysisCsvKind::Profile) => {
            language.text("Completed performance spans CSV", "完成性能区间 CSV")
        }
        Kind::Analysis(AnalysisCsvKind::Memory) => {
            language.text("Memory samples CSV", "内存汇总 CSV")
        }
    });
    if let Some(progress) = job.progress() {
        let (phase, offset) = match progress.phase {
            Phase::NativeRead => (
                language.text("Reading native recording", "读取原版录制"),
                0.0,
            ),
            Phase::NativeExport => (
                language.text("Writing native recording", "保存原版录制"),
                0.5,
            ),
            Phase::NativeRecheck => (
                language.text("Rechecking source recording", "复核源录制"),
                0.5,
            ),
            Phase::CaptureInspect => (
                language.text("Inspecting retained frames", "检查保留帧"),
                0.0,
            ),
            Phase::CaptureCopy => (language.text("Saving retained frames", "保存保留帧"), 0.0),
            Phase::CaptureValidate => (
                language.text("Verifying saved capture", "验证保存的捕获"),
                0.0,
            ),
            crate::recording_export::Phase::Inspect => {
                (language.text("Inspecting source", "检查源录制"), 0.0)
            }
            crate::recording_export::Phase::Export => {
                (language.text("Exporting and verifying", "导出并验证"), 0.5)
            }
            crate::recording_export::Phase::LoadedRows => (
                language.text("Exporting imported rows", "导出已导入的数据行"),
                0.0,
            ),
        };
        let fraction = progress.completed as f64 / progress.total.max(1) as f64;
        let (scale, label) = if matches!(
            progress.phase,
            Phase::LoadedRows | Phase::CaptureInspect | Phase::CaptureCopy
        ) {
            (
                1.0,
                format!("{phase} · {} / {}", progress.completed, progress.total),
            )
        } else {
            (
                if progress.phase == Phase::CaptureValidate {
                    1.0
                } else {
                    0.5
                },
                format!(
                    "{phase} · {:.1} / {:.1} MiB",
                    progress.completed as f64 / 1_048_576.0,
                    progress.total as f64 / 1_048_576.0,
                ),
            )
        };
        if progress.total == 0 {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(phase);
            });
        } else {
            ui.add(
                egui::ProgressBar::new((offset + fraction.clamp(0.0, 1.0) * scale) as f32)
                    .desired_width(ui.available_width().min(360.0))
                    .text(label),
            );
        }
    }
    if ui
        .add_enabled(
            !job.cancelling(),
            egui::Button::new(if job.cancelling() {
                language.text("Cancelling…", "正在取消…")
            } else {
                language.text("Cancel export", "取消导出")
            }),
        )
        .clicked()
    {
        job.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    #[ignore = "requires preserved real PVD3 baseline container"]
    fn native_exports_preserve_source_identity_cancellation_and_existing_outputs() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../captures/native/baseline-paired-native.pxd2");
        let bytes = fs::read(&source).unwrap();
        let recording = pvd_core::Recording::from_bytes(&bytes).unwrap();
        let identity = Identity::from_digest(
            blake3::hash(&bytes),
            bytes.len() as u64,
            recording.frame_count(),
        );
        let wrong = Identity::from_digest(
            blake3::hash(b"different source"),
            bytes.len() as u64,
            recording.frame_count(),
        );
        let directory = std::env::temp_dir().join(format!(
            "pvd-native-export-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let output = directory.join("prefix.pxd2");
        let cancelled = AtomicBool::new(false);
        let kind = Kind::Native(pvd_core::native::export::Selection::Frames { first: 0, last: 29 });
        assert!(
            run_export(
                &mut Cursor::new(&bytes),
                &output,
                kind,
                Some(&wrong),
                &cancelled,
                |_| true,
            )
            .is_err()
        );
        assert!(!output.exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        let error = run_export(
            &mut Cursor::new(&bytes),
            &output,
            kind,
            Some(&identity),
            &cancelled,
            |progress| progress.phase != Phase::NativeRecheck,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!output.exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        run_export(
            &mut Cursor::new(&bytes),
            &output,
            kind,
            Some(&identity),
            &cancelled,
            |_| true,
        )
        .unwrap();
        let saved = fs::read(&output).unwrap();
        let clipped = pvd_core::Recording::from_bytes(&saved).unwrap();
        assert_eq!(clipped.frame_count(), 30);
        for frame in [0, 29, 1, 28] {
            assert_eq!(
                clipped.state_at(frame).unwrap(),
                recording.state_at(frame).unwrap()
            );
        }
        assert!(
            run_export(
                &mut Cursor::new(&bytes),
                &output,
                kind,
                Some(&identity),
                &cancelled,
                |_| true,
            )
            .is_err()
        );
        assert_eq!(fs::read(&output).unwrap(), saved);
        assert_eq!(fs::read(&source).unwrap(), bytes);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        let middle = directory.join("middle.pxd2");
        run_export(
            &mut Cursor::new(&bytes),
            &middle,
            Kind::Native(pvd_core::native::export::Selection::Frames {
                first: 30,
                last: 31,
            }),
            Some(&identity),
            &cancelled,
            |_| true,
        )
        .unwrap();
        let clipped = pvd_core::Recording::from_bytes(&fs::read(&middle).unwrap()).unwrap();
        assert_eq!(clipped.frame_count(), 2);
        for frame in [1, 0, 1] {
            assert_eq!(
                clipped.state_at(frame).unwrap(),
                recording.state_at(frame + 30).unwrap()
            );
        }
        fs::remove_file(middle).unwrap();
        fs::remove_file(output).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn closing_waits_for_detached_export_cleanup() {
        use bevy::{
            app::AppExit,
            ecs::message::Messages,
            prelude::*,
            window::{PrimaryWindow, WindowCloseRequested},
        };
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/clip-export-tests")
            .join(format!("exit-cleanup-{}", std::process::id()));
        fs::create_dir_all(directory.parent().unwrap()).unwrap();
        fs::create_dir(&directory).unwrap();
        let destination = directory.join("unfinished.csv");
        let mut app = App::new();
        app.add_message::<WindowCloseRequested>()
            .add_message::<AppExit>()
            .insert_resource(crate::studio::Studio::default())
            .add_systems(Update, crate::clips::handle_window_close);
        let primary = app
            .world_mut()
            .spawn((Window::default(), PrimaryWindow))
            .id();
        let workers = app
            .world()
            .resource::<crate::studio::Studio>()
            .export_workers
            .clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let permit = workers.acquire(cancelled.clone()).unwrap();
        let (ready, prepared) = mpsc::channel();
        let (release, proceed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _permit = permit;
            let result = crate::atomic_output::write_with(&destination, |output| {
                output.write_all(b"partial export")?;
                ready.send(()).unwrap();
                proceed.recv().unwrap();
                assert!(cancelled.load(Ordering::Relaxed));
                Err(io::ErrorKind::Interrupted.into())
            });
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
            assert!(!destination.exists());
        });
        prepared
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        app.world_mut()
            .write_message(WindowCloseRequested { window: primary });
        app.update();
        assert!(
            app.world()
                .resource::<crate::studio::Studio>()
                .exit_requested
        );
        assert!(app.world().resource::<Messages<AppExit>>().is_empty());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        release.send(()).unwrap();
        worker.join().unwrap();
        app.update();
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<AppExit>>()
                .drain()
                .count(),
            1
        );
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
        fs::remove_dir(directory).unwrap();
    }

    fn fixture() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(pvd_protocol::STREAM_MAGIC.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        let mut name = vec![1];
        name.extend(6u32.to_le_bytes());
        name.extend(b"frame\0");
        name.extend(1u32.to_le_bytes());
        bytes.extend((name.len() as u32).to_le_bytes());
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(1u64.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(name);
        for frame in 0..8u64 {
            let mut events = Vec::new();
            for kind in [18, 19] {
                events.push(kind);
                events.extend(17u64.to_le_bytes());
                events.extend(1u32.to_le_bytes());
                events.extend(frame.to_le_bytes());
            }
            bytes.extend((events.len() as u32).to_le_bytes());
            bytes.extend(2u32.to_le_bytes());
            bytes.extend(1u64.to_le_bytes());
            bytes.extend(frame.to_le_bytes());
            bytes.extend(events);
        }
        bytes
    }

    #[test]
    fn loaded_analysis_cancel_and_incomplete_data_do_not_publish() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/clip-export-tests")
            .join(format!("loaded-publication-{}", std::process::id()));
        fs::create_dir_all(directory.parent().unwrap()).unwrap();
        fs::create_dir(&directory).unwrap();
        let destination = directory.join("analysis.csv");
        let mut recording = pvd_core::Recording::from_bytes(&fixture()).unwrap();
        for stop_at in [1, 3] {
            let mut calls = 0;
            let error = export_loaded(
                &recording,
                &destination,
                Kind::Analysis(AnalysisCsvKind::Profile),
                &AtomicBool::new(false),
                |progress| {
                    assert!(progress.phase == Phase::LoadedRows);
                    calls += 1;
                    calls != stop_at
                },
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert!(fs::read_dir(&directory).unwrap().next().is_none());
        }
        recording
            .diagnostics
            .push("Analysis decoding stopped at frame 4: incomplete input".into());
        assert!(
            export_loaded(
                &recording,
                &destination,
                Kind::Analysis(AnalysisCsvKind::Memory),
                &AtomicBool::new(false),
                |_| true,
            )
            .is_err()
        );
        assert!(fs::read_dir(&directory).unwrap().next().is_none());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn analysis_exports_share_cancellation_and_identity_publication_guards() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/clip-export-tests")
            .join(format!("analysis-publication-{}", std::process::id()));
        fs::create_dir_all(directory.parent().unwrap()).unwrap();
        fs::create_dir(&directory).unwrap();
        let bytes = fixture();
        let stale = Identity::from_digest(blake3::hash(b"replaced source"), bytes.len() as u64, 8);
        let cancel = AtomicBool::new(false);
        for kind in [AnalysisCsvKind::Profile, AnalysisCsvKind::Memory] {
            let path = directory.join("analysis.csv");
            for phase in [Phase::Inspect, Phase::Export] {
                let error = run_export(
                    &mut Cursor::new(&bytes),
                    &path,
                    Kind::Analysis(kind),
                    None,
                    &cancel,
                    |progress| !(progress.phase == phase && progress.completed > 28),
                )
                .unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
                assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
            }
            assert!(
                run_export(
                    &mut Cursor::new(&bytes),
                    &path,
                    Kind::Analysis(kind),
                    Some(&stale),
                    &cancel,
                    |_| true
                )
                .is_err()
            );
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
            run_export(
                &mut Cursor::new(&bytes),
                &path,
                Kind::Analysis(kind),
                None,
                &cancel,
                |_| true,
            )
            .unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn cancellation_and_source_identity_failure_never_publish_partial_clips() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/clip-export-tests")
            .join(format!("publication-{}", std::process::id()));
        fs::create_dir_all(directory.parent().unwrap()).unwrap();
        fs::create_dir(&directory).unwrap();
        let destination = directory.join("片段.pvdstream");
        let bytes = fixture();
        assert_eq!(
            pvd_core::Recording::from_bytes(&bytes)
                .unwrap()
                .frame_count(),
            8
        );
        let identity = Identity::from_digest(blake3::hash(&bytes), bytes.len() as u64, 8);
        let cancelled = AtomicBool::new(false);
        for phase in [TrimPhase::Inspect, TrimPhase::Export] {
            let error = export(
                &mut Cursor::new(&bytes),
                &destination,
                (2, 5),
                Some(&identity),
                &cancelled,
                |progress| !(progress.phase == phase && progress.bytes_read > 28),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        }
        let stale = Identity::from_digest(blake3::hash(b"replaced source"), bytes.len() as u64, 8);
        let error = export(
            &mut Cursor::new(&bytes),
            &destination,
            (2, 5),
            Some(&stale),
            &cancelled,
            |_| true,
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed since it was opened"));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        cancelled.store(true, Ordering::Relaxed);
        let error = export(
            &mut Cursor::new(&bytes),
            &destination,
            (2, 5),
            Some(&identity),
            &cancelled,
            |_| true,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        cancelled.store(false, Ordering::Relaxed);
        export(
            &mut Cursor::new(&bytes),
            &destination,
            (2, 5),
            Some(&identity),
            &cancelled,
            |_| true,
        )
        .unwrap();
        let output = fs::read(&destination).unwrap();
        assert_eq!(
            pvd_core::Recording::from_bytes(&output)
                .unwrap()
                .frame_count(),
            4
        );
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        assert_eq!(
            export(
                &mut Cursor::new(&bytes),
                &destination,
                (0, 0),
                None,
                &cancelled,
                |_| true
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&destination).unwrap(), output);
        fs::remove_file(&destination).unwrap();
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    #[ignore = "requires PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY constructed fixtures"]
    fn constructed_native_memory_totals_partial_history_does_not_offer_full_statistics() {
        let directory = PathBuf::from(
            std::env::var_os("PVD_NATIVE_MEMORY_FIXTURE_DIRECTORY")
                .expect("constructed memory fixture directory"),
        );
        let recording = Arc::new(
            pvd_core::Recording::from_bytes(
                &fs::read(directory.join("memory-initial-zero.pxd2")).unwrap(),
            )
            .unwrap(),
        );
        for language in [
            crate::i18n::Language::English,
            crate::i18n::Language::Chinese,
        ] {
            let mut studio = crate::studio::Studio::default();
            studio.persist_layout = false;
            studio.prefs.language = language;
            studio.recording = Some(recording.clone());
            assert!(studio.can_export_analysis());
            let context = egui::Context::default();
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 450.0),
                    )),
                    ..Default::default()
                },
                |context| {
                    egui::CentralPanel::default().show(context, |ui| {
                        analysis_controls(ui, &mut studio, AnalysisCsvKind::Memory, false)
                    });
                },
            );
            let text: Vec<_> = output
                .shapes
                .into_iter()
                .filter_map(|shape| match shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            assert!(
                text.iter().any(|label| label
                    == language.text("Full memory CSV unavailable", "完整内存统计不可用")),
                "{text:?}"
            );
            assert!(
                text.iter().any(|label| label.contains(language.text(
                    "Earlier memory events are unavailable",
                    "缺少起点前内存事件"
                ))),
                "{text:?}"
            );
            assert!(
                !text
                    .iter()
                    .any(|label| label
                        == language.text("Export all memory samples", "导出全部内存汇总")),
                "{text:?}"
            );
            assert!(studio.recording_export.is_none());
        }
    }
}
