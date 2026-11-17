use super::*;

const CLEANUP_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct Progress {
    started: Option<Instant>,
    cleanup_started: Option<Instant>,
    pending_release: Option<Box<Studio>>,
    exports_pending: bool,
    pub(super) context: Option<egui::Context>,
    ui_cache: Option<egui::util::IdTypeMap>,
    #[cfg(test)]
    pub(super) drop_probe: Option<shutdown_tests::DropProbe>,
}

impl Studio {
    pub fn poll_shutdown(&mut self) -> bool {
        self.poll_shutdown_at(Instant::now())
    }

    pub(super) fn poll_shutdown_at(&mut self, now: Instant) -> bool {
        if !self.shutdown_started {
            // Keep only the small UI shell. Moving the entire old Studio also
            // moves hidden owners in profiles, scene caches and retained takes.
            // Their final destructors must not run on the window's event loop.
            let prefs = std::mem::take(&mut self.prefs);
            // Hidden profile panels retain analysis/results in egui's data map,
            // outside Studio. Detach that map without traversing or dropping it
            // on the UI thread; the exit overlay rebuilds its own small state.
            self.shutdown.ui_cache = self
                .shutdown
                .context
                .as_ref()
                .map(|context| context.data_mut(std::mem::take));
            let mut shell = Self::with_preferences(prefs, self.persist_layout);
            shell.last_layout = std::mem::take(&mut self.last_layout);
            shell.last_layout_save = self.last_layout_save;
            shell.background_jobs = self.background_jobs.clone();
            shell.export_workers = self.export_workers.clone();
            shell.display_trace = self.display_trace.take();
            shell.display_request = self.display_request;
            shell.profile_view_epoch = self.profile_view_epoch.wrapping_add(1);
            shell.revision = self.revision.wrapping_add(1);
            shell.initialized = true;
            shell.shutdown_started = true;
            shell.exit_requested = true;
            shell.shutdown.started = Some(now);
            self.persist_layout = false;
            let retired = Box::new(std::mem::replace(self, shell));
            self.shutdown.pending_release = Some(retired);
        }
        self.background_jobs.cancel_all_and_check_finished();
        self.shutdown.exports_pending = !self.export_workers.cancel_all_and_check_finished();
        if let Some(retired) = self.shutdown.pending_release.take()
            && let Err((_error, retired)) = self.background_jobs.retire(retired)
        {
            // A failed OS thread allocation must not fall back to a synchronous
            // destructor. Keep the owner and try again on a later UI update.
            self.shutdown.pending_release = Some(retired);
        }
        if self.shutdown.pending_release.is_some()
            || self.shutdown.exports_pending
            || self.background_jobs.active_work_count() != 0
        {
            self.shutdown.cleanup_started = None;
            return false;
        }
        if self.background_jobs.active_count() == 0
            && pvd_core::live::pending_history_cleanups() == 0
        {
            return true;
        }
        // Only retired owners and temporary-file cleanup remain. The old
        // Studio's preference writer was disabled before retirement, and all
        // exports, input reception and readers have finished.
        // Let the OS close remaining handles on process exit instead of making
        // slow temporary-file deletion hold the window indefinitely. Cache
        // ownership markers let the next recording reclaim any remainder.
        let cleanup_started = *self.shutdown.cleanup_started.get_or_insert(now);
        now.saturating_duration_since(cleanup_started) >= CLEANUP_GRACE
    }

    pub fn shutdown_elapsed(&self) -> Duration {
        self.shutdown
            .started
            .map_or(Duration::ZERO, |time| time.elapsed())
    }

    pub fn shutdown_phases(&self) -> Vec<&'static str> {
        let language = self.prefs.language;
        let mut phases = Vec::new();
        if self.shutdown.exports_pending {
            phases.push(language.text(
                "Finishing recording export cancellation",
                "正在完成录制导出的取消",
            ));
        }
        for name in self.background_jobs.active_names() {
            let phase = match name.as_str() {
                "pvd-live-indexer" => {
                    language.text("Stopping reception and recording", "正在停止接收与录制")
                }
                "pvd-live-cleanup" => {
                    language.text("Cleaning up receive buffers", "正在清理接收缓存")
                }
                "pvd-file-loader" => language.text("Stopping file loading", "正在停止文件读取"),
                "pvd-history-seek" => {
                    language.text("Cancelling history reads", "正在取消历史帧读取")
                }
                "pvd-studio-release" => language.text(
                    "Releasing scenes and recording memory",
                    "正在释放场景与录制内存",
                ),
                "cpu-index-cleanup" => {
                    language.text("Releasing performance indexes", "正在释放性能索引")
                }
                "save-clip-definitions" | "curve-export" | "repx-export" => {
                    language.text("Finishing file writes", "正在完成文件写入")
                }
                _ => language.text("Stopping background analysis", "正在停止后台分析"),
            };
            if !phases.contains(&phase) {
                phases.push(phase);
            }
        }
        if self.shutdown.pending_release.is_some() {
            phases.push(language.text("Retrying resource release", "正在重试后台资源释放"));
        }
        if pvd_core::live::pending_history_cleanups() != 0 {
            phases.push(language.text(
                "Cleaning up temporary recording files",
                "正在清理临时录制文件",
            ));
        }
        if phases.is_empty() {
            phases.push(language.text("Finishing shutdown", "正在完成退出"));
        }
        phases
    }

    pub fn shutdown_cleanup_pending(&self) -> bool {
        self.shutdown.cleanup_started.is_some()
    }
}
