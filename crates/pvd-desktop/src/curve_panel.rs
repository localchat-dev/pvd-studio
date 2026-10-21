use crate::{
    curve::{Curve, MAX_PAGES},
    file_picker::PickerKind,
    i18n::Language,
    studio::Studio,
};
use bevy_egui::egui;
use std::{collections::BTreeMap, sync::atomic::Ordering};

const MUTED: egui::Color32 = egui::Color32::from_rgb(151, 167, 184);
const MAX_MARKERS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Bounds {
    x: [f64; 2],
    y: [f64; 2],
}

#[derive(Clone)]
struct Marker {
    curve: u64,
    frame: usize,
}

#[derive(Clone, Default)]
struct View {
    bounds: Option<Bounds>,
    auto_fit: bool,
    box_zoom: bool,
    box_start: Option<egui::Pos2>,
    markers: Vec<Marker>,
}

#[derive(Clone, Default)]
struct Views {
    epoch: u64,
    pages: BTreeMap<u64, View>,
}

fn fraction(value: f64, range: [f64; 2]) -> f64 {
    let span = range[1] - range[0];
    if span.is_finite() {
        (value - range[0]) / span
    } else {
        (value * 0.5 - range[0] * 0.5) / (range[1] * 0.5 - range[0] * 0.5)
    }
}

fn interpolate(range: [f64; 2], amount: f64) -> f64 {
    range[0] * (1.0 - amount) + range[1] * amount
}

fn valid_bounds(bounds: Bounds) -> bool {
    bounds.x.iter().chain(&bounds.y).all(|v| v.is_finite())
        && bounds.x[1] > bounds.x[0]
        && bounds.y[1] > bounds.y[0]
}

fn fit(curves: &[&Curve]) -> Bounds {
    let mut x_max = 1.0f64;
    let mut y = None::<[f64; 2]>;
    for curve in curves {
        if let Some(data) = &curve.data {
            x_max = x_max.max(data.source_frames.saturating_sub(1) as f64);
            for (_, value) in &data.samples {
                if let Some(value) = value {
                    let range = y.get_or_insert([*value, *value]);
                    range[0] = range[0].min(*value);
                    range[1] = range[1].max(*value);
                }
            }
        }
    }
    let mut y = y.unwrap_or([0.0, 1.0]);
    if y[0] == y[1] {
        let padding = (y[0].abs() * 0.05).max(1e-6);
        y = [
            (y[0] - padding).max(-f64::MAX),
            (y[1] + padding).min(f64::MAX),
        ];
    } else {
        let padding = (y[1] * 0.5 - y[0] * 0.5) * 0.1;
        y = [
            (y[0] - padding).max(-f64::MAX),
            (y[1] + padding).min(f64::MAX),
        ];
    }
    Bounds { x: [0.0, x_max], y }
}

fn screen(rect: egui::Rect, bounds: Bounds, frame: f64, value: f64) -> egui::Pos2 {
    egui::pos2(
        rect.left() + rect.width() * fraction(frame, bounds.x) as f32,
        rect.bottom() - rect.height() * fraction(value, bounds.y) as f32,
    )
}

fn data_position(rect: egui::Rect, bounds: Bounds, point: egui::Pos2) -> [f64; 2] {
    [
        interpolate(
            bounds.x,
            ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64,
        ),
        interpolate(
            bounds.y,
            ((rect.bottom() - point.y) / rect.height()).clamp(0.0, 1.0) as f64,
        ),
    ]
}

fn nearest_sample(curve: &Curve, frame: f64) -> Option<(usize, f64)> {
    let samples = &curve.data.as_ref()?.samples;
    let next = samples.partition_point(|(frame_index, _)| (*frame_index as f64) < frame);
    [next.checked_sub(1), (next < samples.len()).then_some(next)]
        .into_iter()
        .flatten()
        .filter_map(|index| {
            let (frame_index, value) = samples[index];
            value.map(|value| (frame_index, value))
        })
        .min_by(|a, b| {
            ((a.0 as f64) - frame)
                .abs()
                .total_cmp(&((b.0 as f64) - frame).abs())
        })
}

