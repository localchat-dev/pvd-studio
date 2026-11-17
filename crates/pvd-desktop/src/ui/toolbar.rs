use super::*;

pub(super) fn show(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    navmesh: &mut crate::navmesh::NavMeshOverlay,
) {
    let language = studio.prefs.language;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("PVD STUDIO").size(17.0).strong());
        ui.add_space(8.0);
        ui.add(
            egui::TextEdit::singleline(&mut studio.path_input)
                .hint_text(language.text("Open or drop a capture", "打开或拖入录制文件"))
                .desired_width((ui.available_width() - 520.0).max(120.0)),
        );
        if ui.button(language.text("Open", "打开")).clicked() {
            if studio.path_input.trim().is_empty() {
                studio.pick_file(PickerKind::OpenCapture, ui.ctx().clone());
            } else {
                let path = PathBuf::from(&studio.path_input);
                studio.request_ui_action(ui.ctx(), "toolbar-open", UiAction::Open(path));
            }
        }
        if ui
            .add_enabled(!studio.file_picker.is_busy(), egui::Button::new("…"))
            .on_hover_text(language.text("Browse captures", "选择录制文件"))
            .clicked()
        {
            studio.pick_file(PickerKind::OpenCapture, ui.ctx().clone());
        }
        if ui
            .add_enabled(
                studio.recording.is_some() || studio.bytes.is_some() || studio.live_connected,
                egui::Button::new(language.text("Close", "关闭")),
            )
            .on_hover_text(if studio.temporary_recordings.selected.is_some() {
                language.text(
                    "Close this view; keep the temporary recording and any active reception.",
                    "关闭当前查看；保留临时录制，正在进行的接收继续。",
                )
            } else if studio.live_connected {
                language.text(
                    "Stop this connection and retain its completed frames as a temporary recording.",
                    "停止当前连接，将已有完整帧保留为临时录制。",
                )
            } else {
                language.text("Close the current recording view.", "关闭当前录制视图。")
            })
            .clicked()
        {
            studio.request_ui_action(ui.ctx(), "toolbar-close", UiAction::Close);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            help::menu(ui, language);
            settings_menu(ui, language.text("Settings", "设置"), |ui| {
                egui::ScrollArea::vertical()
                    .max_height(600.0)
                    .show(ui, |ui| {
                        settings(ui, studio);
                    });
            });
            if studio.server.is_none() {
                if ui
                    .add_enabled(
                        !studio.file_picker.is_busy(),
                        egui::Button::new(language.text("Listen", "开始监听"))
                            .fill(egui::Color32::from_rgb(32, 67, 76)),
                    )
                    .on_disabled_hover_text(language.text(
                        "Finish or cancel the file dialog first.",
                        "请先完成或取消文件选择。",
                    ))
                    .clicked()
                {
                    studio.request_ui_action(ui.ctx(), "toolbar-listen", UiAction::StartListener);
                }
            } else if ui
                .button(language.text("Stop listener", "停止监听"))
                .clicked()
            {
                studio.request_ui_action(ui.ctx(), "toolbar-stop-listener", UiAction::StopListener);
            }
            connection(ui, studio);
        });
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        playback(ui, studio);
        frame_sources::show(ui, studio);
        ui.separator();
        if ui.button("NavMesh").clicked() {
            navmesh.open = !navmesh.open;
        }
        let source = studio.source_text(language);
        let name = std::path::Path::new(source)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(source);
        ui.add(egui::Label::new(egui::RichText::new(name).small().color(MUTED)).truncate())
            .on_hover_text(source);
    });
    ui.add_space(4.0);
}

fn connection(ui: &mut egui::Ui, studio: &Studio) {
    let language = studio.prefs.language;
    let label = if studio
        .server
        .as_ref()
        .is_some_and(|server| server.is_stopping())
    {
        language.text("TCP · Stopping", "TCP · 正在停止")
    } else if studio
        .receive_status
        .as_ref()
        .is_some_and(|status| status.last_failure.is_some())
    {
        language.text("TCP · Receive error", "TCP · 接收异常")
    } else if studio.receive_backlog_warning() {
        language.text("TCP · Backlog", "TCP · 接收积压")
    } else if studio
        .receive_status
        .as_ref()
        .is_some_and(|status| status.connected)
    {
        if studio
            .receive_status
            .as_ref()
            .is_some_and(|status| status.received_bytes == 0)
        {
            language.text("TCP · Waiting for data", "TCP · 等待数据")
        } else {
            language.text("TCP · Receiving", "TCP · 接收中")
        }
    } else if studio.server.is_some() {
        language.text("TCP · Listening", "TCP · 监听中")
    } else {
        language.text("TCP · Off", "TCP · 未监听")
    };
    let mut detail = studio.connection.text(language).to_owned();
    if let Some(summary) = studio.receive_summary(language) {
        detail.push('\n');
        detail.push_str(&summary);
        detail.push('\n');
        detail.push_str(&studio.receive_detail(language));
    }
    let color = if studio
        .receive_status
        .as_ref()
        .is_some_and(|status| status.last_failure.is_some())
        || studio.receive_backlog_warning()
    {
        receive_status_color(studio)
    } else if studio.server.is_some() {
        ACCENT
    } else {
        MUTED
    };
    ui.add(egui::Label::new(egui::RichText::new(label).small().color(color)).truncate())
        .on_hover_text(detail);
}

