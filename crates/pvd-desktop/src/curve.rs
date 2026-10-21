use pvd_core::{Recording, Value, analysis::view::ProfileSource};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};

use crate::i18n::{Language, Message};

pub const MAX_CURVES: usize = 16;
pub const MAX_PAGES: usize = 8;
pub const MAX_SAMPLES: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Property,
    Performance(pvd_core::frame_metrics::Metric),
    ProfileEvent(pvd_core::profile_curves::ExactKey),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub source: Source,
    pub object: u64,
    pub generation: u32,
    pub property: String,
    pub component: usize,
    pub component_name: String,
}

impl Spec {
    pub fn needs_analysis(&self) -> bool {
        !matches!(
            self.source,
            Source::Property | Source::Performance(pvd_core::frame_metrics::Metric::FrameInterval)
        )
    }

    pub fn label(&self) -> Message {
        if let Source::ProfileEvent(key) = self.source {
            return Message::new(
                format!(
                    "z0x{:x}/e{} · thread {} · context 0x{:x} · scene 0x{:x} g{} · inclusive sum (ms)",
                    key.zone, key.event, key.thread, key.context, self.object, self.generation
                ),
                format!(
                    "区域0x{:x}/事件{} · 线程 {} · 上下文 0x{:x} · 参考场景 0x{:x} 代际{} · 包含耗时总和 (ms)",
                    key.zone, key.event, key.thread, key.context, self.object, self.generation
                ),
            );
        }
        if let Source::Performance(metric) = self.source {
            return Message::new(
                format!(
                    "0x{:x} · generation {} · {} (ms)",
                    self.object,
                    self.generation,
                    metric_label(metric, Language::English)
                ),
                format!(
                    "0x{:x} · 代际 {} · {} (ms)",
                    self.object,
                    self.generation,
                    metric_label(metric, Language::Chinese)
                ),
            );
        }
        Message::new(
            format!(
                "0x{:x} · generation {} · {}.{}",
                self.object, self.generation, self.property, self.component_name
            ),
            format!(
                "0x{:x} · 代际 {} · {}.{}",
                self.object,
                self.generation,
                self.property,
                component_label(&self.component_name, Language::Chinese)
            ),
        )
    }
}

#[derive(Clone)]
pub struct Data {
    pub samples: Vec<(usize, Option<f64>)>,
    pub stride: usize,
    pub source_frames: usize,
    pub reference_frame_count: Option<usize>,
    pub statuses: Vec<String>,
    pub occurrence_counts: Vec<usize>,
    pub diagnostics: Option<Message>,
}

pub struct Curve {
    pub id: u64,
    pub spec: Spec,
    pub label: Message,
    pub data: Option<Data>,
    pub pending: bool,
    pub error: Option<Message>,
    pub export_path: String,
}

pub struct Page {
    pub id: u64,
    pub name: String,
    pub curves: Vec<u64>,
    pub hidden: BTreeSet<u64>,
}

pub struct Workspace {
    pub curves: Vec<Curve>,
    pub pages: Vec<Page>,
    pub active_page: usize,
    pub selected: Option<u64>,
    pub epoch: u64,
    pub pending: VecDeque<u64>,
    next_id: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            curves: Vec::new(),
            pages: vec![Page {
                id: 0,
                name: String::new(),
                curves: Vec::new(),
                hidden: BTreeSet::new(),
            }],
            active_page: 0,
            selected: None,
            epoch: 0,
            pending: VecDeque::new(),
            next_id: 1,
        }
    }
}

impl Workspace {
    pub fn clear(&mut self) {
        let epoch = self.epoch.wrapping_add(1);
        *self = Self {
            epoch,
            ..Default::default()
        };
    }

    pub fn add(&mut self, spec: Spec) -> Result<u64, Message> {
        let id = if let Some(curve) = self.curves.iter_mut().find(|curve| curve.spec == spec) {
            if curve.data.is_none() && !curve.pending {
                curve.pending = true;
                curve.error = None;
                self.pending.push_back(curve.id);
            }
            curve.id
        } else {
            if self.curves.len() >= MAX_CURVES {
                return Err(Message::new(
                    "The capture can have at most 16 curves. Remove a curve before adding another.",
                    "每个录制最多建立 16 条曲线。请先移除一条再添加。",
                ));
            }
            let id = self.next_id;
            self.next_id += 1;
            let export_path = match spec.source {
                Source::Property => format!("property-{id}.csv"),
                Source::Performance(_) => format!("performance-{id}.csv"),
                Source::ProfileEvent(_) => format!("profile-event-{id}.csv"),
            };
            self.curves.push(Curve {
                id,
                label: spec.label(),
                spec,
                data: None,
                pending: true,
                error: None,
                export_path,
            });
            self.pending.push_back(id);
            id
        };
        let page = &mut self.pages[self.active_page];
        if !page.curves.contains(&id) {
            page.curves.push(id);
        }
        Ok(id)
    }