fn gesture(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    view: &mut View,
    fitted: Bounds,
) -> Option<f64> {
    if ui.ctx().wants_keyboard_input() || rect.width() < 1.0 || rect.height() < 1.0 {
        return None;
    }
    let mut bounds = view.bounds.unwrap_or(fitted);
    let rectangular = view.box_zoom || ui.input(|input| input.modifiers.shift);
    if response.drag_started() && rectangular {
        view.box_start = ui
            .input(|input| input.pointer.press_origin())
            .map(|point| point.clamp(rect.min, rect.max));
    }
    if response.dragged() && view.box_start.is_none() {
        let delta = ui.input(|input| input.pointer.delta());
        let x_offset = -(delta.x as f64) / rect.width() as f64;
        let y_offset = delta.y as f64 / rect.height() as f64;
        bounds = Bounds {
            x: [
                interpolate(bounds.x, x_offset),
                interpolate(bounds.x, 1.0 + x_offset),
            ],
            y: [
                interpolate(bounds.y, y_offset),
                interpolate(bounds.y, 1.0 + y_offset),
            ],
        };
        view.auto_fit = false;
    }
    if response.drag_stopped()
        && let Some(start) = view.box_start.take()
        && let Some(end) = response.interact_pointer_pos()
    {
        let end = end.clamp(rect.min, rect.max);
        if (start.x - end.x).abs() >= 4.0 && (start.y - end.y).abs() >= 4.0 {
            let a = data_position(rect, bounds, start);
            let b = data_position(rect, bounds, end);
            bounds = Bounds {
                x: [a[0].min(b[0]), a[0].max(b[0])],
                y: [a[1].min(b[1]), a[1].max(b[1])],
            };
            view.auto_fit = false;
        }
    }
    if response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y) as f64;
        if scroll != 0.0 {
            ui.input_mut(|input| input.smooth_scroll_delta.y = 0.0);
            let anchor = response.hover_pos().unwrap_or(rect.center());
            let point = data_position(rect, bounds, anchor);
            let factor = (-scroll * 0.005).exp().clamp(0.1, 10.0);
            let zoom = |range: [f64; 2], origin: f64| {
                [
                    origin + (range[0] - origin) * factor,
                    origin + (range[1] - origin) * factor,
                ]
            };
            bounds = Bounds {
                x: zoom(bounds.x, point[0]),
                y: zoom(bounds.y, point[1]),
            };
            view.auto_fit = false;
        }
    }
    if valid_bounds(bounds) {
        view.bounds = Some(bounds);
    }
    if response.clicked() && !rectangular {
        response
            .interact_pointer_pos()
            .map(|point| data_position(rect, view.bounds.unwrap_or(fitted), point)[0])
    } else {
        None
    }
}

fn page_name(name: &str, index: usize, language: Language) -> String {
    if name.is_empty() {
        format!("{} {}", language.text("Graph", "图页"), index + 1)
    } else {
        name.to_owned()
    }
}

pub fn show(ui: &mut egui::Ui, studio: &mut Studio) {
    if studio.curves_need_analysis() {
        studio.ensure_current_analysis(ui);
    }
    let content_height = ui.available_height().max(1.0);
    egui::ScrollArea::vertical()
        .id_salt("curve-panel-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), content_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| show_content(ui, studio),
            );
        });
}

fn curve_management_controls(
    ui: &mut egui::Ui,
    studio: &mut Studio,
    selected_page: usize,
    selected: &mut Option<u64>,
    add: &mut Option<u64>,
    remove_member: &mut Option<u64>,
    remove_curve: &mut Option<u64>,
) {
    let language = studio.prefs.language;
    let members = studio.curves.pages[selected_page].curves.clone();
    ui.menu_button(language.text("Curves", "曲线管理"), |ui| {
        for curve in &studio.curves.curves {
            ui.horizontal(|ui| {
                if members.contains(&curve.id) {
                    let mut visible = !studio.curves.pages[selected_page]
                        .hidden
                        .contains(&curve.id);
                    if ui.checkbox(&mut visible, "").changed() {
                        if visible {
                            studio.curves.pages[selected_page].hidden.remove(&curve.id);
                        } else {
                            studio.curves.pages[selected_page].hidden.insert(curve.id);
                        }
                    }
                    if ui
                        .selectable_label(*selected == Some(curve.id), curve.label.text(language))
                        .clicked()
                    {
                        *selected = Some(curve.id);
                    }
                    if ui
                        .small_button(language.text("Detach", "移出图页"))
                        .clicked()
                    {
                        *remove_member = Some(curve.id);
                    }
                } else if ui
                    .button(format!("+ {}", curve.label.text(language)))
                    .clicked()
                {
                    *add = Some(curve.id);
                }
                if ui
                    .small_button(language.text("Delete", "删除曲线"))
                    .clicked()
                {
                    *remove_curve = Some(curve.id);
                }
            });
        }
        ui.label(language.text(
            "Up to 16 curves and 8 graphs per capture. Graphs share curve samples.",
            "每个录制最多 16 条曲线、8 个图页；图页共享曲线采样数据。",
        ));
    });
    if let Some(id) = *selected
        && ui
            .small_button(language.text("Refresh", "重新采样"))
            .clicked()
    {
        studio.refresh_curve(id, ui.ctx().clone());
    }
}