fn settings(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    ui.set_min_width(360.0);
    ui.add_enabled_ui(studio.server.is_none(), |ui| {
        ui.strong(language.text("TCP connection", "TCP 连接"));
        ui.horizontal(|ui| {
            ui.label(language.text("Address", "地址"));
            ui.add(egui::TextEdit::singleline(&mut studio.prefs.listener.host).desired_width(160.0))
                .on_hover_text(language.text(
                    "0.0.0.0 listens on all local IPv4 interfaces. Remote clients use this computer's reachable IP address.",
                    "0.0.0.0 监听所有本机 IPv4 网卡。远端连接时填写本机可达的 IP 地址。",
                ));
            ui.label(language.text("Port", "端口"));
            ui.add(egui::DragValue::new(&mut studio.prefs.listener.port).range(1..=65535));
        });
        ui.separator();
        ui.strong(language.text("Recording and playback", "录制与回放"));
        ui.add(egui::TextEdit::singleline(&mut studio.prefs.history_directory).desired_width(340.0))
            .on_hover_text(language.text("Temporary recording and playback directory", "临时录制与回放目录"));
        ui.horizontal(|ui| {
            ui.label(language.text("Playback cache", "回放缓存预算"));
            ui.add(egui::DragValue::new(&mut studio.prefs.history_budget_gib).range(1..=1024).suffix(" GiB"))
                .on_hover_text(language.text(
                    "Limits each replay cache. Earlier recording data remains on disk and is loaded when needed; reaching this limit does not discard the start of an active recording.",
                    "限制回放缓存大小。更早的录制数据保留在磁盘，查看时按需读取；达到此预算不会删除活动录制的开头。",
                ));
            ui.label(language.text("Refresh", "刷新"));
            ui.add(egui::DragValue::new(&mut studio.prefs.live_refresh_hz).range(15..=120).suffix(" Hz"));
        });
        ui.horizontal(|ui| {
            ui.label(language.text("Ended recordings", "已结束录制预算"));
            ui.add(egui::DragValue::new(&mut studio.prefs.temporary_budget_gib).range(1..=1024).suffix(" GiB"))
                .on_hover_text(language.text("Default: 8 GiB for ended temporary recordings only.", "默认 8 GiB，仅用于已结束的临时录制。"));
            ui.label(language.text("Count", "数量"));
            ui.add(egui::DragValue::new(&mut studio.prefs.temporary_max_count).range(1..=256))
                .on_hover_text(language.text("Default: 16 ended temporary recordings.", "默认保留 16 条已结束的临时录制。"));
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(language.text(
                    "Active recordings retain their first frame while disk space allows. Cleanup removes the oldest ended temporary recordings; the newest recording and recordings being viewed or saved are protected, even above the cleanup target. Temporary files are cleared on normal exit. Save recordings to keep them; saved files are not cleaned up.",
                    "磁盘空间允许时，活动录制始终保留首帧。超预算时清理最早结束的临时录制；最新一条及查看或保存中的内容继续保留，因此总量可能超出预算。正常退出会清理临时文件；需要保留的录制请保存，已保存文件不参与清理。",
                ))
                .small()
                .color(MUTED),
            )
            .wrap(),
        );
        receive_storage_settings(ui, &mut studio.prefs);
        ui.separator();
        ui.checkbox(&mut studio.prefs.auto_capture, language.text("Save incoming recordings", "自动保存录制"));
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut studio.prefs.capture_directory).desired_width(240.0))
                .on_hover_text(language.text("Recording directory", "录制目录"));
            if ui.add_enabled(!studio.file_picker.is_busy(), egui::Button::new(language.text("Browse…", "选择…"))).clicked() {
                studio.pick_file(PickerKind::RecordingDirectory, ui.ctx().clone());
                ui.close();
            }
        });
        ui.horizontal(|ui| {
            ui.label(language.text("File limit", "文件上限"));
            ui.add(egui::DragValue::new(&mut studio.prefs.raw_capture_limit_gib).range(0..=16384).suffix(" GiB"))
                .on_hover_text(language.text(
                    "0 means unlimited. A quota or write failure stops the recording file; live reception continues. Each connection uses a new .pvdstream file. Incomplete recordings remain .partial.",
                    "0 表示不限容量。达到上限或写入失败时停止保存文件，实时接收继续。每次连接生成新的 .pvdstream 文件，不完整的录制保留为 .partial。",
                ));
        });
    });
    if studio.server.is_some() {
        ui.label(
            egui::RichText::new(language.text(
                "Stop listening to edit connection and recording settings.",
                "停止监听后可修改连接和录制设置。",
            ))
            .small()
            .color(MUTED),
        );
    }
    if let Some(path) = &studio.capture_path {
        ui.add(egui::Label::new(path.display().to_string()).truncate())
            .on_hover_text(path.display().to_string());
    }
    ui.separator();
    ui.menu_button(language.text("Panels", "面板"), |ui| {
        for panel in Panel::ALL {
            let mut visible = studio.prefs.dock.find_tab(&panel).is_some();
            let response = ui.add_enabled(
                panel.is_closeable(),
                egui::Checkbox::new(&mut visible, panel.label(language)),
            );
            if response.changed() {
                if visible {
                    studio.prefs.show_panel(panel);
                } else {
                    studio.prefs.hide_panel(panel);
                }
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(language.text("Language", "语言"));
        egui::ComboBox::from_id_salt("interface-language")
            .selected_text(studio.prefs.language.label())
            .show_ui(ui, |ui| {
                for option in [Language::Chinese, Language::English] {
                    ui.selectable_value(&mut studio.prefs.language, option, option.label());
                }
            });
        if ui
            .button(language.text("Reset layout", "重置布局"))
            .clicked()
        {
            studio.prefs.dock = Preferences::default().dock;
        }
    });
}