    pub fn new_page(&mut self) -> bool {
        if self.pages.len() >= MAX_PAGES {
            return false;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.pages.push(Page {
            id,
            name: String::new(),
            curves: Vec::new(),
            hidden: BTreeSet::new(),
        });
        self.active_page = self.pages.len() - 1;
        true
    }

    pub fn remove_page(&mut self) {
        if self.pages.len() <= 1 {
            return;
        }
        self.pages.remove(self.active_page);
        self.active_page = self.active_page.min(self.pages.len() - 1);
    }

    pub fn remove_curve(&mut self, id: u64) {
        self.curves.retain(|curve| curve.id != id);
        self.pending.retain(|queued| *queued != id);
        for page in &mut self.pages {
            page.curves.retain(|curve| *curve != id);
            page.hidden.remove(&id);
        }
        if self.selected == Some(id) {
            self.selected = None;
        }
    }

    pub fn cancel_pending(&mut self) {
        self.pending.clear();
        for curve in &mut self.curves {
            curve.pending = false;
        }
    }
}

pub struct Job {
    pub id: u64,
    pub progress: Arc<AtomicUsize>,
    pub source_frames: usize,
    cancel: Arc<AtomicBool>,
    receiver: Mutex<mpsc::Receiver<Result<Data, Message>>>,
}

impl Job {
    pub fn start(
        id: u64,
        spec: Spec,
        recording: Arc<Recording>,
        source: Option<crate::full_profile::Source>,
        context: bevy_egui::egui::Context,
        jobs: &crate::background_jobs::BackgroundJobs,
    ) -> Result<Self, Message> {
        let (sender, receiver) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let source_frames = recording.frame_count();
        let worker_cancel = cancel.clone();
        let worker_progress = progress.clone();
        jobs.spawn("pvd-curve", cancel.clone(), move || {
            let result = if let Some(source) = source {
                sample_with_source(
                    &recording,
                    &spec,
                    &worker_cancel,
                    &worker_progress,
                    Some(&source),
                )
            } else {
                sample(&recording, &spec, &worker_cancel, &worker_progress)
            };
            let _ = sender.send(result);
            context.request_repaint();
        })
        .map_err(|error| {
            Message::detail("Could not start curve sampling", "无法启动曲线采样", error)
        })?;
        Ok(Self {
            id,
            progress,
            source_frames,
            cancel,
            receiver: Mutex::new(receiver),
        })
    }