fn show_content(ui: &mut egui::Ui, studio: &mut Studio) {
    let compact = ui.available_height() < 280.0;
    let language = studio.prefs.language;
    if let Some(curve) = studio
        .curves
        .curves
        .iter_mut()
        .find(|curve| Some(curve.id) == studio.curves.selected)
    {
        curve.export_path = studio.curve_export_path.clone();
    }
    let mut selected_page = studio.curves.active_page;
    let mut selected = studio.curves.selected;
    let mut add = None;
    let mut remove_member = None;
    let mut remove_curve = None;
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("curve-page")
            .width(110.0)
            .truncate()
            .selected_text(page_name(
                &studio.curves.pages[selected_page].name,
                selected_page,
                language,
            ))
            .show_ui(ui, |ui| {
                for (index, page) in studio.curves.pages.iter().enumerate() {
                    ui.selectable_value(
                        &mut selected_page,
                        index,
                        page_name(&page.name, index, language),
                    );
                }
            });
        if !compact
            && ui
                .add_enabled(
                    studio.curves.pages.len() < MAX_PAGES,
                    egui::Button::new(language.text("+ Graph", "+ 图页")),
                )
                .clicked()
        {
            studio.curves.new_page();
            selected_page = studio.curves.active_page;
        }
        crate::ui::settings_menu(ui, language.text("Graph settings", "图页设置"), |ui| {
            let page = &mut studio.curves.pages[selected_page];
            ui.add(
                egui::TextEdit::singleline(&mut page.name)
                    .char_limit(80)
                    .hint_text(language.text("Graph name", "图页名称")),
            );
            if ui
                .add_enabled(
                    studio.curves.pages.len() > 1,
                    egui::Button::new(language.text("Remove this graph", "删除当前图页")),
                )
                .clicked()
            {
                studio.curves.active_page = selected_page;
                studio.curves.remove_page();
                selected_page = studio.curves.active_page;
                ui.close();
            }
            if compact {
                if ui
                    .add_enabled(
                        studio.curves.pages.len() < MAX_PAGES,
                        egui::Button::new(language.text("+ Graph", "+ 图页")),
                    )
                    .clicked()
                {
                    studio.curves.new_page();
                    selected_page = studio.curves.active_page;
                    ui.close();
                }
                ui.separator();
                curve_management_controls(
                    ui,
                    studio,
                    selected_page,
                    &mut selected,
                    &mut add,
                    &mut remove_member,
                    &mut remove_curve,
                );
                ui.separator();
                sampling_controls(ui, studio);
                export_controls(ui, studio, true);
            }
        });
        if compact && studio.curve_job.is_some() {
            ui.spinner().on_hover_text(language.text(
                "Sampling curves; progress and cancellation are in Graph settings.",
                "正在采样曲线；进度和取消操作位于图页设置。",
            ));
        } else if !compact {
            sampling_controls(ui, studio);
        }
        studio.curves.active_page = selected_page;
        let page = &studio.curves.pages[selected_page];
        if !page.curves.contains(&selected.unwrap_or(u64::MAX)) {
            selected = page.curves.first().copied();
        }
        let members = studio.curves.pages[selected_page].curves.clone();
        let label = studio
            .curves
            .curves
            .iter()
            .find(|curve| Some(curve.id) == selected)
            .map(|curve| curve.label.text(language))
            .unwrap_or(language.text("Choose a curve", "选择曲线"));
        let selector_width =
            (if compact { 180.0_f32 } else { 240.0_f32 }).min(ui.available_size_before_wrap().x);
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_max_width(selector_width);
            egui::ComboBox::from_id_salt("curve-selected")
                .width(selector_width)
                .truncate()
                .selected_text(label)
                .show_ui(ui, |ui| {
                    ui.set_max_width(520.0);
                    for curve in studio
                        .curves
                        .curves
                        .iter()
                        .filter(|curve| members.contains(&curve.id))
                    {
                        if ui
                            .add(
                                egui::Button::selectable(
                                    selected == Some(curve.id),
                                    curve.label.text(language),
                                )
                                .truncate(),
                            )
                            .on_hover_text(curve.label.text(language))
                            .clicked()
                        {
                            selected = Some(curve.id);
                        }
                    }
                })
                .response
                .on_hover_text(label);
        });
        if !compact {
            curve_management_controls(
                ui,
                studio,
                selected_page,
                &mut selected,
                &mut add,
                &mut remove_member,
                &mut remove_curve,
            );
        }
    });
    let page_id = studio.curves.pages[selected_page].id;
    if selected != studio.curves.selected {
        if let Some(id) = selected {
            studio.select_curve(id);
        } else {
            studio.curves.selected = None;
        }
    }
    if let Some(id) = add {
        studio.curves.pages[selected_page].curves.push(id);
        studio.select_curve(id);
    }
    if let Some(id) = remove_member {
        studio.curves.pages[selected_page]
            .curves
            .retain(|member| *member != id);
    }
    if let Some(id) = remove_curve {
        studio.remove_curve(id);
    }
    let members = &studio.curves.pages[selected_page].curves;
    let hidden = &studio.curves.pages[selected_page].hidden;
    let curves: Vec<_> = studio
        .curves
        .curves
        .iter()
        .filter(|curve| !hidden.contains(&curve.id) && members.contains(&curve.id))
        .collect();
    if studio.curves.curves.is_empty() {
        ui.label(language.text("Choose Plot on an object's numeric property to add a curve to this graph. Different generations never join.", "在对象的数值属性中选择“曲线”，将其添加到当前图页；不同代际不会连接。"));
        return;
    }
    if curves.is_empty() {
        ui.label(language.text(
            "This graph has no visible curves. Choose Curves to add or show one.",
            "当前图页没有可见曲线。请在“曲线管理”中添加或显示曲线。",
        ));
        return;
    }
    let id = ui.id().with("curve-graphs");
    let mut views = ui
        .ctx()
        .data_mut(|data| data.get_temp::<Views>(id))
        .unwrap_or_default();
    if views.epoch != studio.curves.epoch {
        views.pages.clear();
        views.epoch = studio.curves.epoch;
    }
    views
        .pages
        .retain(|id, _| studio.curves.pages.iter().any(|page| page.id == *id));
    let mut view = views.pages.remove(&page_id).unwrap_or(View {
        auto_fit: true,
        ..Default::default()
    });
    let fitted = fit(&curves);
    if view.auto_fit || view.bounds.is_none() {
        view.bounds = Some(fitted);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.small_button(language.text("Fit", "适配全部")).clicked() {
            view.bounds = Some(fitted);
            view.auto_fit = true;
        }
        ui.toggle_value(
            &mut view.box_zoom,
            language.text("Rectangle zoom", "矩形缩放"),
        );
        if !view.markers.is_empty()
            && ui
                .small_button(language.text("Clear markers", "清除标记"))
                .clicked()
        {
            view.markers.clear();
        }
        if let Some(curve) = studio
            .curves
            .curves
            .iter()
            .find(|curve| Some(curve.id) == studio.curves.selected)
        {
            if !compact && let Some(data) = &curve.data {
                let stride_label = if data.reference_frame_count.is_some() {
                    language.text("Scene stride", "Scene 步长")
                } else {
                    language.text("stride", "步长")
                };
                ui.label(
                    egui::RichText::new(format!(
                        "{} {} · {} {} · {} {}",
                        data.samples.len(),
                        language.text("samples", "个采样"),
                        stride_label,
                        data.stride,
                        language.text("through frame", "截止帧"),
                        data.source_frames.saturating_sub(1)
                    ))
                    .small()
                    .color(MUTED),
                );
            }
            if let Some(error) = &curve.error {
                ui.colored_label(egui::Color32::LIGHT_RED, if compact { language.text("Sampling error", "采样失败") } else { error.text(language) }).on_hover_text(error.text(language));
            }
            ui.menu_button(language.text("Details", "详情"), |ui| {
                ui.set_max_width(420.0);
                ui.add(egui::Label::new(curve.label.text(language)).wrap());
                if compact && let Some(data) = &curve.data {
                    ui.label(format!("{} {} · {} {} · {} {}", data.samples.len(), language.text("samples", "个采样"), language.text("stride", "步长"), data.stride, language.text("through frame", "截止帧"), data.source_frames.saturating_sub(1)));
                }
                if let Some(data) = &curve.data
                    && let Some(count) = data.reference_frame_count {
                    ui.add(egui::Label::new(format!("{} {count} · {} {}. {}",
                        language.text("Reference Scene generation frames", "参考 Scene 当前代际帧数"),
                        language.text("sequence stride", "序列步长"), data.stride,
                        language.text("Sampling uses that Scene generation's frame sequence; the horizontal axis and CSV retain global frame indices.", "按该 Scene 代际的帧序列采样；横轴和 CSV 仍使用录制全局帧号。")
                    )).wrap());
                }
                if matches!(curve.spec.source, crate::curve::Source::ProfileEvent(_)) {
                    ui.add(egui::Label::new(language.text("Aligned to this reference Scene's time windows, not Scene ownership. Inclusive sums count nested and overlapping durations separately, not CPU utilization. Missing samples remain gaps; counts and status are available in CSV.", "仅按参考 Scene 时间窗口对齐，不表示 Scene 归属；包含耗时总和会重复累加嵌套和重叠区间，不代表 CPU 占用率。缺失采样保留为空值，次数与状态随 CSV 导出。")).wrap());
                }
                if let Some(diagnostic) = curve.data.as_ref().and_then(|data| data.diagnostics.as_ref()) {
                    ui.add(egui::Label::new(diagnostic.text(language)).wrap());
                }
            });
            if !compact && matches!(curve.spec.source, crate::curve::Source::ProfileEvent(_)) {
                ui.label(egui::RichText::new(language.text("Σ ms · time aligned", "Σ ms · 时间窗对齐")).small().color(MUTED));
            }
        }
    });
    let (whole, _) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width().max(1.0),
            if compact {
                ui.available_height().max(1.0)
            } else {
                (ui.available_height() - 36.0).max(50.0)
            },
        ),
        egui::Sense::hover(),
    );
    let rect = egui::Rect::from_min_max(
        whole.min
            + egui::vec2(
                58.0_f32.min(whole.width() * 0.25),
                6.0_f32.min(whole.height() * 0.15),
            ),
        whole.max
            - egui::vec2(
                8.0_f32.min(whole.width() * 0.1),
                18.0_f32.min(whole.height() * 0.35),
            ),
    );
    ui.painter()
        .rect_filled(rect, 2.0, egui::Color32::from_rgb(16, 23, 31));
    let response = ui.interact(rect, id.with("canvas"), egui::Sense::click_and_drag());
    let jump = gesture(ui, &response, rect, &mut view, fitted);
    let bounds = view.bounds.unwrap_or(fitted);
    for mark in 0..=4 {
        let t = mark as f64 / 4.0;
        let x = rect.left() + rect.width() * t as f32;
        let y = rect.bottom() - rect.height() * t as f32;
        ui.painter().line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(32, 43, 54)),
        );
        ui.painter().text(
            egui::pos2(rect.left() - 5.0, y),
            egui::Align2::RIGHT_CENTER,
            number_label(interpolate(bounds.y, t)),
            egui::FontId::monospace(10.0),
            MUTED,
        );
        ui.painter().text(
            egui::pos2(x, rect.bottom() + 3.0),
            if mark == 0 {
                egui::Align2::LEFT_TOP
            } else if mark == 4 {
                egui::Align2::RIGHT_TOP
            } else {
                egui::Align2::CENTER_TOP
            },
            format!("{:.0}", interpolate(bounds.x, t)),
            egui::FontId::monospace(10.0),
            MUTED,
        );
    }
    let painter = ui.painter().with_clip_rect(rect);
    if !curves.iter().any(|curve| {
        curve
            .data
            .as_ref()
            .is_some_and(|data| data.samples.iter().any(|(_, value)| value.is_some()))
    }) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            language.text(
                "No recorded values in these curves",
                "这些曲线没有可用录制值",
            ),
            egui::FontId::proportional(11.0),
            MUTED,
        );
    }
    for curve in &curves {
        let Some(data) = &curve.data else { continue };
        let first = data
            .samples
            .partition_point(|(frame, _)| (*frame as f64) < bounds.x[0])
            .saturating_sub(1);
        let last = (data
            .samples
            .partition_point(|(frame, _)| (*frame as f64) <= bounds.x[1])
            + 1)
        .min(data.samples.len());
        let mut segment = Vec::new();
        let stroke = egui::Stroke::new(
            if studio.curves.selected == Some(curve.id) {
                2.0
            } else {
                1.2
            },
            color(curve.id),
        );
        for (frame, value) in &data.samples[first..last] {
            if let Some(value) = value {
                let point = screen(rect, bounds, *frame as f64, *value);
                if point.x.is_finite() && point.y.is_finite() {
                    segment.push(point);
                } else {
                    draw_segment(&painter, &mut segment, stroke);
                }
            } else {
                draw_segment(&painter, &mut segment, stroke);
            }
        }
        draw_segment(&painter, &mut segment, stroke);
    }
    let mut seek = None;
    if let Some(frame) = jump {
        seek = Some(frame.max(0.0).round() as usize);
        if let Some(curve) = curves
            .iter()
            .find(|curve| Some(curve.id) == studio.curves.selected)
            && let Some((sample_frame, _)) = nearest_sample(curve, frame)
        {
            if response.double_clicked() {
                seek = Some(sample_frame);
            }
            if !view
                .markers
                .iter()
                .any(|marker| marker.curve == curve.id && marker.frame == sample_frame)
            {
                if view.markers.len() >= MAX_MARKERS {
                    view.markers.remove(0);
                }
                view.markers.push(Marker {
                    curve: curve.id,
                    frame: sample_frame,
                });
            }
        }
    }
    for marker in &view.markers {
        let Some(samples) = curves
            .iter()
            .find(|curve| curve.id == marker.curve)
            .and_then(|curve| curve.data.as_ref())
            .map(|data| &data.samples)
        else {
            continue;
        };
        let Ok(index) = samples.binary_search_by_key(&marker.frame, |(frame, _)| *frame) else {
            continue;
        };
        let Some(value) = samples[index].1 else {
            continue;
        };
        let point = screen(rect, bounds, marker.frame as f64, value);
        if rect.contains(point) {
            painter.circle_filled(point, 3.0, color(marker.curve));
            painter.text(
                point + egui::vec2(5.0, -5.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{}: {}", marker.frame, number_label(value)),
                egui::FontId::monospace(10.0),
                color(marker.curve),
            );
        }
    }
    let current = screen(rect, bounds, studio.frame as f64, bounds.y[0]).x;
    if current >= rect.left() && current <= rect.right() {
        painter.line_segment(
            [
                egui::pos2(current, rect.top()),
                egui::pos2(current, rect.bottom()),
            ],
            egui::Stroke::new(1.0, egui::Color32::WHITE),
        );
    }
    if let Some(start) = view.box_start
        && let Some(end) = response.interact_pointer_pos()
    {
        painter.rect_stroke(
            egui::Rect::from_two_pos(start, end.clamp(rect.min, rect.max)),
            0.0,
            egui::Stroke::new(1.0, egui::Color32::WHITE),
            egui::StrokeKind::Inside,
        );
    }
    let mut legend_x = rect.left() + 4.0;
    for (index, curve) in curves.iter().enumerate() {
        let label = match curve.spec.source {
            crate::curve::Source::Property => format!(
                "{} #{}",
                crate::curve::component_label(&curve.spec.component_name, language),
                curve.id
            ),
            crate::curve::Source::Performance(metric) => {
                use pvd_core::frame_metrics::Metric;
                match metric {
                    Metric::FrameInterval => language.text("Frame interval", "帧间隔"),
                    Metric::Simulate => language.text("Simulate", "模拟"),
                    Metric::Collision => language.text("Collision", "碰撞"),
                    Metric::Solver => language.text("Solver", "求解"),
                }
                .into()
            }
            crate::curve::Source::ProfileEvent(key) => {
                format!("e{} · t{} · #{}", key.event, key.thread, curve.id)
            }
        };
        let text =
            ui.painter()
                .layout_no_wrap(label, egui::FontId::proportional(10.0), color(curve.id));
        let width = text.size().x + 17.0;
        if legend_x + width + 32.0 > rect.right() {
            let remaining = egui::Rect::from_min_size(
                egui::pos2(legend_x, rect.top() + 2.0),
                egui::vec2((rect.right() - legend_x).max(1.0), 13.0),
            );
            painter.rect_filled(remaining, 2.0, egui::Color32::from_rgb(16, 23, 31));
            painter.text(
                remaining.left_center(),
                egui::Align2::LEFT_CENTER,
                format!("+{}", curves.len() - index),
                egui::FontId::proportional(10.0),
                MUTED,
            );
            ui.interact(remaining, id.with("legend-overflow"), egui::Sense::hover())
                .on_hover_ui(|ui| {
                    for curve in &curves[index..] {
                        ui.colored_label(color(curve.id), curve.label.text(language));
                    }
                });
            break;
        }
        let item = egui::Rect::from_min_size(
            egui::pos2(legend_x, rect.top() + 2.0),
            egui::vec2(width, 13.0),
        );
        painter.rect_filled(item, 2.0, egui::Color32::from_rgb(16, 23, 31));
        painter.line_segment(
            [
                item.left_center() + egui::vec2(2.0, 0.0),
                item.left_center() + egui::vec2(10.0, 0.0),
            ],
            egui::Stroke::new(2.0, color(curve.id)),
        );
        painter.galley(item.min + egui::vec2(13.0, 0.0), text, color(curve.id));
        ui.interact(item, id.with(("legend", curve.id)), egui::Sense::hover())
            .on_hover_text(curve.label.text(language));
        legend_x += width + 5.0;
    }
    if let Some(point) = response.hover_pos() {
        let frame = data_position(rect, bounds, point)[0];
        response.on_hover_ui(|ui| {
            ui.label(language.text("Click: jump and mark · Double-click: nearest sample · Drag: pan · Wheel: zoom · Shift+drag: rectangle zoom", "单击：跳帧并标记 · 双击：跳到最近采样 · 拖动：平移 · 滚轮：缩放 · Shift+拖动：矩形缩放"));
            for curve in &curves {
                if let Some((frame, value)) = nearest_sample(curve, frame) {
                    let count = curve.data.as_ref().and_then(|data| data.samples.binary_search_by_key(&frame, |(frame, _)| *frame).ok().and_then(|index| data.occurrence_counts.get(index))).map(|count| format!(" · {} {count}", language.text("occurrences", "次数"))).unwrap_or_default();
                    ui.colored_label(color(curve.id), format!("#{} · {} {} · {}{count}", curve.id, language.text("sample frame", "采样帧"), frame, number_label(value)));
                }
            }
            ui.label(language.text("Markers report recorded samples, not interpolated values. Curves share one value axis.", "标记使用录制采样值，不显示插值结果；曲线共用一个数值轴。"));
        });
    }
    views.pages.insert(page_id, view);
    ui.ctx().data_mut(|data| data.insert_temp(id, views));
    if let Some(frame) = seek {
        studio.request_ui_action(
            ui.ctx(),
            ui.id().with("curve-sample"),
            crate::studio::UiAction::Locate(
                frame.min(studio.count().saturating_sub(1)),
                crate::studio::DisplayReason::Curve,
            ),
        );
    }
    if !compact {
        export_controls(ui, studio, false);
    }
}

