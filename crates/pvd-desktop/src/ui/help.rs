use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Controls,
    About,
    Report,
}

fn page_id() -> egui::Id {
    egui::Id::new("application-help-page")
}

fn open(context: &egui::Context, page: Page) {
    context.data_mut(|data| data.insert_temp(page_id(), page));
    context.request_repaint();
}

pub(super) fn menu(ui: &mut egui::Ui, language: Language) {
    settings_menu(ui, language.text("Help", "帮助"), |ui| {
        for (page, label) in [
            (Page::Controls, language.text("Controls", "操作说明")),
            (
                Page::About,
                language.text("About pvd-studio", "关于 pvd-studio"),
            ),
            (Page::Report, language.text("Report a problem", "问题反馈")),
        ] {
            let button = egui::Button::new(label);
            let button = if page == Page::Controls {
                button.shortcut_text("F1")
            } else {
                button
            };
            if ui.add(button).clicked() {
                open(ui.ctx(), page);
                ui.close();
            }
        }
    });
}

pub(super) fn show(context: &egui::Context, studio: &Studio) -> bool {
    if context
        .input_mut(|input| input.focused && input.consume_key(egui::Modifiers::NONE, egui::Key::F1))
    {
        open(context, Page::Controls);
    }
    let Some(page) = context.data(|data| data.get_temp::<Page>(page_id())) else {
        return false;
    };
    crate::camera_controller::cancel_gesture(context);
    let language = studio.prefs.language;
    let modal = egui::Modal::new(egui::Id::new("application-help")).show(context, |ui| {
        ui.set_max_width(560.0);
        ui.set_min_width(400.0);
        ui.heading(match page {
            Page::Controls => language.text("Controls", "操作说明"),
            Page::About => language.text("About pvd-studio", "关于 pvd-studio"),
            Page::Report => language.text("Report a problem", "问题反馈"),
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height((context.content_rect().height() - 180.0).clamp(120.0, 480.0))
            .show(ui, |ui| match page {
                Page::Controls => controls(ui, language),
                Page::About => about(ui, language),
                Page::Report => report(ui, studio),
            });
        ui.separator();
        ui.button(language.text("Close", "关闭")).clicked()
    });
    if modal.inner || modal.should_close() {
        context.data_mut(|data| data.remove::<Page>(page_id()));
        context.request_repaint();
    }
    true
}

fn controls(ui: &mut egui::Ui, language: Language) {
    ui.strong(language.text("Playback", "回放"));
    egui::Grid::new("playback-controls").show(ui, |ui| {
        for (key, action) in [
            ("Space", language.text("Play / pause", "播放 / 暂停")),
            (
                "← / →",
                language.text("Previous / next frame", "上一帧 / 下一帧"),
            ),
            (
                "F",
                language.text("Focus the selected object", "聚焦选中对象"),
            ),
            ("F1", language.text("Open these controls", "打开操作说明")),
        ] {
            ui.monospace(key);
            ui.label(action);
            ui.end_row();
        }
    });
    ui.label(language.text(
        "For slow motion, pause live follow, turn off Recorded timing, then lower FPS and play forward or reverse. Pausing keeps TCP reception running. Use Return to live to follow incoming frames.",
        "慢放：暂停实时跟随，关闭“录制速度”，降低帧率后正放或倒放。暂停时 TCP 继续接收；点击“返回实时”跟随新帧。",
    ));
    ui.add_space(8.0);
    ui.strong(language.text("Camera", "相机"));
    ui.label(crate::camera_controller::CameraMode::Orbit.help(language));
    ui.label(crate::camera_controller::CameraMode::Fly.help(language));
    ui.label(language.text(
        "Click geometry to select it; double-click to focus. Shift+click in the scene or object list toggles individual objects in the selection. F focuses the selection; properties and Follow use the primary object. Choose Orbit or First person in Display. Recorded cameras must be switched to Free camera before navigation.",
        "单击几何选择，双击聚焦。Shift+单击场景或对象列表可增减所选对象。F 聚焦全部所选对象；属性和跟随使用当前对象。在“显示”中选择轨道或第一人称相机；使用录制相机时，先切换到自由视角才能移动。",
    ));
    ui.add_space(8.0);
    ui.strong(language.text("Objects and curves", "对象与曲线"));
    ui.label(language.text(
        "Enter a decimal or 0x hexadecimal ID in object search and press Enter to select it. Right-click a property or expanded array element to copy it or plot a numeric component.",
        "在对象搜索中输入十进制或 0x 十六进制 ID，按 Enter 选择。右键属性或已展开的数组元素，可复制或绘制数值分量曲线。",
    ));
    ui.label(language.text(
        "Drag the plot to pan, scroll to zoom, and click to jump to a frame. Right-click the plot to export the selected curve as CSV.",
        "拖动曲线图平移，滚轮缩放，单击跳帧。右键图表可将选中的曲线导出为 CSV。",
    ));
    ui.add_space(8.0);
    ui.strong(language.text("Capture and storage", "连接与保存"));
    ui.label(language.text(
        "Configure TCP in Settings, then Listen. The default port is 5425. The sender connects to this computer's reachable IP address.",
        "在“设置”中配置 TCP 后开始监听，默认端口 5425。发送端填写调试机可达的 IP 地址。",
    ));
    ui.label(language.text(
        "Save capture preserves the available playback history. Enable Save incoming recordings before listening when a complete raw stream is needed. History capacity and recording directories are in Settings.",
        "“保存录制”保存当前可读取的历史。需要完整原始流时，在监听前启用“自动保存录制”。历史容量和录制目录均在“设置”中配置。",
    ));
    ui.add_space(8.0);
    ui.strong(language.text("Panels", "面板"));
    ui.label(language.text(
        "Drag a tab to reorder, split or float it inside the window. Right-click a tab to close it. Restore panels in Settings > Panels, or use Reset layout. The Scene panel stays available.",
        "拖动标签可排序、分栏或在窗口内浮动。右键标签可关闭面板，在“设置 → 面板”中重新打开，或使用“重置布局”。场景面板始终保留。",
    ));
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "Debug"
    } else {
        "Release"
    }
}

fn about(ui: &mut egui::Ui, language: Language) {
    ui.strong("pvd-studio");
    egui::Grid::new("application-version").show(ui, |ui| {
        for (label, value) in [
            (language.text("Version", "版本"), env!("CARGO_PKG_VERSION")),
            (language.text("Build", "构建"), build_profile()),
            (language.text("Platform", "平台"), std::env::consts::OS),
            (
                language.text("Architecture", "架构"),
                std::env::consts::ARCH,
            ),
        ] {
            ui.label(label);
            ui.monospace(value);
            ui.end_row();
        }
    });
    ui.add_space(8.0);
    ui.label(language.text(
        "PhysX capture, playback and scene inspection. Displayed state comes from the recording; the viewer does not run a physics simulation.",
        "PhysX 数据捕获、回放与场景检查工具。显示状态来自录制，查看器不运行物理模拟。",
    ));
    ui.label(language.text(
        "Usage and compatibility details are included with the application:",
        "完整用法和兼容范围见发行包中的文档：",
    ));
    ui.monospace("docs/distribution.md");
    ui.monospace("docs/compatibility.md");
}

fn report_text(studio: &Studio) -> String {
    let language = studio.prefs.language;
    format!(
        "pvd-studio {}\nBuild: {}\nPlatform: {} {}\nFrames: {}\nCurrent frame: {}\nObjects: {}\n\n{}\n\n{}\n\n{}\n",
        env!("CARGO_PKG_VERSION"),
        build_profile(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        studio.count().saturating_sub(studio.first_frame()),
        studio.frame,
        studio.state.objects.len(),
        language.text("Steps to reproduce:", "复现步骤："),
        language.text("Expected result:", "预期结果："),
        language.text("Actual result:", "实际结果："),
    )
}

fn report(ui: &mut egui::Ui, studio: &Studio) {
    let language = studio.prefs.language;
    ui.label(language.text(
        "Copy the report template, fill in the steps to reproduce, expected result and actual result, then send it to the project maintainer.",
        "复制反馈模板，填写复现步骤、预期结果和实际结果后交给项目维护者。",
    ));
    ui.label(language.text(
        "For TCP or playback issues, copy timing JSON from Diagnostics after starting with --live-profile. After a crash, check for logs in:",
        "TCP 或回放问题可使用 --live-profile 启动，复现后在“诊断”复制计时 JSON。程序崩溃后可在此目录检查日志：",
    ));
    ui.monospace("%LOCALAPPDATA%/pvd-studio/diagnostics/");
    if ui
        .button(language.text("Copy report template", "复制反馈模板"))
        .clicked()
    {
        ui.ctx().copy_text(report_text(studio));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn help_shortcut_and_escape_preserve_live_display_and_block_scene_input() {
        for language in [Language::Chinese, Language::English] {
            let context = egui::Context::default();
            configure(&context);
            let mut studio = Studio::default();
            studio.prefs.language = language;
            studio.frame = 17;
            studio.follow_live = true;
            studio.live_connected = true;
            studio.playing = -1;
            let draw = |events| {
                let mut visible = false;
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(800.0, 600.0),
                        )),
                        events,
                        focused: true,
                        ..Default::default()
                    },
                    |context| {
                        crate::camera_controller::begin_frame(context);
                        visible = show(context, &studio);
                        if visible {
                            assert!(crate::camera_controller::keyboard_input_blocked(context));
                        }
                    },
                );
                visible
            };
            assert!(!draw(vec![]));
            context.memory_mut(|memory| memory.request_focus(egui::Id::new("capture-path-editor")));
            assert!(draw(vec![key(egui::Key::F1)]));
            assert!(draw(vec![]));
            assert!(draw(vec![key(egui::Key::Escape)]));
            assert!(!draw(vec![]));
            assert_eq!(studio.frame, 17);
            assert!(studio.follow_live && studio.live_connected);
            assert_eq!(studio.playing, -1);
        }
    }

    #[test]
    fn problem_report_has_version_and_display_context_without_source_paths() {
        let mut studio = Studio::default();
        studio.path_input = "C:/private-captures/customer-session.pvdstream".into();
        studio.frame = 17;
        for language in [Language::Chinese, Language::English] {
            studio.prefs.language = language;
            let report = report_text(&studio);
            assert!(report.contains(env!("CARGO_PKG_VERSION")));
            assert!(report.contains("Current frame: 17"));
            assert!(report.contains(language.text("Steps to reproduce:", "复现步骤：")));
            assert!(!report.contains("private-captures"));
            assert!(!report.contains("customer-session"));
        }
    }
}