    pub fn result(&self) -> Option<Result<Data, Message>> {
        match self.receiver.lock() {
            Ok(receiver) => match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(Err(Message::new(
                    "Curve worker ended without a result",
                    "曲线后台任务未返回结果",
                ))),
            },
            Err(_) => Some(Err(Message::new(
                "Curve result lock failed",
                "曲线结果锁失败",
            ))),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn sampling_frames(count: usize) -> (usize, Vec<usize>) {
    let stride = count.saturating_sub(1).div_ceil(MAX_SAMPLES - 1).max(1);
    let mut frames: Vec<_> = (0..count).step_by(stride).collect();
    if count > 0 && frames.last() != Some(&(count - 1)) {
        frames.push(count - 1);
    }
    (stride, frames)
}

fn sample(
    recording: &Recording,
    spec: &Spec,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> Result<Data, Message> {
    sample_with_source(recording, spec, cancel, progress, None)
}

fn sample_with_source(
    recording: &Recording,
    spec: &Spec,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
    source: Option<&crate::full_profile::Source>,
) -> Result<Data, Message> {
    let source_frames = recording.frame_count();
    if let Source::ProfileEvent(key) = spec.source {
        let scene = pvd_core::profile_curves::ReferenceScene {
            id: spec.object,
            generation: spec.generation,
        };
        let sampling = pvd_core::profile_curves::sampling_frames(recording, scene, cancel)
            .map_err(|error| {
                Message::detail(
                    "Reference Scene sampling failed",
                    "参考 Scene 采样失败",
                    error,
                )
            })?;
        let points = if let Some(source) = source {
            source
                .sample(
                    crate::full_profile::SampleRequest {
                        recording,
                        key,
                        scene,
                        frames: &sampling.frames,
                    },
                    cancel,
                    |status| {
                        let completed = u128::from(status.bytes_read) * source_frames as u128
                            / u128::from(status.total_bytes.max(1));
                        progress.store(
                            completed.min(source_frames as u128) as usize,
                            Ordering::Relaxed,
                        );
                        !cancel.load(Ordering::Relaxed)
                    },
                )?
                .points
        } else {
            pvd_core::profile_curves::sample(recording, key, scene, &sampling.frames, cancel)
                .map_err(|error| {
                    Message::detail(
                        "Profile event curve indexing failed",
                        "性能事件曲线索引失败",
                        error,
                    )
                })?
        };
        let mut reasons = std::collections::BTreeMap::<&str, usize>::new();
        for point in &points {
            if point.milliseconds.is_none() {
                *reasons.entry(point.status.name()).or_default() += 1;
            }
        }
        let reasons = reasons
            .into_iter()
            .map(|(reason, count)| format!("{reason}: {count}"))
            .collect::<Vec<_>>()
            .join(" · ");
        progress.store(source_frames, Ordering::Relaxed);
        return Ok(Data {
            samples: points
                .iter()
                .map(|point| (point.frame, point.milliseconds))
                .collect(),
            statuses: points
                .iter()
                .map(|point| point.status.name().to_owned())
                .collect(),
            occurrence_counts: points.iter().map(|point| point.occurrence_count).collect(),
            stride: sampling.stride,
            source_frames,
            reference_frame_count: Some(sampling.reference_frame_count),
            diagnostics: (!reasons.is_empty()).then(|| {
                Message::new(format!("Gaps · {reasons}"), format!("空值诊断 · {reasons}"))
            }),
        });
    }
    let (stride, positions) = sampling_frames(recording.frames.len());
    let frames: Vec<_> = positions
        .into_iter()
        .map(|position| recording.frames[position].index)
        .collect();
    if let Source::Performance(metric) = spec.source {
        let (points, complete_source_counts) = if let Some(source) =
            source.filter(|_| metric != pvd_core::frame_metrics::Metric::FrameInterval)
        {
            let report = source.sample_stage(
                crate::full_profile::StageRequest {
                    recording,
                    scene: pvd_core::profile_curves::ReferenceScene {
                        id: spec.object,
                        generation: spec.generation,
                    },
                    metric,
                    frames: &frames,
                },
                cancel,
                |status| {
                    let phase = u128::from(
                        status.phase == pvd_core::analysis_export::AnalysisExportPhase::Export,
                    );
                    let total = u128::from(status.total_bytes.max(1));
                    let completed = (phase * total + u128::from(status.bytes_read))
                        * source_frames as u128
                        / (total * 2);
                    progress.store(
                        completed.min(source_frames as u128) as usize,
                        Ordering::Relaxed,
                    );
                    !cancel.load(Ordering::Relaxed)
                },
            )?;
            (
                report.points,
                Some((report.open_spans, report.unmatched_stops)),
            )
        } else {
            let points = pvd_core::frame_metrics::sample(
                recording,
                spec.object,
                spec.generation,
                metric,
                &frames,
                cancel,
            )
            .map_err(|error| {
                Message::detail("Frame metric indexing failed", "帧性能曲线索引失败", error)
            })?;
            (points, None)
        };
        let mut reasons = std::collections::BTreeMap::<&str, usize>::new();
        for point in &points {
            if point.milliseconds.is_none() {
                *reasons.entry(point.status.name()).or_default() += 1;
            }
        }
        let reasons = reasons
            .into_iter()
            .map(|(reason, count)| format!("{reason}: {count}"))
            .collect::<Vec<_>>()
            .join(" · ");
        let mut diagnostics = (!reasons.is_empty())
            .then(|| Message::new(format!("Gaps · {reasons}"), format!("空值诊断 · {reasons}")));
        if metric != pvd_core::frame_metrics::Metric::FrameInterval {
            let source_context =
                pvd_core::frame_metrics::source_context(recording, spec.object, spec.generation)
                    .map_err(|error| {
                        Message::detail(
                            "Frame metric context is unavailable",
                            "帧性能曲线上下文不可用",
                            error,
                        )
                    })?;
            let matches = |(zone, event, thread, event_context): &(u64, u16, u32, u64)| {
                *event_context == source_context
                    && *thread == pvd_core::frame_metrics::CROSS_THREAD_ID
                    && recording.analysis.profile_zone_name(*zone) == Some("PhysXSDK")
                    && recording.analysis.profile_event_name(*zone, *event) == Some(metric.name())
            };
            let open: usize = complete_source_counts.map_or_else(
                || {
                    recording
                        .analysis
                        .profile_open_counts()
                        .filter(|(key, _)| matches(key))
                        .map(|(_, count)| count)
                        .sum()
                },
                |(open, _)| open,
            );
            let unmatched: u64 = complete_source_counts.map_or_else(
                || {
                    recording
                        .analysis
                        .unmatched_profile_events()
                        .filter(|(key, _)| matches(key))
                        .map(|(_, count)| count)
                        .sum()
                },
                |(_, unmatched)| unmatched,
            );
            if open > 0 || unmatched > 0 {
                let english = format!(
                    "{open} open starts · {unmatched} unmatched stops. This curve shows a single complete interval only; incomplete events are excluded. It is not the total accumulated phase duration."
                );
                let chinese = format!(
                    "{open} 个未结束起点 · {unmatched} 个未配对结束。这里只显示单个完整区间；未结束或未配对事件不计入，不代表全部阶段累计耗时。"
                );
                diagnostics = Some(match diagnostics {
                    Some(previous) => Message::new(
                        format!("{}\n{english}", previous.text(Language::English)),
                        format!("{}\n{chinese}", previous.text(Language::Chinese)),
                    ),
                    None => Message::new(english, chinese),
                });
            }
        }
        progress.store(source_frames, Ordering::Relaxed);
        return Ok(Data {
            samples: points
                .iter()
                .map(|point| (point.frame, point.milliseconds))
                .collect(),
            statuses: points
                .iter()
                .map(|point| point.status.name().to_owned())
                .collect(),
            occurrence_counts: Vec::new(),
            stride,
            source_frames,
            reference_frame_count: None,
            diagnostics,
        });
    }
    let mut samples = Vec::with_capacity(frames.len());
    for frame in frames {
        if cancel.load(Ordering::Relaxed) {
            return Err(Message::new("Curve indexing cancelled", "已取消曲线索引"));
        }
        let state = recording
            .state_at(frame)
            .map_err(|error| Message::detail("Curve indexing failed", "曲线索引失败", error))?;
        let value = state
            .objects
            .get(&spec.object)
            .filter(|object| object.generation == spec.generation)
            .and_then(|object| object.properties.get(&spec.property))
            .filter(|value| {
                components(value)
                    .get(spec.component)
                    .is_some_and(|name| *name == spec.component_name)
            })
            .and_then(|value| number(value, spec.component));
        samples.push((frame, value));
        progress.store(frame + 1, Ordering::Relaxed);
    }
    Ok(Data {
        samples,
        stride,
        source_frames,
        reference_frame_count: None,
        statuses: Vec::new(),
        occurrence_counts: Vec::new(),
        diagnostics: None,
    })
}

pub fn csv(curve: &Curve) -> Option<String> {
    let data = curve.data.as_ref()?;
    let mut csv = String::from(
        "frame,value,object_id,generation,property,component,stride,captured_frame_count,source,unit,status,association,zone,event,thread,context,occurrence_count,aggregation,reference_scene,reference_scene_generation,stride_basis,reference_frame_count\r\n",
    );
    let quote = |value: &str| format!("\"{}\"", value.replace('"', "\"\""));
    for (index, (frame, value)) in data.samples.iter().enumerate() {
        let (source, unit, association, aggregation) = match curve.spec.source {
            Source::Property => (
                "property",
                "property_value",
                "recorded_frame_state",
                "recorded_value",
            ),
            Source::Performance(pvd_core::frame_metrics::Metric::FrameInterval) => (
                "frame_interval",
                "ms",
                "same_scene_end_section_difference",
                "interval_difference",
            ),
            Source::Performance(_) => (
                "profile_stage_elapsed",
                "ms",
                "same_scene_context_and_end_section_time_interval",
                "single_complete_span",
            ),
            Source::ProfileEvent(_) => (
                "profile_event_inclusive_sum",
                "ms",
                "reference_scene_time_window_not_scene_ownership",
                "inclusive_sum",
            ),
        };
        let status = data
            .statuses
            .get(index)
            .map(String::as_str)
            .unwrap_or(if value.is_some() {
                "recorded"
            } else {
                "missing_property_or_generation"
            });
        let identity = match curve.spec.source {
            Source::ProfileEvent(key) => format!(
                "0x{:x},{},{},0x{:x},{}",
                key.zone,
                key.event,
                key.thread,
                key.context,
                data.occurrence_counts
                    .get(index)
                    .map(usize::to_string)
                    .unwrap_or_default()
            ),
            _ => ",,,,".into(),
        };
        let reference_scene = match curve.spec.source {
            Source::Property => ",".into(),
            _ => format!("0x{:x},{}", curve.spec.object, curve.spec.generation),
        };
        let stride_basis = if matches!(curve.spec.source, Source::ProfileEvent(_)) {
            "reference_scene_generation_frames"
        } else {
            "captured_frames"
        };
        let reference_frame_count = data
            .reference_frame_count
            .map(|count| count.to_string())
            .unwrap_or_default();
        csv.push_str(&format!(
            "{frame},{},0x{:x},{},{},{},{},{},{source},{unit},{status},{association},{identity},{aggregation},{reference_scene},{stride_basis},{reference_frame_count}\r\n",
            value.map(|v| v.to_string()).unwrap_or_default(),
            curve.spec.object,
            curve.spec.generation,
            quote(&curve.spec.property),
            quote(&curve.spec.component_name),
            data.stride,
            data.source_frames
        ));
    }
    Some(csv)
}

pub fn metric_label(metric: pvd_core::frame_metrics::Metric, language: Language) -> &'static str {
    use pvd_core::frame_metrics::Metric;
    match metric {
        Metric::FrameInterval => language.text("Frame end interval", "帧结束间隔"),
        Metric::Simulate => language.text("Simulate elapsed", "模拟阶段经过时间"),
        Metric::Collision => language.text("Collision elapsed", "碰撞阶段经过时间"),
        Metric::Solver => language.text("Solver elapsed", "求解阶段经过时间"),
    }
}

pub fn components(value: &Value) -> &'static [&'static str] {
    match value {
        Value::I64(_) | Value::U64(_) | Value::F32(_) | Value::F64(_) => &["value"],
        Value::Vec3(_) => &["x", "y", "z"],
        Value::Quat(_) => &["x", "y", "z", "w"],
        Value::Transform { .. } => &[
            "position.x",
            "position.y",
            "position.z",
            "rotation.x",
            "rotation.y",
            "rotation.z",
            "rotation.w",
        ],
        _ => &[],
    }
}

pub fn component_label(component: &str, language: Language) -> &str {
    match component {
        "value" => language.text("value", "值"),
        "position.x" => language.text("position.x", "位置.x"),
        "position.y" => language.text("position.y", "位置.y"),
        "position.z" => language.text("position.z", "位置.z"),
        "rotation.x" => language.text("rotation.x", "旋转.x"),
        "rotation.y" => language.text("rotation.y", "旋转.y"),
        "rotation.z" => language.text("rotation.z", "旋转.z"),
        "rotation.w" => language.text("rotation.w", "旋转.w"),
        _ => component,
    }
}

pub fn number(value: &Value, component: usize) -> Option<f64> {
    let value = match value {
        Value::I64(v) if component == 0 => *v as f64,
        Value::U64(v) if component == 0 => *v as f64,
        Value::F32(v) if component == 0 => *v as f64,
        Value::F64(v) if component == 0 => *v,
        Value::Vec3(v) => *v.get(component)? as f64,
        Value::Quat(v) => *v.get(component)? as f64,
        Value::Transform { position, rotation } => {
            if component < 3 {
                position[component] as f64
            } else {
                *rotation.get(component - 3)? as f64
            }
        }
        _ => return None,
    };
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(object: u64) -> Spec {
        Spec {
            source: Source::Property,
            object,
            generation: 0,
            property: "GlobalPose".into(),
            component: 1,
            component_name: "position.y".into(),
        }
    }

    #[test]
    fn sample_budget_includes_first_and_last_frames_without_exceeding_limit() {
        for count in [
            0,
            1,
            2,
            4999,
            5000,
            5001,
            9999,
            10000,
            10001,
            1_000_000,
            usize::MAX,
        ] {
            let (stride, frames) = sampling_frames(count);
            assert!(stride > 0 && frames.len() <= MAX_SAMPLES, "count {count}");
            if count == 0 {
                assert!(frames.is_empty());
            } else {
                assert_eq!(frames.first(), Some(&0));
                assert_eq!(frames.last(), Some(&(count - 1)));
                assert!(frames.windows(2).all(|pair| pair[0] < pair[1]));
            }
        }
    }

    #[test]
    fn workspace_deduplicates_requests_bounds_pages_and_removes_all_references() {
        let mut workspace = Workspace::default();
        let first = workspace.add(spec(1)).unwrap();
        assert_eq!(workspace.add(spec(1)).unwrap(), first);
        assert_eq!(workspace.pending.len(), 1);
        assert!(workspace.new_page());
        assert_eq!(workspace.add(spec(1)).unwrap(), first);
        workspace.pages[1].hidden.insert(first);
        assert!(!workspace.pages[0].hidden.contains(&first));
        for object in 2..=16 {
            workspace.add(spec(object)).unwrap();
        }
        assert!(workspace.add(spec(17)).is_err());
        while workspace.new_page() {}
        assert_eq!(workspace.pages.len(), MAX_PAGES);
        workspace.remove_curve(first);
        assert!(!workspace.pending.contains(&first));
        assert!(
            workspace
                .pages
                .iter()
                .all(|page| !page.curves.contains(&first) && !page.hidden.contains(&first))
        );
        workspace.add(spec(17)).unwrap();
        workspace.cancel_pending();
        assert!(workspace.pending.is_empty());
        assert!(workspace.curves.iter().all(|curve| !curve.pending));
        let retried = workspace.add(spec(17)).unwrap();
        assert_eq!(workspace.pending.front(), Some(&retried));
        workspace.add(spec(17)).unwrap();
        assert_eq!(workspace.pending.len(), 1);
        let epoch = workspace.epoch;
        workspace.clear();
        assert!(workspace.curves.is_empty());
        assert_eq!(workspace.pages.len(), 1);
        assert_ne!(workspace.epoch, epoch);
    }

    #[test]
    fn csv_preserves_missing_values_generation_and_sampling_metadata() {
        let mut workspace = Workspace::default();
        let mut key = spec(0x123);
        key.property = "quoted,\"property\"".into();
        key.generation = 7;
        workspace.add(key).unwrap();
        let curve = &mut workspace.curves[0];
        curve.data = Some(Data {
            samples: vec![(0, Some(1.25)), (3, None), (5, Some(-2.5))],
            stride: 3,
            source_frames: 6,
            reference_frame_count: None,
            statuses: Vec::new(),
            occurrence_counts: Vec::new(),
            diagnostics: None,
        });
        let csv = csv(curve).unwrap();
        assert!(csv.contains("0,1.25,0x123,7,\"quoted,\"\"property\"\"\",\"position.y\",3,6,property,property_value,recorded,recorded_frame_state,,,,,,recorded_value,,,captured_frames,\r\n"));
        assert!(csv.contains("3,,0x123,7,"));
        assert!(csv.contains("5,-2.5,0x123,7,"));
        assert_eq!(number(&Value::F32(f32::NAN), 0), None);
        assert_eq!(number(&Value::F32(1.0), 1), None);
        assert_eq!(number(&Value::Vec3([1.0, 2.0, 3.0]), 2), Some(3.0));
    }

    #[test]
    fn event_csv_retains_exact_identity_counts_gaps_and_reference_scene() {
        use pvd_core::profile_curves::ExactKey;
        let key = ExactKey {
            zone: 0x999,
            event: 21,
            thread: 7,
            context: 0x123,
        };
        let mut source = spec(0x456);
        source.source = Source::ProfileEvent(key);
        source.generation = 3;
        source.property = "profile_event_inclusive_sum".into();
        source.component_name = "ms".into();
        let mut workspace = Workspace::default();
        let original = workspace.add(source.clone()).unwrap();
        assert_eq!(workspace.add(source.clone()).unwrap(), original);
        for changed in [
            ExactKey {
                zone: key.zone + 1,
                ..key
            },
            ExactKey {
                event: key.event + 1,
                ..key
            },
            ExactKey {
                thread: key.thread + 1,
                ..key
            },
            ExactKey {
                context: key.context + 1,
                ..key
            },
        ] {
            source.source = Source::ProfileEvent(changed);
            assert_ne!(workspace.add(source.clone()).unwrap(), original);
        }
        source.source = Source::ProfileEvent(key);
        source.generation += 1;
        assert_ne!(workspace.add(source).unwrap(), original);
        let curve = &mut workspace.curves[0];
        curve.data = Some(Data {
            samples: vec![(0, None), (1, Some(3.5)), (2, None)],
            stride: 1,
            source_frames: 3,
            reference_frame_count: Some(3),
            statuses: vec![
                "no_previous_frame".into(),
                "recorded".into(),
                "span_crosses_frame_boundary".into(),
            ],
            occurrence_counts: vec![0, 2, 1],
            diagnostics: None,
        });
        let csv = csv(curve).unwrap();
        let rows: Vec<Vec<_>> = csv.lines().map(|line| line.split(',').collect()).collect();
        assert!(rows.iter().all(|row| row.len() == 22));
        assert_eq!(
            &rows[0][12..20],
            &[
                "zone",
                "event",
                "thread",
                "context",
                "occurrence_count",
                "aggregation",
                "reference_scene",
                "reference_scene_generation"
            ]
        );
        assert_eq!(
            &rows[2][12..20],
            &[
                "0x999",
                "21",
                "7",
                "0x123",
                "2",
                "inclusive_sum",
                "0x456",
                "3"
            ]
        );
        assert_eq!(rows[2][1], "3.5");
        assert_eq!(
            &rows[2][8..12],
            &[
                "profile_event_inclusive_sum",
                "ms",
                "recorded",
                "reference_scene_time_window_not_scene_ownership"
            ]
        );
        assert_eq!(rows[3][1], "");
        assert_eq!(rows[3][10], "span_crosses_frame_boundary");
        assert_eq!(rows[3][16], "1");
        assert_eq!(&rows[2][20..], &["reference_scene_generation_frames", "3"]);
    }

    #[test]
    fn phase_csv_does_not_claim_generic_event_identity_or_occurrence_count() {
        use pvd_core::frame_metrics::Metric;
        for (metric, aggregation) in [
            (Metric::FrameInterval, "interval_difference"),
            (Metric::Solver, "single_complete_span"),
        ] {
            let mut workspace = Workspace::default();
            let mut source = spec(0x456);
            source.source = Source::Performance(metric);
            workspace.add(source).unwrap();
            let curve = &mut workspace.curves[0];
            curve.data = Some(Data {
                samples: vec![(1, Some(0.2))],
                stride: 1,
                source_frames: 2,
                reference_frame_count: None,
                statuses: vec!["recorded".into()],
                occurrence_counts: vec![],
                diagnostics: None,
            });
            let csv = csv(curve).unwrap();
            let row: Vec<_> = csv.lines().nth(1).unwrap().split(',').collect();
            assert_eq!(&row[12..17], &["", "", "", "", ""]);
            assert_eq!(row[17], aggregation);
            assert_eq!(&row[18..], &["0x456", "0", "captured_frames", ""]);
        }
    }

    #[test]
    #[ignore = "requires official SDK stage profile capture"]
    fn sdk_single_stage_diagnostic_discloses_open_events_without_hiding_complete_values() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../captures/physx-3.4-profile-metrics.pvdstream");
        let recording = Recording::from_path(&path).unwrap();
        let truth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        let mut spec = spec(truth["scene"].as_u64().unwrap());
        spec.source = Source::Performance(pvd_core::frame_metrics::Metric::Solver);
        let data = sample(
            &recording,
            &spec,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
        assert_eq!(data.samples.len(), 24);
        assert!(data.samples.iter().all(|(_, value)| value.is_some()));
        let diagnostic = data.diagnostics.unwrap();
        assert!(
            diagnostic
                .text(Language::English)
                .contains("24 open starts")
        );
        assert!(
            diagnostic
                .text(Language::English)
                .contains("single complete interval")
        );
        assert!(
            diagnostic
                .text(Language::Chinese)
                .contains("不代表全部阶段累计耗时")
        );
    }

    #[test]
    #[ignore = "requires official SDK profile capture and independent QPC callback bounds"]
    fn sdk_event_curve_csv_matches_callback_bounds_and_cancels_on_source_close() {
        let path = std::env::var_os("PVD_PROFILE_EVENTS_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4-profile-events.pvdstream")
            });
        let recording = Arc::new(Recording::from_path(&path).unwrap());
        let truth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path.with_extension("json")).unwrap()).unwrap();
        let scene = truth["scene"].as_u64().unwrap();
        let key = pvd_core::profile_curves::catalog(&recording, &AtomicBool::new(false))
            .unwrap()
            .into_iter()
            .find(|entry| {
                entry.key.context == truth["context_a"].as_u64().unwrap()
                    && entry.key.thread == truth["main_thread"].as_u64().unwrap() as u32
                    && recording
                        .analysis
                        .profile_event_name(entry.key.zone, entry.key.event)
                        == Some("PvdStudio.Profile.Sample")
            })
            .unwrap()
            .key;
        assert_ne!(key.context, scene);
        let context = bevy_egui::egui::Context::default();
        let mut studio = crate::studio::Studio::default();
        studio.recording = Some(recording.clone());
        studio.seek(1);
        studio.plot_profile_event(key, context.clone());
        assert_eq!(studio.requested_panel, Some(crate::studio::Panel::Curves));
        let cancelled = studio.curve_job.as_ref().unwrap().cancel.clone();
        studio.close();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(studio.curves.curves.is_empty() && studio.requested_panel.is_none());
        studio.recording = Some(recording.clone());
        studio.seek(1);
        studio.plot_profile_event(key, context.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while studio.curve_job.is_some() && std::time::Instant::now() < deadline {
            studio.poll(&context);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(studio.curve_job.is_none());
        let curve = &studio.curves.curves[0];
        let data = curve.data.as_ref().unwrap();
        assert_eq!(data.samples.len(), 8);
        assert!(data.samples[0].1.is_none());
        assert_eq!(data.statuses[0], "no_previous_frame");
        let frequency = truth["counter_frequency"].as_u64().unwrap() as f64;
        let csv = csv(curve).unwrap();
        let rows: Vec<Vec<_>> = csv
            .lines()
            .skip(1)
            .map(|line| line.split(',').collect())
            .collect();
        let mut compared = 0;
        for (frame, row) in rows.iter().enumerate().skip(1) {
            let expected: Vec<_> = truth["spans"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|span| {
                    span["event"].as_str() == Some("PvdStudio.Profile.Sample")
                        && span["thread"].as_u64() == Some(key.thread as u64)
                        && span["context"].as_u64() == Some(key.context)
                        && span["start_frame"].as_u64() == Some(frame as u64)
                        && span["end_frame"].as_u64() == Some(frame as u64)
                })
                .collect();
            assert_eq!(row[16].parse::<usize>().unwrap(), expected.len());
            if expected.is_empty() {
                assert!(data.samples[frame].1.is_none());
                assert_eq!(row[1], "");
                assert_eq!(row[10], "no_complete_span");
                continue;
            }
            let minimum = expected
                .iter()
                .map(|span| {
                    span["end_lower"]
                        .as_u64()
                        .unwrap()
                        .saturating_sub(span["start_upper"].as_u64().unwrap())
                })
                .sum::<u64>() as f64
                * 1000.0
                / frequency;
            let maximum = expected
                .iter()
                .map(|span| {
                    span["end_upper"].as_u64().unwrap() - span["start_lower"].as_u64().unwrap()
                })
                .sum::<u64>() as f64
                * 1000.0
                / frequency;
            let value = data.samples[frame].1.unwrap();
            assert!((minimum..=maximum).contains(&value));
            assert_eq!(row[1].parse::<f64>().unwrap(), value);
            assert_eq!(rows[frame][15], format!("0x{:x}", key.context));
            assert_eq!(rows[frame][18], format!("0x{scene:x}"));
            compared += expected.len();
        }
        assert_eq!(compared, 7);
    }

    #[test]
    #[ignore = "requires official SDK 3.4 lifecycle capture and independent pose CSV"]
    fn sdk_curve_values_match_truth_and_never_join_reused_actor_generations() {
        let path = std::env::var_os("PVD_SDK34_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4.pvdstream")
            });
        let recording = Recording::from_path(&path).unwrap();
        let initial = recording.state_at(0).unwrap();
        let actor = initial
            .objects
            .values()
            .find(|actor| {
                actor.properties.get("Name") == Some(&Value::String("PvdStudioSphere".into()))
            })
            .unwrap();
        let mut key = spec(actor.id);
        key.generation = actor.generation;
        let progress = AtomicUsize::new(0);
        let original = sample(&recording, &key, &AtomicBool::new(false), &progress).unwrap();
        let replacement = recording.state_at(90).unwrap();
        key.generation = replacement.objects[&actor.id].generation;
        assert_ne!(key.generation, actor.generation);
        let replaced = sample(&recording, &key, &AtomicBool::new(false), &progress).unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), 120);
        assert!(
            original.samples[90..]
                .iter()
                .all(|(_, value)| value.is_none())
        );
        assert!(
            replaced.samples[..90]
                .iter()
                .all(|(_, value)| value.is_none())
        );
        let truth = std::fs::read_to_string(path.with_extension("csv")).unwrap();
        let mut compared = 0;
        for line in truth.lines().skip(1) {
            let fields: Vec<_> = line.split(',').collect();
            if fields[1].parse::<u64>().unwrap() != actor.id {
                continue;
            }
            let frame = fields[0].parse::<usize>().unwrap();
            let expected = fields[4].parse::<f32>().unwrap() as f64;
            let data = if frame < 90 { &original } else { &replaced };
            assert!((data.samples[frame].1.unwrap() - expected).abs() < 0.00001);
            compared += 1;
        }
        assert_eq!(compared, 120);
        let progress = AtomicUsize::new(0);
        assert!(sample(&recording, &key, &AtomicBool::new(true), &progress).is_err());
        assert_eq!(progress.load(Ordering::Relaxed), 0);

        let context = bevy_egui::egui::Context::default();
        let recording = Arc::new(recording);
        let mut studio = crate::studio::Studio::default();
        studio.recording = Some(recording.clone());
        studio.seek(0);
        studio.selected = Some(actor.id);
        studio.plot("GlobalPose".into(), 0, context.clone());
        studio.plot("GlobalPose".into(), 1, context.clone());
        let cancelled = studio.curve_job.as_ref().unwrap().cancel.clone();
        assert_eq!(studio.curves.pending.len(), 1);
        studio.close();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(studio.curve_job.is_none() && studio.curves.pending.is_empty());
        studio.recording = Some(recording);
        studio.seek(90);
        studio.selected = Some(actor.id);
        studio.plot("GlobalPose".into(), 2, context.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while studio.curve_job.is_some() && std::time::Instant::now() < deadline {
            studio.poll(&context);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(studio.curve_job.is_none(), "curve indexing did not finish");
        assert_eq!(studio.curves.curves.len(), 1);
        let curve = &studio.curves.curves[0];
        assert_eq!(curve.spec.component, 2);
        assert_eq!(curve.spec.generation, key.generation);
        assert!(
            curve.data.as_ref().unwrap().samples[..90]
                .iter()
                .all(|(_, value)| value.is_none())
        );
        println!(
            "120 independent SDK position.y values match; generation gaps and cancelled source replacement are isolated"
        );
    }
}