fn sampling_controls(ui: &mut egui::Ui, studio: &mut Studio) {
    let language = studio.prefs.language;
    if let Some(job) = &studio.curve_job {
        let fraction =
            job.progress.load(Ordering::Relaxed) as f32 / job.source_frames.max(1) as f32;
        ui.add(egui::ProgressBar::new(fraction).desired_width(70.0));
        ui.label(format!(
            "{} {}",
            studio.curves.pending.len() + 1,
            language.text("sampling", "条待采样")
        ));
        if ui
            .small_button(language.text("Cancel sampling", "取消采样"))
            .clicked()
        {
            studio.cancel_curves();
        }
    } else if !studio.curves.pending.is_empty() {
        ui.label(if studio.playing != 0 && studio.curves_need_analysis() {
            language.text(
                "Pause playback to sample analysis curves.",
                "暂停回放后采样分析曲线。",
            )
        } else {
            language.text("Waiting for analysis…", "等待分析加载…")
        });
        if ui
            .small_button(language.text("Cancel sampling", "取消采样"))
            .clicked()
        {
            studio.cancel_curves();
        }
    }
}

fn export_controls(ui: &mut egui::Ui, studio: &mut Studio, menu: bool) {
    let language = studio.prefs.language;
    let exportable = studio
        .curves
        .curves
        .iter()
        .any(|curve| Some(curve.id) == studio.curves.selected && curve.data.is_some());
    ui.horizontal_wrapped(|ui| {
        ui.add(egui::TextEdit::singleline(&mut studio.curve_export_path).desired_width(180.0));
        if ui
            .add_enabled(
                !studio.file_picker.is_busy(),
                egui::Button::new(language.text("Choose path…", "选择路径…")),
            )
            .clicked()
        {
            studio.pick_file(PickerKind::SaveCsv, ui.ctx().clone());
            if menu {
                ui.close();
            }
        }
        if ui
            .add_enabled(
                exportable && studio.save_job.is_none(),
                egui::Button::new(language.text("Export CSV", "导出 CSV")),
            )
            .clicked()
        {
            studio.export_curve(ui.ctx().clone());
            if menu {
                ui.close();
            }
        }
    });
}