#[cfg(test)]
mod streaming_stage_tests {
    use super::*;

    #[test]
    #[ignore = "requires preserved official SDK 3.4 QPC profile capture"]
    fn full_source_fixed_stage_hook_keeps_values_and_single_complete_span_csv_after_expiry() {
        let path = std::path::PathBuf::from(
            std::env::var_os("PVD_PROFILE_METRICS_CAPTURE").expect("PVD_PROFILE_METRICS_CAPTURE"),
        );
        let bytes = std::fs::read(path).unwrap();
        let mut recording = Recording::from_bytes(&bytes).unwrap();
        let scene = recording.frames[0].scene_id;
        let generation = recording.state_at(0).unwrap().objects[&scene].generation;
        let identity = crate::clip_store::Identity::from_digest(
            blake3::hash(&bytes),
            bytes.len() as u64,
            recording.frame_count(),
        );
        let source = crate::full_profile::Source::memory(Arc::new(bytes), identity);
        let frames: Vec<_> = (0..recording.frame_count()).collect();
        let cancel = AtomicBool::new(false);
        let progress = AtomicUsize::new(0);
        let original = recording.analysis.clone();
        for metric in [
            pvd_core::frame_metrics::Metric::Simulate,
            pvd_core::frame_metrics::Metric::Collision,
            pvd_core::frame_metrics::Metric::Solver,
        ] {
            recording.analysis = original.clone();
            let expected = pvd_core::frame_metrics::sample(
                &recording, scene, generation, metric, &frames, &cancel,
            )
            .unwrap();
            let mut retained = pvd_core::analysis::Analysis::default();
            retained.first_retained_frame = recording.frame_count();
            recording.analysis = retained.into();
            let spec = Spec {
                source: Source::Performance(metric),
                object: scene,
                generation,
                property: metric.name().into(),
                component: 0,
                component_name: "ms".into(),
            };
            let data =
                sample_with_source(&recording, &spec, &cancel, &progress, Some(&source)).unwrap();
            assert_eq!(data.samples.len(), 24);
            for ((frame, value), point) in data.samples.iter().zip(&expected) {
                assert_eq!(*frame, point.frame);
                assert_eq!(*value, point.milliseconds);
            }
            assert_eq!(progress.load(Ordering::Relaxed), recording.frame_count());
            if metric == pvd_core::frame_metrics::Metric::Solver {
                assert!(
                    data.diagnostics
                        .as_ref()
                        .unwrap()
                        .text(Language::English)
                        .contains("24 open starts")
                );
            }
            let curve = Curve {
                id: 1,
                label: spec.label(),
                spec,
                data: Some(data),
                pending: false,
                error: None,
                export_path: String::new(),
            };
            let exported = csv(&curve).unwrap();
            assert_eq!(exported.lines().count(), 25);
            assert!(
                exported
                    .lines()
                    .skip(1)
                    .all(|line| line.contains("single_complete_span"))
            );
            assert!(!exported.contains("inclusive_sum"));
        }
    }
}