fn draw_segment(painter: &egui::Painter, segment: &mut Vec<egui::Pos2>, stroke: egui::Stroke) {
    if segment.len() == 1 {
        painter.circle_filled(segment[0], 2.0, stroke.color);
    } else if !segment.is_empty() {
        painter.add(egui::Shape::line(std::mem::take(segment), stroke));
    }
    segment.clear();
}

fn number_label(value: f64) -> String {
    if value != 0.0 && (value.abs() >= 1e5 || value.abs() < 0.001) {
        format!("{value:.2e}")
    } else {
        format!("{value:.4}")
    }
}

fn color(id: u64) -> egui::Color32 {
    match id % 6 {
        0 => egui::Color32::from_rgb(205, 158, 225),
        1 => egui::Color32::from_rgb(78, 183, 189),
        2 => egui::Color32::from_rgb(126, 177, 232),
        3 => egui::Color32::from_rgb(235, 186, 101),
        4 => egui::Color32::from_rgb(207, 132, 148),
        _ => egui::Color32::from_rgb(141, 196, 126),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::{Data, Spec};

    fn curve(samples: Vec<(usize, Option<f64>)>) -> Curve {
        let spec = Spec {
            source: crate::curve::Source::Property,
            object: 1,
            generation: 0,
            property: "value".into(),
            component: 0,
            component_name: "value".into(),
        };
        Curve {
            id: 1,
            label: spec.label(),
            spec,
            data: Some(Data {
                samples,
                stride: 1,
                source_frames: 100,
                reference_frame_count: None,
                statuses: Vec::new(),
                occurrence_counts: Vec::new(),
                diagnostics: None,
            }),
            pending: false,
            error: None,
            export_path: "value.csv".into(),
        }
    }

    #[test]
    fn fitted_ranges_handle_constant_missing_and_extreme_finite_values() {
        let constant = curve(vec![(0, Some(3.0)), (1, None), (99, Some(3.0))]);
        let bounds = fit(&[&constant]);
        assert!(valid_bounds(bounds));
        assert_eq!(bounds.x, [0.0, 99.0]);
        assert!(bounds.y[0] < 3.0 && bounds.y[1] > 3.0);
        let extreme = curve(vec![(0, Some(-f64::MAX)), (99, Some(f64::MAX))]);
        let bounds = fit(&[&extreme]);
        assert!(valid_bounds(bounds));
        assert!((fraction(0.0, bounds.y) - 0.5).abs() < 1e-12);
        let missing = curve(vec![(0, None)]);
        assert!(valid_bounds(fit(&[&missing])));
    }

    #[test]
    fn value_markers_use_captured_samples_and_preserve_gaps() {
        let curve = curve(vec![
            (0, Some(1.0)),
            (5, Some(4.0)),
            (10, None),
            (15, None),
            (20, Some(9.0)),
        ]);
        assert_eq!(nearest_sample(&curve, 3.0), Some((5, 4.0)));
        assert_eq!(nearest_sample(&curve, 12.0), None);
        assert_eq!(nearest_sample(&curve, 20.0), Some((20, 9.0)));
    }

    #[test]
    fn long_event_identity_stays_within_a_normal_panel_and_preserves_plot_height() {
        for language in [Language::English, Language::Chinese] {
            let context = egui::Context::default();
            let mut studio = Studio::default();
            studio.prefs.language = language;
            let mut curve = curve(vec![(0, None), (1, Some(2.0)), (2, Some(3.0))]);
            curve.spec.source =
                crate::curve::Source::ProfileEvent(pvd_core::profile_curves::ExactKey {
                    zone: u64::MAX,
                    event: u16::MAX,
                    thread: u32::MAX,
                    context: u64::MAX,
                });
            curve.data.as_mut().unwrap().reference_frame_count = Some(100);
            curve.label = curve.spec.label();
            studio.curves.curves.push(curve);
            studio.curves.pages[0].curves.push(1);
            studio.curves.selected = Some(1);
            let mut minimum_rect = egui::Rect::ZERO;
            for _ in 0..2 {
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(620.0, 280.0),
                        )),
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            show(ui, &mut studio);
                            minimum_rect = ui.min_rect();
                        });
                    },
                );
                let plot = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Rect(rect)
                            if rect.fill == egui::Color32::from_rgb(16, 23, 31) =>
                        {
                            Some(rect.rect)
                        }
                        _ => None,
                    })
                    .unwrap();
                assert!(
                    minimum_rect.right() <= 620.0,
                    "panel overflow: {minimum_rect:?}"
                );
                assert!(plot.height() >= 80.0, "plot too small: {plot:?}");
                assert!(plot.right() <= 612.0, "plot outside panel: {plot:?}");
            }
        }
    }

    #[test]
    fn low_height_curve_canvas_is_visible_inside_its_actual_clip() {
        for language in [Language::English, Language::Chinese] {
            for (width, height, minimum_visible) in [
                (620.0, 160.0, 50.0),
                (620.0, 180.0, 70.0),
                (400.0, 180.0, 40.0),
                (620.0, 280.0, 140.0),
            ] {
                let context = egui::Context::default();
                crate::ui::configure(&context);
                let mut studio = Studio::default();
                studio.prefs.language = language;
                let mut item = curve(vec![(0, None), (1, Some(2.0)), (2, Some(3.0))]);
                item.spec.source =
                    crate::curve::Source::ProfileEvent(pvd_core::profile_curves::ExactKey {
                        zone: u64::MAX,
                        event: u16::MAX,
                        thread: u32::MAX,
                        context: u64::MAX,
                    });
                item.label = item.spec.label();
                item.data.as_mut().unwrap().reference_frame_count = Some(100);
                studio.curves.curves.push(item);
                studio.curves.pages[0].curves.push(1);
                studio.curves.selected = Some(1);
                let viewport =
                    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
                for _ in 0..3 {
                    let output = context.run(
                        egui::RawInput {
                            screen_rect: Some(viewport),
                            ..Default::default()
                        },
                        |context| {
                            egui::CentralPanel::default().show(context, |ui| show(ui, &mut studio));
                        },
                    );
                    let (plot, clip) = output
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::epaint::Shape::Rect(rect)
                                if rect.fill == egui::Color32::from_rgb(16, 23, 31) =>
                            {
                                Some((rect.rect, shape.clip_rect))
                            }
                            _ => None,
                        })
                        .max_by(|a, b| a.0.area().total_cmp(&b.0.area()))
                        .unwrap();
                    let visible = plot.intersect(clip).intersect(viewport);
                    println!(
                        "{language:?} {width}x{height}: visible plot {}px",
                        visible.height()
                    );
                    assert!(
                        visible.height() >= minimum_visible,
                        "{language:?} {width}x{height}: plot={plot:?}, clip={clip:?}, visible={visible:?}"
                    );
                    assert!(plot.right() <= width, "horizontal overflow: {plot:?}");
                }
            }
        }
    }

    fn drag(view: &mut View, from: egui::Pos2, to: egui::Pos2) -> Vec<f64> {
        let context = egui::Context::default();
        let fitted = Bounds {
            x: [0.0, 100.0],
            y: [0.0, 100.0],
        };
        view.bounds = Some(fitted);
        let mut jumps = Vec::new();
        let events = [
            vec![],
            vec![
                egui::Event::PointerMoved(from),
                egui::Event::PointerButton {
                    pos: from,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            vec![egui::Event::PointerMoved(to)],
            vec![egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        ];
        for events in events {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(400.0, 240.0),
                )),
                events,
                ..Default::default()
            };
            let _ = context.run(input, |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(200.0, 100.0),
                        egui::Sense::click_and_drag(),
                    );
                    if let Some(frame) = gesture(ui, &response, rect, view, fitted) {
                        jumps.push(frame);
                    }
                });
            });
        }
        jumps
    }

    #[test]
    fn rectangle_drag_zooms_both_axes_without_seeking() {
        let mut view = View {
            box_zoom: true,
            ..Default::default()
        };
        let jumps = drag(&mut view, egui::pos2(48.0, 28.0), egui::pos2(148.0, 68.0));
        assert!(jumps.is_empty());
        let bounds = view.bounds.unwrap();
        for (actual, expected) in bounds
            .x
            .into_iter()
            .chain(bounds.y)
            .zip([20.0, 70.0, 40.0, 80.0])
        {
            assert!((actual - expected).abs() < 1e-5);
        }
        assert!(view.box_start.is_none() && !view.auto_fit);
    }

    #[test]
    fn panning_moves_axes_and_clicks_return_frame_coordinates() {
        let mut view = View::default();
        assert!(drag(&mut view, egui::pos2(48.0, 28.0), egui::pos2(148.0, 68.0)).is_empty());
        let bounds = view.bounds.unwrap();
        assert_eq!(bounds.x, [-50.0, 50.0]);
        assert_eq!(bounds.y, [40.0, 140.0]);
        let jumps = drag(&mut view, egui::pos2(108.0, 58.0), egui::pos2(108.0, 58.0));
        assert_eq!(jumps, vec![50.0]);
    }

    #[test]
    fn wheel_zooms_at_pointer_and_text_focus_blocks_graph_gestures() {
        for text_focus in [false, true] {
            let context = egui::Context::default();
            let fitted = Bounds {
                x: [0.0, 100.0],
                y: [0.0, 100.0],
            };
            let mut view = View {
                bounds: Some(fitted),
                ..Default::default()
            };
            let mut center = egui::Pos2::ZERO;
            let mut text = String::new();
            for frame in 0..2 {
                let events = if frame == 0 {
                    vec![]
                } else {
                    vec![
                        egui::Event::PointerMoved(center),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, 60.0),
                            modifiers: egui::Modifiers::NONE,
                        },
                    ]
                };
                let _ = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(400.0, 240.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |context| {
                        egui::CentralPanel::default().show(context, |ui| {
                            if text_focus {
                                ui.text_edit_singleline(&mut text).request_focus();
                            }
                            let (rect, response) = ui.allocate_exact_size(
                                egui::vec2(200.0, 100.0),
                                egui::Sense::click_and_drag(),
                            );
                            center = rect.center();
                            assert!(gesture(ui, &response, rect, &mut view, fitted).is_none());
                        });
                    },
                );
            }
            let bounds = view.bounds.unwrap();
            if text_focus {
                assert_eq!(bounds, fitted);
            } else {
                assert!(bounds.x[1] - bounds.x[0] < 100.0);
                assert!(bounds.y[1] - bounds.y[0] < 100.0);
                assert!((interpolate(bounds.x, 0.5) - 50.0).abs() < 1e-8);
                assert!((interpolate(bounds.y, 0.5) - 50.0).abs() < 1e-8);
            }
        }
    }
}
