use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
};

use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, visibility::RenderLayers},
    mesh::{Indices, VertexAttributeValues},
    prelude::*,
    render::render_resource::{
        Extent3d, PrimitiveTopology, TextureDescriptor, TextureDimension, TextureFormat,
        TextureUsages,
    },
};
use bevy_egui::{EguiGlobalSettings, EguiTextureHandle, EguiUserTextures, PrimaryEguiContext};
use pvd_core::{
    Geometry, RenderObject,
    analysis::view::{MemorySource, ProfileSource},
};

use crate::coordinates::Coordinates;
use crate::{i18n::Message, studio::Studio};

mod display_validation;
mod incremental;
#[cfg(test)]
mod selected_overlay_tests;
mod source_index;
#[cfg(test)]
mod source_index_sync_tests;
pub(crate) use incremental::SceneChanges;

const MAX_DEBUG_PRIMITIVES: usize = 100_000;
const MAX_OVERLAY_LINES: usize = 300_000;

struct QueryLine {
    positions: [Vec3; 2],
    overlap: bool,
    color: Color,
}

#[derive(Component)]
pub struct ViewCamera;
#[derive(Component)]
pub struct CapturedObject;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MaterialKey {
    tint: u8,
    opacity: u32,
}

struct RenderedObject {
    entity: Entity,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    transform: Transform,
    visible: bool,
}

struct DebugSurface {
    entity: Entity,
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

struct SharedGeometry {
    vertices: std::sync::Arc<Vec<[f32; 3]>>,
    indices: std::sync::Arc<Vec<u32>>,
    hash: u64,
    valid: bool,
}

#[derive(Resource)]
pub struct RenderCheck {
    pub output: String,
    pub target: Handle<Image>,
    pub frames: u32,
    pub requested: bool,
    pub desired_frame: Option<usize>,
    pub panel: Option<String>,
    pub prepared: bool,
    pub seek_requested: bool,
    pub size: UVec2,
    pub completion: Option<bevy::app::AppExit>,
    pub exit_sent: bool,
}

#[derive(Resource)]
pub struct Viewport {
    pub image: Handle<Image>,
    pub texture: egui::TextureId,
    pub pixel_size: UVec2,
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub captured_pose: Option<crate::captured_camera::Pose>,
    pub near: f32,
    pub far: f32,
    pub coordinates: Coordinates,
    pub objects: Vec<RenderObject>,
    pub invalid_shapes: usize,
    pub diagnostics: Vec<Message>,
    pub queries: pvd_core::queries::QueryScene,
    pub point_clouds: crate::point_clouds::PointClouds,
    pub point_radius: f32,
    show_particles: bool,
    show_cloth: bool,
    query_lines: Vec<QueryLine>,
    limit_lines: Vec<pvd_core::joint_geometry::Line>,
    meshes: HashMap<u64, Handle<Mesh>>,
    mesh_cache: pvd_core::MeshCache,
    shared_geometry: HashMap<(usize, usize), SharedGeometry>,
    rendered: HashMap<(u64, u64), RenderedObject>,
    materials: HashMap<MaterialKey, Handle<StandardMaterial>>,
    debug_surface: Option<DebugSurface>,
    wires: HashMap<u64, Vec<[Vec3; 2]>>,
    wire_keys: HashMap<u64, u64>,
    revision: u64,
    selected: Option<u64>,
    settings: [u32; 12],
    incremental: incremental::SceneIndex,
    source_index: source_index::SourceIndex,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            image: Handle::default(),
            texture: egui::TextureId::default(),
            pixel_size: UVec2::splat(512),
            target: Vec3::ZERO,
            yaw: 0.7,
            pitch: 0.45,
            distance: 25.0,
            captured_pose: None,
            near: 0.01,
            far: 1_000_000.0,
            coordinates: Coordinates::default(),
            objects: Vec::new(),
            invalid_shapes: 0,
            diagnostics: Vec::new(),
            queries: pvd_core::queries::QueryScene::default(),
            point_clouds: crate::point_clouds::PointClouds::default(),
            point_radius: 0.025,
            show_particles: true,
            show_cloth: true,
            query_lines: Vec::new(),
            limit_lines: Vec::new(),
            meshes: HashMap::new(),
            mesh_cache: pvd_core::MeshCache::default(),
            shared_geometry: HashMap::new(),
            rendered: HashMap::new(),
            materials: HashMap::new(),
            debug_surface: None,
            wires: HashMap::new(),
            wire_keys: HashMap::new(),
            revision: u64::MAX,
            selected: None,
            settings: [u32::MAX; 12],
            incremental: incremental::SceneIndex::default(),
            source_index: source_index::SourceIndex::default(),
        }
    }
}

impl Viewport {
    pub fn project(&self, position: Vec3, rect: egui::Rect) -> Option<(egui::Pos2, f32)> {
        let position = self.coordinates.point(position);
        let eye = self.position();
        let forward = (self.view_target() - eye).normalize();
        let right = forward.cross(self.view_up()).normalize();
        let up = right.cross(forward).normalize();
        let offset = position - eye;
        let depth = offset.dot(forward);
        if depth < self.near || depth > self.far {
            return None;
        }
        let scale = rect.height() / (2.0 * (std::f32::consts::FRAC_PI_4 * 0.5).tan() * depth);
        Some((
            rect.center() + egui::vec2(offset.dot(right) * scale, -offset.dot(up) * scale),
            scale,
        ))
    }
    pub fn position(&self) -> Vec3 {
        if let Some(pose) = self.captured_pose {
            return pose.position;
        }
        self.target
            + Vec3::new(
                self.yaw.sin() * self.pitch.cos(),
                self.pitch.sin(),
                self.yaw.cos() * self.pitch.cos(),
            ) * self.distance
    }
    pub fn view_target(&self) -> Vec3 {
        self.captured_pose.map_or(self.target, |pose| pose.target)
    }
    pub fn view_up(&self) -> Vec3 {
        self.captured_pose.map_or(Vec3::Y, |pose| pose.up)
    }
    pub fn view_distance(&self) -> f32 {
        self.captured_pose
            .map_or(self.distance, |pose| pose.position.distance(pose.target))
    }
    pub fn focus(&mut self, selected: Option<u64>) {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        let mut include = |position: Vec3, radius: f32| {
            let position = self.coordinates.point(position);
            let radius = radius * self.coordinates.scale;
            if valid_position(position.to_array()) && radius.is_finite() {
                min = min.min(position - Vec3::splat(radius));
                max = max.max(position + Vec3::splat(radius));
            }
        };
        for object in self
            .objects
            .iter()
            .filter(|o| selected.is_none_or(|id| o.id == id || o.shape_id == id))
            .filter(|o| selected.is_some() || !matches!(o.geometry, Geometry::Plane))
        {
            include(
                Vec3::from(object.position),
                geometry_radius(&object.geometry),
            );
        }
        for cloud in &self.point_clouds.clouds {
            let visible = if cloud.kind == crate::point_clouds::Kind::Cloth {
                self.show_cloth
            } else {
                self.show_particles
            };
            if visible && selected.is_none_or(|id| id == cloud.id) {
                for point in &cloud.points {
                    include(*point, self.point_radius);
                }
            }
        }
        if selected.is_none() {
            for sweep in &self.queries.sweeps {
                let start = Vec3::from(sweep.geometry.position);
                let radius = geometry_radius(&sweep.geometry.geometry);
                include(start, radius);
                include(start + Vec3::from(sweep.direction) * sweep.distance, radius);
            }
            for overlap in &self.queries.overlaps {
                include(
                    Vec3::from(overlap.geometry.position),
                    geometry_radius(&overlap.geometry.geometry),
                );
            }
        }
        if min.is_finite() && max.is_finite() {
            self.target = (min + max) * 0.5;
            self.distance = ((max - min).length() * 1.4)
                .clamp(0.01 * self.coordinates.scale, 1e12 * self.coordinates.scale);
        }
    }
    pub fn select_at(&self, point: egui::Pos2, rect: egui::Rect) -> Option<u64> {
        let x = ((point.x - rect.left()) / rect.width()) * 2.0 - 1.0;
        let y = 1.0 - ((point.y - rect.top()) / rect.height()) * 2.0;
        let eye = self.position();
        let forward = (self.view_target() - eye).normalize();
        let right = forward.cross(self.view_up()).normalize();
        let up = right.cross(forward).normalize();
        let tangent = (std::f32::consts::FRAC_PI_4 * 0.5).tan();
        let direction =
            (forward + right * x * tangent * rect.aspect_ratio() + up * y * tangent).normalize();
        let source_eye = self.coordinates.source_point(eye);
        let source_direction = self.coordinates.source_direction(direction);
        let shape = self
            .objects
            .iter()
            .filter_map(|object| {
                let rotation = Quat::from_array(object.rotation).normalize();
                let inverse = rotation.inverse();
                let origin = inverse * (source_eye - Vec3::from(object.position));
                let ray = inverse * source_direction;
                let hit = if self.settings[4] == 1 {
                    intersect_geometry_sided(origin, ray, &object.geometry, false)
                } else {
                    intersect_geometry(origin, ray, &object.geometry)
                };
                hit.map(|distance| distance * self.coordinates.scale)
                    .filter(|distance| {
                        (self.near..=self.far).contains(&(distance * direction.dot(forward)))
                    })
                    .map(|distance| (distance, object.id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let point = self
            .point_clouds
            .clouds
            .iter()
            .filter(|cloud| {
                if cloud.kind == crate::point_clouds::Kind::Cloth {
                    self.show_cloth
                } else {
                    self.show_particles
                }
            })
            .flat_map(|cloud| {
                cloud.points.iter().filter_map(|point| {
                    ray_sphere(source_eye - *point, source_direction, self.point_radius)
                        .map(|distance| distance * self.coordinates.scale)
                        .filter(|distance| {
                            (self.near..=self.far).contains(&(distance * direction.dot(forward)))
                        })
                        .map(|distance| (distance, cloud.id))
                })
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        shape
            .into_iter()
            .chain(point)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }
}

pub fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut textures: ResMut<EguiUserTextures>,
    mut settings: ResMut<EguiGlobalSettings>,
    mut viewport: ResMut<Viewport>,
    render_check: Option<ResMut<RenderCheck>>,
) {
    settings.auto_create_primary_context = false;
    let size = Extent3d {
        width: 512,
        height: 512,
        depth_or_array_layers: 1,
    };
    let mut image = Image {
        texture_descriptor: TextureDescriptor {
            label: Some("pvd-scene-viewport"),
            size,
            dimension: TextureDimension::D2,
            format: TextureFormat::Bgra8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        ..default()
    };
    image.resize(size);
    viewport.image = images.add(image);
    viewport.texture = textures.add_image(EguiTextureHandle::Strong(viewport.image.clone()));
    commands.spawn((
        Camera3d::default(),
        bevy::core_pipeline::tonemapping::Tonemapping::Reinhard,
        Camera {
            order: -1,
            clear_color: ClearColorConfig::Custom(Color::srgb(0.045, 0.059, 0.078)),
            ..default()
        },
        RenderTarget::Image(viewport.image.clone().into()),
        Projection::custom(crate::camera_projection::FinitePerspective::new(
            0.01,
            1_000_000.0,
        )),
        Transform::from_translation(viewport.position()).looking_at(viewport.target, Vec3::Y),
        ViewCamera,
    ));
    if let Some(mut check) = render_check {
        let target = images.add(Image::new_target_texture(
            check.size.x,
            check.size.y,
            TextureFormat::Bgra8UnormSrgb,
            None,
        ));
        check.target = target.clone();
        commands.spawn((
            PrimaryEguiContext,
            Camera2d,
            RenderLayers::layer(1),
            RenderTarget::Image(target.into()),
        ));
    } else {
        commands.spawn((PrimaryEguiContext, Camera2d, RenderLayers::layer(1)));
    }
    commands.spawn((
        DirectionalLight {
            illuminance: 8000.0,
            ..default()
        },
        Transform::from_xyz(6.0, 10.0, 5.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.insert_resource(GlobalAmbientLight {
        color: Color::WHITE,
        brightness: 100.0,
        ..default()
    });
}

fn render_check_clip_range(first_frame: usize, frame_count: usize) -> Option<(usize, usize)> {
    let last = frame_count.checked_sub(1)?;
    (first_frame <= last).then_some(((frame_count / 4).max(first_frame), last))
}

pub fn render_check(
    mut commands: Commands,
    mut check: ResMut<RenderCheck>,
    mut studio: ResMut<Studio>,
    mut contexts: bevy_egui::EguiContexts,
    mut exit: MessageWriter<bevy::app::AppExit>,
) {
    if let Some(completion) = check.completion.clone() {
        if !check.exit_sent && studio.poll_shutdown() {
            check.exit_sent = true;
            exit.write(completion);
        }
        return;
    }
    check.frames += 1;
    let language = studio.prefs.language;
    if check.frames > 18_000 {
        eprintln!(
            "{}: {}",
            language.text("Render check timed out", "渲染检查超时"),
            studio
                .notices
                .iter()
                .map(|notice| notice.text(language))
                .collect::<Vec<_>>()
                .join("; ")
        );
        check.completion = Some(bevy::app::AppExit::error());
        return;
    }

    if !check.seek_requested && studio.recording.is_some() && studio.job.is_none() {
        check.seek_requested = true;
        if let Some(frame) = check.desired_frame {
            let target = frame.min(studio.count().saturating_sub(1));
            studio.seek(target);
        }
    }
    if !check.prepared && studio.recording.is_some() && studio.job.is_none() {
        if studio.seeking.is_some() {
            return;
        }
        if check
            .desired_frame
            .is_some_and(|frame| studio.frame != frame.min(studio.count().saturating_sub(1)))
        {
            eprintln!("Render check could not seek to the requested frame");
            check.completion = Some(bevy::app::AppExit::error());
            return;
        }
        check.prepared = true;
        // Scene entities and render pipelines need frames after the background load completes.
        check.frames = 0;
        studio.selected = studio
            .state
            .render_scene()
            .objects
            .iter()
            .find(|object| object.dynamic)
            .map(|object| object.id);
        if matches!(
            check.panel.as_deref(),
            Some("coordinates-z-left" | "source-camera-z-left")
        ) {
            studio.prefs.up_axis = crate::coordinates::UpAxis::Z;
            studio.prefs.handedness = crate::coordinates::Handedness::Left;
            studio.prefs.meters_per_unit = 0.1;
            studio.prefs.show_bounds = true;
            studio.prefs.show_local_axes = true;
        }
        if matches!(
            check.panel.as_deref(),
            Some("camera-fly" | "help-hidden-fly")
        ) {
            studio.prefs.camera_mode = crate::camera_controller::CameraMode::Fly;
        }
        if matches!(
            check.panel.as_deref(),
            Some("help-hidden" | "help-hidden-fly")
        ) {
            studio.prefs.show_viewport_help = false;
        }
        if matches!(
            check.panel.as_deref(),
            Some("source-camera" | "source-camera-z-left")
        ) {
            let coordinates = Coordinates::new(
                studio.prefs.up_axis,
                studio.prefs.handedness,
                studio.prefs.meters_per_unit,
            );
            studio.captured_camera = studio
                .state
                .cameras
                .iter()
                .find(|(_, pose)| {
                    crate::captured_camera::Pose::from_source(pose, coordinates).is_some()
                })
                .map(|(name, _)| name.clone());
            if studio.captured_camera.is_none() {
                eprintln!("Render check has no valid source camera in the requested frame");
                check.completion = Some(bevy::app::AppExit::error());
                return;
            }
            studio.focus_selected = false;
            studio.follow_selected = false;
        }
        if check.panel.as_deref() == Some("profile-events") {
            studio.profile_events_view = true;
            studio.profile_filter = "PvdStudio.Profile".into();
        }
        if check.panel.as_deref() == Some("profile-first-start") {
            studio.profile_events_view = true;
            studio.profile_filter.clear();
        }
        if check.panel.as_deref() == Some("thread-window") {
            studio.profile_events_view = false;
            studio.profile_filter.clear();
        }
        if matches!(
            check.panel.as_deref(),
            Some("overview" | "coordinates-z-left")
        ) {
            studio.selected = None;
            studio.focus_selected = true;
        }
        let panel = match check.panel.as_deref() {
            Some("performance" | "profile-events" | "profile-first-start" | "thread-window") => {
                Some(crate::studio::Panel::Performance)
            }
            Some("memory" | "memory-events" | "memory-end") => Some(crate::studio::Panel::Memory),
            Some("curves" | "frame-curves" | "event-curves") => Some(crate::studio::Panel::Curves),
            Some("diagnostics") => Some(crate::studio::Panel::Diagnostics),
            Some(
                "display"
                | "camera-fly"
                | "source-camera"
                | "source-camera-z-left"
                | "help-hidden"
                | "help-hidden-fly",
            ) => Some(crate::studio::Panel::Display),
            Some("clips" | "retained-capture") => Some(crate::studio::Panel::Clips),
            _ => None,
        };
        if let Some(panel) = panel {
            if check.panel.as_deref() == Some("clips")
                && let Some((first, last)) =
                    render_check_clip_range(studio.first_frame(), studio.count())
            {
                studio.clip_set.add(first, last);
                let frame = studio.frame;
                studio.clip_set.split(frame);
            }
            if check.panel.as_deref() == Some("memory-events") {
                studio.memory_panel.events = true;
            }
            if check.panel.as_deref() == Some("memory-end") {
                studio.memory_panel.at_end = true;
            }
            if let Some(location) = studio.prefs.dock.find_tab(&panel) {
                studio.prefs.dock.set_active_tab(location);
            }
            if check.panel.as_deref() == Some("event-curves") {
                if let Some(recording) = studio.recording.clone() {
                    let catalogue = pvd_core::profile_curves::catalog(
                        &recording,
                        &std::sync::atomic::AtomicBool::new(false),
                    );
                    match catalogue {
                        Ok(entries) => {
                            let keys: Vec<_> = entries
                                .into_iter()
                                .filter_map(|entry| {
                                    let name = recording
                                        .analysis
                                        .profile_event_name(entry.key.zone, entry.key.event)?;
                                    (name == "PvdStudio.Profile.Sample"
                                        || name == "PvdStudio.Profile.CrossFrame"
                                        || name == "PvdStudio.Profile.Pending")
                                        .then_some(entry.key)
                                })
                                .take(8)
                                .collect();
                            if let Ok(context) = contexts.ctx_mut() {
                                for key in keys {
                                    studio.plot_profile_event(key, context.clone());
                                }
                            }
                        }
                        Err(error) => studio.notice(crate::i18n::Message::detail(
                            "Profile catalog failed",
                            "性能事件目录失败",
                            error,
                        )),
                    }
                }
            } else if check.panel.as_deref() == Some("frame-curves") {
                if let Ok(context) = contexts.ctx_mut() {
                    studio.plot_performance(context.clone());
                }
            } else if panel == crate::studio::Panel::Curves {
                let property = studio
                    .selected
                    .and_then(|id| studio.state.objects.get(&id))
                    .and_then(|object| {
                        object
                            .properties
                            .get("GlobalPose")
                            .map(|_| "GlobalPose".to_owned())
                    });
                if let (Some(property), Ok(context)) = (property, contexts.ctx_mut()) {
                    for component in 0..3 {
                        studio.plot(property.clone(), component, context.clone());
                    }
                }
            }
        }
        if let Ok(context) = contexts.ctx_mut() {
            context.request_repaint();
        }
    }
    if check.prepared && check.panel.as_deref() == Some("retained-capture") {
        let Ok(context) = contexts.ctx_mut() else {
            return;
        };
        if let Err(error) = crate::recording_export::prepare_retained_render(context, &mut studio) {
            eprintln!("Retained-capture render preparation failed: {error}");
            check.completion = Some(bevy::app::AppExit::error());
            return;
        }
    }
    if check.prepared && check.panel.as_deref() == Some("thread-window") {
        let Ok(context) = contexts.ctx_mut() else {
            return;
        };
        match crate::profile::prepare_thread_window_render(context, &studio) {
            Ok(true) => (),
            Ok(false) => return,
            Err(error) => {
                eprintln!(
                    "Thread-window render preparation failed: {}",
                    error.text(language)
                );
                check.completion = Some(bevy::app::AppExit::error());
                return;
            }
        }
    }
    if check.prepared && check.panel.as_deref() == Some("profile-first-start") {
        let Ok(context) = contexts.ctx_mut() else {
            return;
        };
        match crate::profile::prepare_first_start_render(context, &studio) {
            Ok(true) => (),
            Ok(false) => return,
            Err(error) => {
                eprintln!(
                    "First-start catalog render preparation failed: {}",
                    error.text(language)
                );
                check.completion = Some(bevy::app::AppExit::error());
                return;
            }
        }
    }
    if check.frames > 90 && studio.job.is_none() && studio.curve_job.is_none() && !check.requested {
        if studio.seeking.is_some() {
            return;
        }
        if matches!(
            check.panel.as_deref(),
            Some("memory" | "memory-events" | "memory-end")
        ) {
            if !studio.analysis_ready() {
                eprintln!("Memory render check has no analysis for the displayed frame");
                check.completion = Some(bevy::app::AppExit::error());
                return;
            }
            if let Some(recording) = &studio.recording {
                println!(
                    "Memory render analysis loaded: {}",
                    serde_json::json!({
                        "frame": studio.frame,
                        "disk_backed": recording.is_disk_backed(),
                        "allocations": recording.analysis.memory_snapshot().summary().allocations,
                        "events": recording.analysis.memory_snapshot().event_count(),
                        "spans": recording.analysis.profile_spans().len(),
                    })
                );
            }
        }
        check.requested = true;
        if studio.recording.is_none() {
            eprintln!(
                "{}: {}",
                language.text(
                    "Render check has no decoded recording",
                    "渲染检查未获得已解码的录制"
                ),
                studio
                    .notices
                    .iter()
                    .map(|notice| notice.text(language))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            check.completion = Some(bevy::app::AppExit::error());
            return;
        }
        let path = check.output.clone();
        commands
            .spawn(bevy::render::view::screenshot::Screenshot::image(
                check.target.clone(),
            ))
            .observe(
                move |capture: On<bevy::render::view::screenshot::ScreenshotCaptured>,
                      mut check: ResMut<RenderCheck>| {
                    if check.completion.is_some() {
                        return;
                    }
                    match capture
                        .image
                        .clone()
                        .try_into_dynamic()
                        .map_err(|e| e.to_string())
                        .and_then(|image| image.to_rgb8().save(&path).map_err(|e| e.to_string()))
                    {
                        Ok(()) => {
                            println!(
                                "{}: {path}",
                                language.text("Rendered offscreen UI to", "离屏界面已保存至")
                            );
                            check.completion = Some(bevy::app::AppExit::Success);
                        }
                        Err(error) => {
                            eprintln!(
                                "{}: {error}",
                                language.text("Render check failed", "渲染检查失败")
                            );
                            check.completion = Some(bevy::app::AppExit::error());
                        }
                    }
                },
            );
    }
}

pub fn sync_scene(
    mut commands: Commands,
    mut studio: ResMut<Studio>,
    mut viewport: ResMut<Viewport>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let coordinates = Coordinates::new(
        studio.prefs.up_axis,
        studio.prefs.handedness,
        studio.prefs.meters_per_unit,
    );
    let coordinates_changed = coordinates != viewport.coordinates;
    let settings = [
        studio.prefs.show_static as u32,
        studio.prefs.show_dynamic as u32,
        studio.prefs.show_sleeping as u32,
        studio.prefs.show_kinematic as u32,
        studio.prefs.wireframe as u32,
        studio.prefs.opacity.to_bits(),
        studio.prefs.show_debug_draw as u32,
        studio.prefs.opacity_scope as u32,
        studio.prefs.show_particles as u32,
        studio.prefs.show_cloth_points as u32,
        studio.prefs.show_cloth_constraints as u32,
        studio.prefs.point_radius.to_bits(),
    ];
    if viewport.revision == studio.revision
        && viewport.selected == studio.selected
        && viewport.settings == settings
        && !coordinates_changed
    {
        return;
    }
    if coordinates.mirrored != viewport.coordinates.mirrored {
        for (_, mesh) in viewport.meshes.drain() {
            meshes.remove(mesh.id());
        }
        viewport.wires.clear();
    }
    if coordinates_changed {
        let ratio = coordinates.scale / viewport.coordinates.scale;
        viewport.target = coordinates.point(viewport.coordinates.source_point(viewport.target));
        viewport.distance *= ratio;
        if viewport.revision != u64::MAX {
            studio.prefs.camera_near *= ratio;
            studio.prefs.camera_far *= ratio;
        }
        viewport.coordinates = coordinates;
    }
    let revision = studio.revision;
    let changed = studio.scene_changes.take(viewport.revision, revision);
    if viewport.revision != revision {
        viewport
            .source_index
            .update(&studio.state, changed.as_ref());
    }
    let incremental = !coordinates_changed
        && viewport.settings == settings
        && viewport.selected == studio.selected;
    let actors = if incremental {
        changed.and_then(|changed| viewport.incremental.affected(&studio.state, changed))
    } else {
        None
    };
    let (mut scene, was_subset) = if let Some(actors) = &actors {
        (
            studio
                .state
                .render_actors_cached(actors.iter().copied(), &mut viewport.mesh_cache),
            true,
        )
    } else {
        (
            studio.state.render_scene_cached(&mut viewport.mesh_cache),
            false,
        )
    };
    let actors = actors.filter(|actors| {
        viewport
            .incremental
            .accepts(actors, &scene, &viewport.shared_geometry)
    });
    if actors.is_none() && was_subset {
        scene = studio.state.render_scene_cached(&mut viewport.mesh_cache);
    }
    let full_diagnostics = scene.diagnostics.is_empty();
    viewport.point_clouds = if viewport.source_index.complete {
        crate::point_clouds::PointClouds::collect_from_ids(
            &studio.state,
            &viewport.source_index.cloths,
            &viewport.source_index.particles,
        )
    } else {
        crate::point_clouds::PointClouds::collect(&studio.state)
    };
    viewport.point_radius = if studio.prefs.point_radius.is_finite() {
        studio.prefs.point_radius.clamp(0.0001, 1000.0)
    } else {
        0.025
    };
    viewport.show_particles = studio.prefs.show_particles;
    viewport.show_cloth = studio.prefs.show_cloth_points || studio.prefs.show_cloth_constraints;
    viewport.diagnostics = scene
        .diagnostics
        .into_iter()
        .map(|detail| {
            Message::new(
                format!("Scene data diagnostic: {detail}"),
                format!("场景数据诊断：{detail}"),
            )
        })
        .collect();
    let point_diagnostics = viewport.point_clouds.diagnostics.clone();
    viewport.diagnostics.extend(point_diagnostics);
    let mut invalid_debug = 0usize;
    let mut limited_lines = false;
    viewport.limit_lines.clear();
    let debug_count: usize = studio
        .state
        .debug_draw
        .values()
        .map(|batch| batch.len())
        .sum();
    if debug_count > MAX_DEBUG_PRIMITIVES {
        viewport.diagnostics.push(Message::new(
            format!("Debug display is limited to the first {MAX_DEBUG_PRIMITIVES} of {debug_count} primitives in this frame; captured data is retained."),
            format!("本帧包含 {debug_count} 个调试图元，仅显示前 {MAX_DEBUG_PRIMITIVES} 个；捕获数据仍完整保留。"),
        ));
    }
    for primitive in debug_primitives(&studio.state) {
        if is_joint_limit(primitive) {
            match pvd_core::joint_geometry::tessellate(primitive) {
                Ok(lines)
                    if lines
                        .iter()
                        .all(|line| line.positions.iter().all(|p| valid_position(*p))) =>
                {
                    let remaining = MAX_OVERLAY_LINES - viewport.limit_lines.len();
                    limited_lines |= lines.len() > remaining;
                    viewport
                        .limit_lines
                        .extend(lines.into_iter().take(remaining));
                }
                _ => invalid_debug += 1,
            }
        } else if !drawable_debug(primitive) {
            invalid_debug += 1;
        }
    }
    if limited_lines {
        viewport.diagnostics.push(Message::new(
            format!("Joint-limit display reached the {MAX_OVERLAY_LINES}-line budget; captured data is retained."),
            format!("关节限位显示达到 {MAX_OVERLAY_LINES} 条线的预算；捕获数据仍完整保留。"),
        ));
    }
    if invalid_debug > 0 {
        viewport.diagnostics.push(Message::new(
            format!("{invalid_debug} debug primitives have invalid coordinates, rotations or text sizes and cannot be drawn."),
            format!("{invalid_debug} 个调试图元的坐标、旋转或文字大小无效，无法绘制。"),
        ));
    }
    let mut queries = if viewport.source_index.complete {
        pvd_core::queries::collect_from_ids(&studio.state, &viewport.source_index.scenes)
    } else {
        pvd_core::queries::collect(&studio.state)
    };
    viewport
        .diagnostics
        .extend(queries.diagnostics.drain(..).map(|detail| {
            Message::new(
                format!("Scene query diagnostic: {detail}"),
                format!("场景查询诊断：{detail}"),
            )
        }));
    let (query_lines, query_limited) = query_lines(&queries);
    viewport.query_lines = query_lines;
    viewport.queries = queries;
    if query_limited {
        viewport.diagnostics.push(Message::new(
            format!("Query-shape display reached the {MAX_OVERLAY_LINES}-line budget; captured data is retained."),
            format!("查询形状显示达到 {MAX_OVERLAY_LINES} 条线的预算；捕获数据仍完整保留。"),
        ));
    }
    if studio.prefs.show_debug_draw
        && !studio.prefs.wireframe
        && let Some(mut mesh) = debug_triangles(&studio.state)
    {
        orient_winding(&mut mesh, coordinates.mirrored);
        if let Some(surface) = &viewport.debug_surface {
            if let Some(existing) = meshes.get_mut(&surface.mesh) {
                *existing = mesh;
            }
            if coordinates_changed {
                commands
                    .entity(surface.entity)
                    .insert(coordinates.transform(Transform::default()));
            }
        } else {
            let mesh = meshes.add(mesh);
            let material = materials.add(StandardMaterial {
                unlit: true,
                cull_mode: None,
                alpha_mode: AlphaMode::Blend,
                ..default()
            });
            let entity = commands
                .spawn((
                    Mesh3d(mesh.clone()),
                    MeshMaterial3d(material.clone()),
                    coordinates.transform(Transform::default()),
                    CapturedObject,
                ))
                .id();
            viewport.debug_surface = Some(DebugSurface {
                entity,
                mesh,
                material,
            });
        }
    } else if let Some(surface) = viewport.debug_surface.take() {
        commands.entity(surface.entity).despawn();
        meshes.remove(surface.mesh.id());
        materials.remove(surface.material.id());
    }
    if let Some(actors) = actors {
        incremental::sync_actors(
            &mut commands,
            &studio,
            &mut viewport,
            &mut meshes,
            &mut materials,
            actors,
            scene.objects,
        );
        viewport.revision = studio.revision;
        return;
    }
    let all_objects = scene.objects;
    let source = incremental::SourceResources::from_objects(&all_objects);
    let mut used_shared = HashSet::new();
    let mut invalid_shapes = 0;
    let objects: Vec<_> = all_objects
        .into_iter()
        .filter(|object| {
            let geometry_valid = if let Geometry::TriangleMesh { vertices, indices } =
                &object.geometry
            {
                let key = (
                    std::sync::Arc::as_ptr(vertices) as usize,
                    std::sync::Arc::as_ptr(indices) as usize,
                );
                used_shared.insert(key);
                let entry = viewport
                    .shared_geometry
                    .entry(key)
                    .or_insert_with(|| SharedGeometry {
                        vertices: vertices.clone(),
                        indices: indices.clone(),
                        hash: geometry_hash(&object.geometry),
                        valid: valid_geometry(&object.geometry),
                    });
                debug_assert!(std::sync::Arc::ptr_eq(&entry.vertices, vertices));
                debug_assert!(std::sync::Arc::ptr_eq(&entry.indices, indices));
                entry.valid
            } else {
                valid_geometry(&object.geometry)
            };
            let valid = valid_pose(object) && geometry_valid;
            invalid_shapes += usize::from(!valid);
            valid
        })
        .filter(|object| incremental::actor_visible(&studio, object))
        .collect();
    viewport.invalid_shapes = invalid_shapes;
    viewport
        .shared_geometry
        .retain(|key, _| used_shared.contains(key));
    let mut used = HashSet::new();
    let mut used_materials = HashSet::new();
    let mut used_objects = HashSet::new();
    viewport.wire_keys.clear();
    let mut asset_uses = HashMap::new();
    for object in &objects {
        if let Some(usage) = sync_object(
            &mut commands,
            &studio,
            &mut viewport,
            &mut meshes,
            &mut materials,
            object,
        ) {
            used.insert(usage.mesh);
            used_materials.insert(usage.material);
            used_objects.insert((object.id, object.shape_id));
            asset_uses.insert((object.id, object.shape_id), usage);
        }
    }
    viewport.rendered.retain(|key, object| {
        if used_objects.contains(key) {
            true
        } else {
            commands.entity(object.entity).despawn();
            false
        }
    });
    viewport.meshes.retain(|key, mesh| {
        if used.contains(key) {
            true
        } else {
            meshes.remove(mesh.id());
            false
        }
    });
    viewport.materials.retain(|key, material| {
        if used_materials.contains(key) {
            true
        } else {
            materials.remove(material.id());
            false
        }
    });
    viewport.wires.retain(|key, _| used.contains(key));
    viewport.objects = objects;
    viewport.incremental = incremental::SceneIndex::rebuild(
        &studio.state,
        &viewport.objects,
        asset_uses,
        source,
        full_diagnostics && invalid_shapes == 0,
    );
    viewport.revision = studio.revision;
    viewport.selected = studio.selected;
    viewport.settings = settings;
}

fn sync_object(
    commands: &mut Commands,
    studio: &Studio,
    viewport: &mut Viewport,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    object: &RenderObject,
) -> Option<incremental::AssetUse> {
    let key = match &object.geometry {
        Geometry::TriangleMesh { vertices, indices } => {
            viewport.shared_geometry[&(
                std::sync::Arc::as_ptr(vertices) as usize,
                std::sync::Arc::as_ptr(indices) as usize,
            )]
                .hash
        }
        _ => geometry_hash(&object.geometry),
    };
    viewport.wire_keys.insert(object.shape_id, key);
    let mesh = if let Some(mesh) = viewport.meshes.get(&key) {
        mesh.clone()
    } else {
        let mut mesh = geometry_mesh(&object.geometry)?;
        viewport.wires.insert(key, mesh_edges(&mesh));
        orient_winding(&mut mesh, viewport.coordinates.mirrored);
        let handle = meshes.add(mesh);
        viewport.meshes.insert(key, handle.clone());
        handle
    };
    let selected = studio
        .selected
        .is_some_and(|id| id == object.id || id == object.shape_id);
    let tint = if selected {
        0
    } else if object.dynamic && is_kinematic(&studio.state, object.id) {
        1
    } else if object.sleeping {
        2
    } else if object.dynamic {
        3
    } else {
        4
    };
    let opacity = if studio.prefs.opacity_scope.includes(selected) {
        studio.prefs.opacity.clamp(0.1, 1.0)
    } else {
        1.0
    };
    let material_key = MaterialKey {
        tint,
        opacity: opacity.to_bits(),
    };
    let material = viewport
        .materials
        .entry(material_key)
        .or_insert_with(|| {
            let [r, g, b] = [
                [0.24, 0.82, 0.92],
                [0.68, 0.47, 0.81],
                [0.36, 0.43, 0.52],
                [0.56, 0.67, 0.79],
                [0.28, 0.34, 0.42],
            ][usize::from(tint)];
            materials.add(StandardMaterial {
                base_color: Color::srgba(r, g, b, opacity),
                perceptual_roughness: 0.8,
                alpha_mode: if opacity < 1.0 {
                    AlphaMode::Blend
                } else {
                    AlphaMode::Opaque
                },
                ..default()
            })
        })
        .clone();
    let mut rotation = Quat::from_array(object.rotation).normalize();
    if matches!(object.geometry, Geometry::Capsule { .. }) {
        rotation *= Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    }
    let transform = viewport.coordinates.transform(
        Transform::from_translation(Vec3::from(object.position)).with_rotation(rotation),
    );
    let visible = !studio.prefs.wireframe;
    let object_key = (object.id, object.shape_id);
    sync_rendered_object(
        commands,
        &mut viewport.rendered,
        object_key,
        mesh,
        material,
        transform,
        visible,
    );
    Some(incremental::AssetUse {
        mesh: key,
        material: material_key,
    })
}

fn sync_rendered_object(
    commands: &mut Commands,
    rendered: &mut HashMap<(u64, u64), RenderedObject>,
    key: (u64, u64),
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
    transform: Transform,
    visible: bool,
) {
    let visibility = if visible {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if let Some(object) = rendered.get_mut(&key) {
        let mut entity = commands.entity(object.entity);
        if object.mesh != mesh {
            entity.insert(Mesh3d(mesh.clone()));
            object.mesh = mesh;
        }
        if object.material != material {
            entity.insert(MeshMaterial3d(material.clone()));
            object.material = material;
        }
        if object.transform != transform {
            entity.insert(transform);
            object.transform = transform;
        }
        if object.visible != visible {
            entity.insert(visibility);
            object.visible = visible;
        }
    } else {
        let entity = commands
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                transform,
                visibility,
                CapturedObject,
            ))
            .id();
        rendered.insert(
            key,
            RenderedObject {
                entity,
                mesh,
                material,
                transform,
                visible,
            },
        );
    }
}

fn geometry_hash(geometry: &Geometry) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(geometry).hash(&mut hash);
    match geometry {
        Geometry::Box { half_extents } => half_extents.map(f32::to_bits).hash(&mut hash),
        Geometry::Sphere { radius } => radius.to_bits().hash(&mut hash),
        Geometry::Capsule {
            radius,
            half_height,
        } => {
            radius.to_bits().hash(&mut hash);
            half_height.to_bits().hash(&mut hash);
        }
        Geometry::Plane => (),
        Geometry::TriangleMesh { vertices, indices } => {
            for vertex in vertices.iter() {
                vertex.map(f32::to_bits).hash(&mut hash);
            }
            indices.hash(&mut hash);
        }
    }
    hash.finish()
}

fn geometry_radius(geometry: &Geometry) -> f32 {
    match geometry {
        Geometry::Box { half_extents } => Vec3::from(*half_extents).length(),
        Geometry::Sphere { radius } => *radius,
        Geometry::Capsule {
            radius,
            half_height,
        } => radius + half_height,
        Geometry::Plane => 3.0,
        Geometry::TriangleMesh { vertices, .. } => vertices
            .iter()
            .map(|v| Vec3::from(*v).length())
            .fold(0.0, f32::max),
    }
}

fn geometry_mesh(geometry: &Geometry) -> Option<Mesh> {
    if !valid_geometry(geometry) {
        return None;
    }
    match geometry {
        Geometry::Box { half_extents } => Some(
            Cuboid::new(
                half_extents[0] * 2.0,
                half_extents[1] * 2.0,
                half_extents[2] * 2.0,
            )
            .into(),
        ),
        Geometry::Sphere { radius } => Some(Sphere::new(*radius).mesh().uv(24, 16)),
        Geometry::Capsule {
            radius,
            half_height,
        } => Some(Capsule3d::new(*radius, *half_height * 2.0).mesh().build()),
        Geometry::Plane => Some(Plane3d::new(Vec3::X, Vec2::splat(100.0)).mesh().build()),
        Geometry::TriangleMesh { vertices, indices } => {
            if vertices.is_empty()
                || indices.len() % 3 != 0
                || indices.iter().any(|i| *i as usize >= vertices.len())
            {
                return None;
            }
            let mut mesh = Mesh::new(
                PrimitiveTopology::TriangleList,
                RenderAssetUsages::default(),
            );
            mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, vertices.as_ref().clone());
            mesh.insert_indices(Indices::U32(indices.as_ref().clone()));
            mesh.duplicate_vertices();
            mesh.compute_flat_normals();
            Some(mesh)
        }
    }
}

fn valid_position(value: [f32; 3]) -> bool {
    value.iter().all(|v| v.is_finite() && v.abs() <= 1e12)
}

#[cfg(test)]
fn valid_object(object: &RenderObject) -> bool {
    valid_pose(object) && valid_geometry(&object.geometry)
}

fn valid_pose(object: &RenderObject) -> bool {
    let rotation = Quat::from_array(object.rotation);
    valid_position(object.position)
        && rotation.is_finite()
        && rotation.length_squared().is_finite()
        && rotation.length_squared() > 1e-12
}

fn valid_geometry(geometry: &Geometry) -> bool {
    let positive = |value: f32| value.is_finite() && value > 0.0 && value <= 1e12;
    match geometry {
        Geometry::Box { half_extents } => half_extents.iter().all(|v| positive(*v)),
        Geometry::Sphere { radius } => positive(*radius),
        Geometry::Capsule {
            radius,
            half_height,
        } => {
            positive(*radius)
                && half_height.is_finite()
                && *half_height >= 0.0
                && *half_height <= 1e12
        }
        Geometry::Plane => true,
        Geometry::TriangleMesh { vertices, indices } => {
            !vertices.is_empty()
                && indices.len() >= 3
                && indices.len().is_multiple_of(3)
                && vertices.iter().all(|v| valid_position(*v))
                && indices
                    .iter()
                    .all(|index| (*index as usize) < vertices.len())
        }
    }
}

pub fn update_camera(
    mut viewport: ResMut<Viewport>,
    mut studio: ResMut<Studio>,
    mut camera: Query<(&mut Transform, &mut Projection), With<ViewCamera>>,
) {
    if studio.focus_selected || studio.follow_selected {
        studio.captured_camera = None;
    }
    crate::captured_camera::sync(&studio, &mut viewport);
    if studio.focus_selected {
        viewport.focus(studio.selected);
        studio.focus_selected = false;
    }
    if studio.follow_selected
        && let Some(object) = viewport.objects.iter().find(|o| {
            studio
                .selected
                .is_some_and(|id| id == o.id || id == o.shape_id)
        })
    {
        let position = viewport.coordinates.point(Vec3::from(object.position));
        viewport.target = position;
    } else if studio.follow_selected
        && let Some(cloud) = viewport
            .point_clouds
            .clouds
            .iter()
            .find(|cloud| Some(cloud.id) == studio.selected)
    {
        let (min, max) = cloud.points.iter().fold(
            (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
            |(min, max), point| (min.min(*point), max.max(*point)),
        );
        if min.is_finite() && max.is_finite() {
            viewport.target = viewport.coordinates.point((min + max) * 0.5);
        }
    }
    let scale = viewport.coordinates.scale;
    let near = if studio.prefs.camera_near.is_finite() {
        studio.prefs.camera_near.clamp(1e-7 * scale, 1e6 * scale)
    } else {
        0.01 * scale
    };
    let far = if studio.prefs.camera_far.is_finite() {
        studio.prefs.camera_far.clamp(near * 1.01, 1e12 * scale)
    } else {
        (1_000_000.0 * scale).max(near * 1.01)
    };
    viewport.near = near;
    viewport.far = far;
    studio.prefs.camera_near = near;
    studio.prefs.camera_far = far;
    studio.prefs.camera_speed = if studio.prefs.camera_speed.is_finite() {
        studio.prefs.camera_speed.clamp(0.05, 20.0)
    } else {
        1.0
    };
    if let Ok((mut transform, mut projection)) = camera.single_mut() {
        *transform = Transform::from_translation(viewport.position())
            .looking_at(viewport.view_target(), viewport.view_up());
        if let Projection::Custom(custom) = &mut *projection
            && let Some(perspective) =
                custom.get_mut::<crate::camera_projection::FinitePerspective>()
        {
            perspective.set_clip(near, far);
        }
    }
}

pub fn draw_grid(
    mut raw_gizmos: Gizmos,
    studio: Res<Studio>,
    viewport: Res<Viewport>,
    #[cfg(test)] output: Option<ResMut<selected_overlay_tests::Output>>,
) {
    let mut gizmos = SourceGizmos {
        inner: &mut raw_gizmos,
        coordinates: viewport.coordinates,
    };
    let marker_scale = display_scale(studio.prefs.gizmo_scale);
    for cloud in &viewport.point_clouds.clouds {
        let cloth = cloud.kind == crate::point_clouds::Kind::Cloth;
        if !(if cloth {
            studio.prefs.show_cloth_points
        } else {
            studio.prefs.show_particles
        }) {
            continue;
        }
        let color = if studio.selected == Some(cloud.id) {
            Color::srgb(0.25, 0.85, 0.9)
        } else if cloud.inactive {
            Color::srgb(0.5, 0.55, 0.62)
        } else if cloud.kind == crate::point_clouds::Kind::Fluid {
            Color::srgb(0.35, 0.65, 1.0)
        } else if cloth {
            Color::srgb(0.85, 0.74, 0.5)
        } else {
            Color::srgb(0.64, 0.86, 0.56)
        };
        for point in &cloud.points {
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                gizmos.line(
                    *point - axis * viewport.point_radius,
                    *point + axis * viewport.point_radius,
                    color,
                );
            }
        }
    }
    if studio.prefs.show_cloth_constraints {
        for (id, [a, b]) in &viewport.point_clouds.constraints {
            gizmos.line(
                *a,
                *b,
                if studio.selected == Some(*id) {
                    Color::srgb(0.25, 0.85, 0.9)
                } else {
                    Color::srgb(0.61, 0.56, 0.44)
                },
            );
        }
    }
    for ray in &viewport.queries.raycasts {
        if studio.prefs.show_query_rays {
            let start = Vec3::from(ray.origin);
            let end = start + Vec3::from(ray.direction) * ray.distance;
            if valid_position(start.to_array()) && valid_position(end.to_array()) {
                gizmos.line(
                    start,
                    end,
                    if ray.hits.is_empty() {
                        Color::srgb(0.4, 0.6, 0.8)
                    } else {
                        Color::srgb(0.89, 0.75, 0.3)
                    },
                );
            }
        }
    }
    for line in &viewport.query_lines {
        if if line.overlap {
            studio.prefs.show_query_overlaps
        } else {
            studio.prefs.show_query_sweeps
        } {
            gizmos.line(line.positions[0], line.positions[1], line.color);
        }
    }
    for hit in viewport
        .queries
        .raycasts
        .iter()
        .flat_map(|ray| &ray.hits)
        .chain(viewport.queries.sweeps.iter().flat_map(|sweep| &sweep.hits))
    {
        let Some(position) = hit.position.filter(|position| valid_position(*position)) else {
            continue;
        };
        let position = Vec3::from(position);
        if studio.prefs.show_query_hits {
            let size = (viewport.view_distance() * 0.002).max(0.015 * viewport.coordinates.scale)
                * marker_scale
                / viewport.coordinates.scale;
            for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                gizmos.line(
                    position - axis * size,
                    position + axis * size,
                    Color::srgb(0.5, 1.0, 0.6),
                );
            }
        }
        if studio.prefs.show_query_normals
            && let Some(normal) = hit.normal.filter(|normal| valid_position(*normal))
        {
            gizmos.arrow(
                position,
                position + Vec3::from(normal) * 0.4 * marker_scale,
                Color::srgb(0.5, 1.0, 0.6),
            );
        }
    }
    if studio.prefs.wireframe || studio.prefs.show_bounds {
        for object in &viewport.objects {
            let Some(edges) = viewport
                .wire_keys
                .get(&object.shape_id)
                .and_then(|key| viewport.wires.get(key))
            else {
                continue;
            };
            let mut rotation = Quat::from_array(object.rotation).normalize();
            if matches!(object.geometry, Geometry::Capsule { .. }) {
                rotation *= Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
            }
            let transform =
                Transform::from_translation(Vec3::from(object.position)).with_rotation(rotation);
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for edge in edges {
                let [a, b] = edge.map(|point| transform.transform_point(point));
                if studio.prefs.wireframe {
                    gizmos.line(
                        a,
                        b,
                        if studio.selected == Some(object.id) {
                            Color::srgb(0.25, 0.85, 0.9)
                        } else {
                            Color::srgb(0.6, 0.68, 0.76)
                        },
                    );
                }
                min = min.min(a).min(b);
                max = max.max(a).max(b);
            }
            if studio.prefs.show_bounds
                && studio.prefs.bounds_scope.includes(
                    studio
                        .selected
                        .is_some_and(|id| id == object.id || id == object.shape_id),
                )
                && !matches!(object.geometry, Geometry::Plane)
                && min.is_finite()
                && max.is_finite()
            {
                let corners = [
                    Vec3::new(min.x, min.y, min.z),
                    Vec3::new(max.x, min.y, min.z),
                    Vec3::new(max.x, max.y, min.z),
                    Vec3::new(min.x, max.y, min.z),
                    Vec3::new(min.x, min.y, max.z),
                    Vec3::new(max.x, min.y, max.z),
                    Vec3::new(max.x, max.y, max.z),
                    Vec3::new(min.x, max.y, max.z),
                ];
                for (a, b) in [
                    (0, 1),
                    (1, 2),
                    (2, 3),
                    (3, 0),
                    (4, 5),
                    (5, 6),
                    (6, 7),
                    (7, 4),
                    (0, 4),
                    (1, 5),
                    (2, 6),
                    (3, 7),
                ] {
                    gizmos.line(corners[a], corners[b], Color::srgb(0.93, 0.73, 0.35));
                }
            }
        }
    }
    if studio.prefs.show_com || studio.prefs.show_velocity || studio.prefs.show_local_axes {
        let selected_actor = studio.selected.map(|selected| {
            viewport
                .objects
                .iter()
                .find(|object| object.shape_id == selected)
                .map_or(selected, |object| object.id)
        });
        let ids: HashSet<_> = viewport.objects.iter().map(|o| o.id).collect();
        for id in ids {
            let Some(object) = studio.state.objects.get(&id) else {
                continue;
            };
            let Some(pvd_core::Value::Transform { position, rotation }) =
                object.properties.get("GlobalPose")
            else {
                continue;
            };
            if !valid_position(*position) {
                continue;
            }
            let position = Vec3::from(*position);
            let rotation = Quat::from_array(*rotation);
            if !rotation.is_finite() || rotation.length_squared() <= 1e-12 {
                continue;
            }
            let rotation = rotation.normalize();
            let selected = selected_actor == Some(id);
            if studio.prefs.show_local_axes && studio.prefs.local_axes_scope.includes(selected) {
                gizmos.axes(
                    Transform::from_translation(position).with_rotation(rotation),
                    0.5 * marker_scale,
                );
            }
            if studio.prefs.show_com
                && studio.prefs.com_scope.includes(selected)
                && let Some(pvd_core::Value::Transform {
                    position: local,
                    rotation: local_rotation,
                }) = object.properties.get("CMassLocalPose")
            {
                let local_rotation = Quat::from_array(*local_rotation);
                if valid_position(*local)
                    && local_rotation.is_finite()
                    && local_rotation.length_squared() > 1e-12
                {
                    gizmos.axes(
                        Transform::from_translation(position + rotation * Vec3::from(*local))
                            .with_rotation(rotation * local_rotation.normalize()),
                        0.35 * marker_scale,
                    );
                }
            }
            if studio.prefs.show_velocity && studio.prefs.velocity_scope.includes(selected) {
                for (property, color) in [
                    ("LinearVelocity", Color::srgb(0.38, 0.9, 0.56)),
                    ("AngularVelocity", Color::srgb(0.85, 0.53, 0.92)),
                ] {
                    if let Some(pvd_core::Value::Vec3(value)) = object.properties.get(property)
                        && valid_position(*value)
                    {
                        let vector =
                            Vec3::from(*value) * display_scale(studio.prefs.velocity_scale);
                        if property == "AngularVelocity" {
                            gizmos.axial_arrow(position, vector, color);
                        } else {
                            gizmos.arrow(position, position + vector, color);
                        }
                    }
                }
            }
        }
    }
    if studio.prefs.show_contacts {
        for contacts in studio.state.contacts.values() {
            for contact in contacts.iter() {
                let position = Vec3::from(contact.position);
                let normal = Vec3::from(contact.normal);
                gizmos.line(
                    position,
                    position + normal * 0.35 * marker_scale,
                    Color::srgb(1.0, 0.62, 0.26),
                );
                for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                    gizmos.line(
                        position - axis * 0.025 * marker_scale,
                        position + axis * 0.025 * marker_scale,
                        Color::srgb(1.0, 0.8, 0.42),
                    );
                }
            }
        }
    }
    if studio.prefs.show_debug_draw {
        for line in &viewport.limit_lines {
            gizmos.line(
                Vec3::from(line.positions[0]),
                Vec3::from(line.positions[1]),
                argb(line.color),
            );
        }
        use pvd_core::debug_draw::Primitive;
        for primitive in debug_primitives(&studio.state) {
            if !drawable_debug(primitive) {
                continue;
            }
            match primitive {
                Primitive::Point {
                    position, color, ..
                } => {
                    let p = Vec3::from(*position);
                    let r = (viewport.view_distance() * 0.001)
                        .max(0.005 * viewport.coordinates.scale)
                        * marker_scale
                        / viewport.coordinates.scale;
                    for axis in [Vec3::X, Vec3::Y, Vec3::Z] {
                        gizmos.line(p - axis * r, p + axis * r, argb(*color));
                    }
                }
                Primitive::Line {
                    positions, colors, ..
                } => {
                    gizmos.line_gradient(
                        Vec3::from(positions[0]),
                        Vec3::from(positions[1]),
                        argb(colors[0]),
                        argb(colors[1]),
                    );
                }
                Primitive::Triangle {
                    positions, colors, ..
                } if studio.prefs.wireframe => {
                    for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                        gizmos.line_gradient(
                            Vec3::from(positions[a]),
                            Vec3::from(positions[b]),
                            argb(colors[a]),
                            argb(colors[b]),
                        );
                    }
                }
                Primitive::JointFrames { parent, child, .. } => {
                    for pose in [parent, child] {
                        gizmos.axes(
                            Transform::from_translation(Vec3::from(pose.position))
                                .with_rotation(Quat::from_array(pose.rotation)),
                            0.5 * display_scale(studio.prefs.joint_frame_scale),
                        );
                    }
                }
                _ => (),
            }
        }
    }
    if studio.prefs.show_grid {
        gizmos.coordinates = Coordinates::default();
        let step = 10_f32
            .powf((viewport.view_distance() / 10.0).log10().floor())
            .max(0.1 * viewport.coordinates.scale);
        let center = (viewport.view_target() / step).round() * step;
        for line in -20..=20 {
            let p = line as f32 * step;
            let color = Color::srgba(0.28, 0.35, 0.43, if line % 5 == 0 { 0.45 } else { 0.2 });
            gizmos.line(
                Vec3::new(center.x + p, 0.0, center.z - step * 20.0),
                Vec3::new(center.x + p, 0.0, center.z + step * 20.0),
                color,
            );
            gizmos.line(
                Vec3::new(center.x - step * 20.0, 0.0, center.z + p),
                Vec3::new(center.x + step * 20.0, 0.0, center.z + p),
                color,
            );
        }
    }
    gizmos.coordinates = viewport.coordinates;
    if studio.prefs.show_axes {
        gizmos.line(Vec3::ZERO, Vec3::X * 2.0, Color::srgb(0.9, 0.3, 0.35));
        gizmos.line(Vec3::ZERO, Vec3::Y * 2.0, Color::srgb(0.38, 0.8, 0.5));
        gizmos.line(Vec3::ZERO, Vec3::Z * 2.0, Color::srgb(0.35, 0.6, 0.95));
    }
    #[cfg(test)]
    if let Some(mut output) = output {
        output.capture(&raw_gizmos);
    }
}

struct SourceGizmos<'a, 'w, 's> {
    inner: &'a mut Gizmos<'w, 's>,
    coordinates: Coordinates,
}

impl SourceGizmos<'_, '_, '_> {
    fn axial_arrow(&mut self, origin: Vec3, vector: Vec3, color: Color) {
        let start = self.coordinates.point(origin);
        let end = start + self.coordinates.axial_direction(vector) * self.coordinates.scale;
        self.inner.arrow(start, end, color);
    }
    fn line(&mut self, a: Vec3, b: Vec3, color: Color) {
        self.inner
            .line(self.coordinates.point(a), self.coordinates.point(b), color);
    }
    fn line_gradient(&mut self, a: Vec3, b: Vec3, first: Color, last: Color) {
        self.inner.line_gradient(
            self.coordinates.point(a),
            self.coordinates.point(b),
            first,
            last,
        );
    }
    fn arrow(&mut self, a: Vec3, b: Vec3, color: Color) {
        self.inner
            .arrow(self.coordinates.point(a), self.coordinates.point(b), color);
    }
    fn axes(&mut self, transform: Transform, length: f32) {
        for (axis, color) in [
            (Vec3::X, Color::srgb(1.0, 0.0, 0.0)),
            (Vec3::Y, Color::srgb(0.0, 1.0, 0.0)),
            (Vec3::Z, Color::srgb(0.0, 0.0, 1.0)),
        ] {
            self.arrow(
                transform.translation,
                transform.transform_point(axis * length),
                color,
            );
        }
    }
}

fn orient_winding(mesh: &mut Mesh, mirrored: bool) {
    if !mirrored {
        return;
    }
    let mut indices: Vec<u32> = mesh
        .indices()
        .map(|indices| indices.iter().map(|i| i as u32).collect())
        .unwrap_or_else(|| (0..mesh.count_vertices() as u32).collect());
    for triangle in indices.chunks_exact_mut(3) {
        triangle.swap(1, 2);
    }
    mesh.insert_indices(Indices::U32(indices));
}

pub(crate) fn drawable_debug(primitive: &pvd_core::debug_draw::Primitive) -> bool {
    use pvd_core::debug_draw::Primitive;
    match primitive {
        Primitive::Point { position, .. } => valid_position(*position),
        Primitive::Line { positions, .. } => positions.iter().all(|p| valid_position(*p)),
        Primitive::Triangle { positions, .. } => positions.iter().all(|p| valid_position(*p)),
        Primitive::Text { position, size, .. } => {
            valid_position(*position) && size.is_finite() && *size > 0.0
        }
        Primitive::JointFrames { parent, child, .. } => [parent, child].iter().all(|pose| {
            valid_position(pose.position)
                && pose.rotation.iter().all(|v| v.is_finite())
                && (1e-6..=1e12).contains(&Quat::from_array(pose.rotation).length_squared())
        }),
        _ => false,
    }
}

fn is_joint_limit(primitive: &pvd_core::debug_draw::Primitive) -> bool {
    use pvd_core::debug_draw::Primitive;
    matches!(
        primitive,
        Primitive::LinearLimit { .. }
            | Primitive::AngularLimit { .. }
            | Primitive::LimitCone { .. }
            | Primitive::DoubleCone { .. }
    )
}

fn debug_triangles(state: &pvd_core::SceneState) -> Option<Mesh> {
    let mut positions = Vec::<[f32; 3]>::new();
    let mut normals = Vec::<[f32; 3]>::new();
    let mut colors = Vec::<[f32; 4]>::new();
    for primitive in debug_primitives(state) {
        if let pvd_core::debug_draw::Primitive::Triangle {
            positions: triangle,
            colors: tint,
            ..
        } = primitive
            && triangle.iter().all(|p| valid_position(*p))
        {
            let [a, b, c] = triangle.map(Vec3::from);
            let Some(normal) = (b - a).cross(c - a).try_normalize() else {
                continue;
            };
            positions.extend(triangle);
            normals.extend([normal.to_array(); 3]);
            colors.extend(tint.map(|color| argb(color).to_linear().to_f32_array()));
        }
    }
    (!positions.is_empty()).then(|| {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colors)
    })
}

fn query_lines(queries: &pvd_core::queries::QueryScene) -> (Vec<QueryLine>, bool) {
    let mut lines = Vec::new();
    let mut cache = HashMap::<u64, Vec<[Vec3; 2]>>::new();
    let mut cached_edges = 0usize;
    let mut limited = false;
    let shapes = queries
        .sweeps
        .iter()
        .flat_map(|sweep| {
            let start = Vec3::from(sweep.geometry.position);
            let end = start + Vec3::from(sweep.direction) * sweep.distance;
            let hit = !sweep.hits.is_empty();
            [
                (&sweep.geometry, start, false, hit),
                (&sweep.geometry, end, false, false),
            ]
        })
        .chain(queries.overlaps.iter().map(|overlap| {
            (
                &overlap.geometry,
                Vec3::from(overlap.geometry.position),
                true,
                !overlap.hits.is_empty(),
            )
        }));
    for (shape, position, overlap, hit) in shapes {
        if lines.len() == MAX_OVERLAY_LINES {
            limited = true;
            break;
        }
        if !valid_position(position.to_array()) || !valid_geometry(&shape.geometry) {
            continue;
        }
        let key = geometry_hash(&shape.geometry);
        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(key) {
            let Some(mesh) = geometry_mesh(&shape.geometry) else {
                continue;
            };
            let edges = mesh_edges(&mesh);
            if edges.len() > MAX_OVERLAY_LINES - cached_edges {
                limited = true;
                break;
            }
            cached_edges += edges.len();
            entry.insert(edges);
        }
        let mut rotation = Quat::from_array(shape.rotation).normalize();
        if matches!(shape.geometry, Geometry::Capsule { .. }) {
            rotation *= Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
        }
        let color = if hit {
            Color::srgb(0.89, 0.75, 0.3)
        } else if overlap {
            Color::srgb(0.75, 0.48, 0.86)
        } else {
            Color::srgb(0.4, 0.6, 0.8)
        };
        for edge in &cache[&key] {
            if lines.len() == MAX_OVERLAY_LINES {
                limited = true;
                break;
            }
            let positions = edge.map(|point| position + rotation * point);
            if positions.iter().all(|p| valid_position(p.to_array())) {
                lines.push(QueryLine {
                    positions,
                    overlap,
                    color,
                });
            }
        }
    }
    for sweep in &queries.sweeps {
        if lines.len() == MAX_OVERLAY_LINES {
            limited = true;
            break;
        }
        let start = Vec3::from(sweep.geometry.position);
        let end = start + Vec3::from(sweep.direction) * sweep.distance;
        if [start, end].iter().all(|p| valid_position(p.to_array())) {
            lines.push(QueryLine {
                positions: [start, end],
                overlap: false,
                color: Color::srgb(0.4, 0.6, 0.8),
            });
        }
    }
    (lines, limited)
}

pub(crate) fn debug_primitives(
    state: &pvd_core::SceneState,
) -> impl Iterator<Item = &pvd_core::debug_draw::Primitive> {
    state
        .debug_draw
        .values()
        .flat_map(|batch| batch.iter())
        .take(MAX_DEBUG_PRIMITIVES)
}

pub fn argb(color: u32) -> Color {
    Color::srgba_u8(
        (color >> 16) as u8,
        (color >> 8) as u8,
        color as u8,
        (color >> 24) as u8,
    )
}

fn is_kinematic(state: &pvd_core::SceneState, id: u64) -> bool {
    state
        .objects
        .get(&id)
        .filter(|object| object.type_name.rsplit("::").next() == Some("PxRigidDynamic"))
        .and_then(|object| object.properties.get("RigidBodyFlags"))
        .is_some_and(|value| match value {
            pvd_core::Value::U64(value) => value & 1 != 0,
            pvd_core::Value::I64(value) => value & 1 != 0,
            _ => false,
        })
}

fn display_scale(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.01, 100.0)
    } else {
        1.0
    }
}

fn mesh_edges(mesh: &Mesh) -> Vec<[Vec3; 2]> {
    let Some(VertexAttributeValues::Float32x3(vertices)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION)
    else {
        return Vec::new();
    };
    let indices: Vec<usize> = match mesh.indices() {
        Some(indices) => indices.iter().collect(),
        None => (0..vertices.len()).collect(),
    };
    let mut keys = std::collections::BTreeSet::new();
    let mut edges = Vec::new();
    for triangle in indices.chunks_exact(3) {
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            let points = [vertices[triangle[a]], vertices[triangle[b]]];
            let mut key = points.map(|p| p.map(f32::to_bits));
            key.sort();
            if keys.insert(key) {
                edges.push(points.map(Vec3::from));
            }
        }
    }
    edges
}

fn intersect_geometry(origin: Vec3, direction: Vec3, geometry: &Geometry) -> Option<f32> {
    intersect_geometry_sided(origin, direction, geometry, true)
}

fn intersect_geometry_sided(
    origin: Vec3,
    direction: Vec3,
    geometry: &Geometry,
    cull_back: bool,
) -> Option<f32> {
    match geometry {
        Geometry::Box { half_extents } => ray_box(origin, direction, Vec3::from(*half_extents)),
        Geometry::Sphere { radius } => ray_sphere(origin, direction, *radius),
        Geometry::Plane => {
            let t = -origin.x / direction.x;
            (t.is_finite() && t > 0.0).then_some(t)
        }
        Geometry::Capsule {
            radius,
            half_height,
        } => {
            let a = direction.y * direction.y + direction.z * direction.z;
            let b = origin.y * direction.y + origin.z * direction.z;
            let c = origin.y * origin.y + origin.z * origin.z - radius * radius;
            let discriminant = b * b - a * c;
            let cylinder = if a > 1e-8 && discriminant >= 0.0 {
                [
                    (-b - discriminant.sqrt()) / a,
                    (-b + discriminant.sqrt()) / a,
                ]
                .into_iter()
                .filter(|t| *t > 0.0 && (origin.x + direction.x * *t).abs() <= *half_height)
                .min_by(f32::total_cmp)
            } else {
                None
            };
            [
                cylinder,
                ray_sphere(origin - Vec3::X * *half_height, direction, *radius),
                ray_sphere(origin + Vec3::X * *half_height, direction, *radius),
            ]
            .into_iter()
            .flatten()
            .min_by(f32::total_cmp)
        }
        Geometry::TriangleMesh { vertices, indices } => indices
            .chunks_exact(3)
            .filter_map(|triangle| {
                let a = Vec3::from(*vertices.get(triangle[0] as usize)?);
                let b = Vec3::from(*vertices.get(triangle[1] as usize)?);
                let c = Vec3::from(*vertices.get(triangle[2] as usize)?);
                let edge1 = b - a;
                let edge2 = c - a;
                let p = direction.cross(edge2);
                let determinant = edge1.dot(p);
                if if cull_back {
                    determinant <= 1e-8
                } else {
                    determinant.abs() < 1e-8
                } {
                    return None;
                }
                let t = origin - a;
                let u = t.dot(p) / determinant;
                let q = t.cross(edge1);
                let v = direction.dot(q) / determinant;
                let distance = edge2.dot(q) / determinant;
                (u >= 0.0 && v >= 0.0 && u + v <= 1.0 && distance > 0.0).then_some(distance)
            })
            .min_by(f32::total_cmp),
    }
}

fn ray_sphere(origin: Vec3, direction: Vec3, radius: f32) -> Option<f32> {
    let b = origin.dot(direction);
    let discriminant = b * b - origin.length_squared() + radius * radius;
    if discriminant < 0.0 {
        return None;
    }
    [-b - discriminant.sqrt(), -b + discriminant.sqrt()]
        .into_iter()
        .filter(|t| *t > 0.0)
        .min_by(f32::total_cmp)
}

fn ray_box(origin: Vec3, direction: Vec3, half: Vec3) -> Option<f32> {
    let mut near = 0.0_f32;
    let mut far = f32::INFINITY;
    for axis in 0..3 {
        if direction[axis].abs() < 1e-8 {
            if origin[axis].abs() > half[axis] {
                return None;
            }
        } else {
            let a = (-half[axis] - origin[axis]) / direction[axis];
            let b = (half[axis] - origin[axis]) / direction[axis];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
    }
    (far >= near && far > 0.0).then_some(if near > 0.0 { near } else { far })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires the official PhysX 3.4 large-scene capture; CPU-only render synchronization benchmark"]
    fn real_large_scene_warm_sync_profile() {
        use std::time::Instant;
        let path = std::env::var_os("PVD_LARGE_CAPTURE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../captures/physx-3.4-cct-large.pvdstream")
            });
        let recording = pvd_core::Recording::from_path(&path).unwrap();
        let last = recording.frame_count() - 1;
        let states = [
            recording.state_at(last - 1).unwrap(),
            recording.state_at(last).unwrap(),
        ];
        let mut app = App::new();
        app.insert_resource(Studio::default())
            .insert_resource(Viewport::default())
            .insert_resource(Assets::<Mesh>::default())
            .insert_resource(Assets::<StandardMaterial>::default())
            .add_systems(Update, sync_scene);
        app.world_mut().resource_mut::<Studio>().state = states[0].clone();
        let start = Instant::now();
        app.update();
        let initial_ms = start.elapsed().as_secs_f64() * 1000.0;
        let identities: HashMap<_, _> = app
            .world()
            .resource::<Viewport>()
            .rendered
            .iter()
            .map(|(key, value)| (*key, value.entity))
            .collect();
        let mut cache = pvd_core::MeshCache::default();
        states[0].render_scene_cached(&mut cache);
        let mut sync_times = Vec::new();
        let mut render_times = Vec::new();
        let mut query_times = Vec::new();
        let mut point_times = Vec::new();
        let mut tree_times = Vec::new();
        let mut tree = crate::tree::TreeCache::default();
        for index in 0..12 {
            let state = &states[index % 2];
            {
                let mut studio = app.world_mut().resource_mut::<Studio>();
                studio.state = state.clone();
                studio.revision += 1;
            }
            let start = Instant::now();
            app.update();
            sync_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            std::hint::black_box(state.render_scene_cached(&mut cache));
            render_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            std::hint::black_box(pvd_core::queries::collect(state));
            query_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            std::hint::black_box(crate::point_clouds::PointClouds::collect(state));
            point_times.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            tree.update(state, index as u64, "", crate::i18n::Language::English);
            tree_times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let viewport = app.world().resource::<Viewport>();
        assert_eq!(identities.len(), viewport.rendered.len());
        assert!(
            viewport
                .rendered
                .iter()
                .all(|(key, object)| identities.get(key) == Some(&object.entity))
        );
        let average = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
        eprintln!(
            "CPU-only scene synchronization: source={} objects={} shapes={} meshes={} materials={} initial_ms={initial_ms:.3} warm_avg_ms={:.3} warm_max_ms={:.3} render_scene_avg_ms={:.3} queries_avg_ms={:.3} points_avg_ms={:.3}",
            path.display(),
            states[0].objects.len(),
            viewport.rendered.len(),
            app.world().resource::<Assets<Mesh>>().len(),
            app.world().resource::<Assets<StandardMaterial>>().len(),
            average(&sync_times),
            sync_times.iter().copied().fold(0.0, f64::max),
            average(&render_times),
            average(&query_times),
            average(&point_times)
        );
        eprintln!(
            "Object tree full rebuild: rows={} average_ms={:.3} max_ms={:.3}",
            tree.rows.len(),
            average(&tree_times),
            tree_times.iter().copied().fold(0.0, f64::max)
        );
    }

    #[test]
    fn live_render_entities_reuse_meshes_and_only_update_changed_components() {
        use bevy::ecs::world::CommandQueue;
        let mut world = World::new();
        let mut meshes = Assets::<Mesh>::default();
        let mesh = meshes.add(Cuboid::default());
        let changed_mesh = meshes.add(Sphere::default());
        let mut materials = Assets::<StandardMaterial>::default();
        let material = materials.add(StandardMaterial::default());
        let selected_material = materials.add(StandardMaterial::default());
        let mut rendered = HashMap::new();
        let mut queue = CommandQueue::default();
        for actor in [1, 2] {
            sync_rendered_object(
                &mut Commands::new(&mut queue, &world),
                &mut rendered,
                (actor, 10),
                mesh.clone(),
                material.clone(),
                Transform::default(),
                true,
            );
        }
        queue.apply(&mut world);
        let first = rendered[&(1, 10)].entity;
        let second = rendered[&(2, 10)].entity;
        assert_ne!(
            first, second,
            "shared shapes must retain distinct actor transforms"
        );
        world.clear_trackers();
        for frame in 0..120 {
            sync_rendered_object(
                &mut Commands::new(&mut queue, &world),
                &mut rendered,
                (1, 10),
                mesh.clone(),
                material.clone(),
                Transform::default(),
                true,
            );
            sync_rendered_object(
                &mut Commands::new(&mut queue, &world),
                &mut rendered,
                (2, 10),
                mesh.clone(),
                material.clone(),
                Transform::from_xyz(frame as f32 + 1.0, 0.0, 0.0),
                true,
            );
            queue.apply(&mut world);
            assert_eq!(rendered[&(1, 10)].entity, first);
            assert_eq!(rendered[&(2, 10)].entity, second);
            let changed = world
                .query_filtered::<Entity, Changed<Transform>>()
                .iter(&world)
                .collect::<Vec<_>>();
            assert_eq!(changed, vec![second]);
            world.clear_trackers();
        }
        sync_rendered_object(
            &mut Commands::new(&mut queue, &world),
            &mut rendered,
            (1, 10),
            changed_mesh.clone(),
            selected_material.clone(),
            Transform::default(),
            false,
        );
        queue.apply(&mut world);
        assert_eq!(world.get::<Mesh3d>(first).unwrap().0, changed_mesh);
        assert_eq!(
            world
                .get::<MeshMaterial3d<StandardMaterial>>(first)
                .unwrap()
                .0,
            selected_material
        );
        assert_eq!(*world.get::<Visibility>(first).unwrap(), Visibility::Hidden);
        assert_eq!(
            world
                .query_filtered::<Entity, With<CapturedObject>>()
                .iter(&world)
                .count(),
            2
        );
    }

    #[test]
    fn debug_surfaces_reuse_assets_and_release_them_when_hidden() {
        let mut app = App::new();
        app.insert_resource(Studio::default())
            .insert_resource(Viewport::default())
            .insert_resource(Assets::<Mesh>::default())
            .insert_resource(Assets::<StandardMaterial>::default())
            .add_systems(Update, sync_scene);
        {
            let mut studio = app.world_mut().resource_mut::<Studio>();
            studio.prefs.show_debug_draw = true;
            studio.prefs.wireframe = false;
            studio.state.debug_draw.insert(
                1,
                std::sync::Arc::new(vec![pvd_core::debug_draw::Primitive::Triangle {
                    instance: 1,
                    positions: [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
                    colors: [0xffffffff; 3],
                }]),
            );
        }
        app.update();
        let entity = app
            .world()
            .resource::<Viewport>()
            .debug_surface
            .as_ref()
            .unwrap()
            .entity;
        for _ in 0..32 {
            app.world_mut().resource_mut::<Studio>().revision += 1;
            app.update();
            assert_eq!(
                app.world()
                    .resource::<Viewport>()
                    .debug_surface
                    .as_ref()
                    .unwrap()
                    .entity,
                entity
            );
            assert_eq!(app.world().resource::<Assets<Mesh>>().len(), 1);
            assert_eq!(app.world().resource::<Assets<StandardMaterial>>().len(), 1);
        }
        app.world_mut()
            .resource_mut::<Studio>()
            .prefs
            .show_debug_draw = false;
        app.update();
        assert!(app.world().get_entity(entity).is_err());
        assert!(app.world().resource::<Assets<Mesh>>().is_empty());
        assert!(
            app.world()
                .resource::<Assets<StandardMaterial>>()
                .is_empty()
        );
    }

    #[test]
    fn coordinate_changes_keep_surface_picking_projection_and_mesh_facing_consistent() {
        use crate::coordinates::{Handedness, UpAxis};
        let object = RenderObject {
            id: 17,
            shape_id: 18,
            dynamic: true,
            sleeping: false,
            position: [3.0, -1.0, 4.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            geometry: Geometry::Sphere { radius: 1.0 },
        };
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 600.0));
        for up in UpAxis::ALL {
            for hand in [Handedness::Right, Handedness::Left] {
                for scale in [1e-6, 0.001, 1.0, 1000.0, 1e6] {
                    let coordinates = Coordinates::new(up, hand, scale);
                    let viewport = Viewport {
                        coordinates,
                        objects: vec![object.clone()],
                        target: coordinates.point(Vec3::from(object.position)),
                        distance: 8.0 * scale,
                        near: 0.01 * scale,
                        far: 100.0 * scale,
                        ..default()
                    };
                    let (center, _) = viewport.project(Vec3::from(object.position), rect).unwrap();
                    assert!(center.distance(rect.center()) < 0.001);
                    assert_eq!(viewport.select_at(center, rect), Some(17));
                    assert_eq!(viewport.select_at(rect.left_top(), rect), None);
                    let mut focused = viewport;
                    focused.focus(Some(17));
                    let (_, pixels_per_meter) =
                        focused.project(Vec3::from(object.position), rect).unwrap();
                    let radius_pixels = pixels_per_meter * scale;
                    assert!(
                        (50.0..=250.0).contains(&radius_pixels),
                        "focus at {scale}: {radius_pixels}"
                    );
                    let mut mesh = geometry_mesh(&Geometry::Plane).unwrap();
                    orient_winding(&mut mesh, coordinates.mirrored);
                    let VertexAttributeValues::Float32x3(vertices) =
                        mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap()
                    else {
                        panic!("positions");
                    };
                    let indices: Vec<_> = mesh.indices().unwrap().iter().take(3).collect();
                    let points: Vec<_> = indices
                        .into_iter()
                        .map(|index| coordinates.point(Vec3::from(vertices[index])))
                        .collect();
                    let normal = (points[1] - points[0])
                        .cross(points[2] - points[0])
                        .normalize();
                    let expected = coordinates.point(Vec3::X).normalize();
                    assert!(normal.abs_diff_eq(expected, 1e-5));
                }
            }
        }
    }

    #[test]
    fn picking_and_labels_respect_the_camera_clip_range() {
        let mut viewport = Viewport {
            yaw: 0.0,
            pitch: 0.0,
            distance: 10.0,
            near: 1.0,
            far: 6.0,
            ..default()
        };
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        viewport.objects.push(RenderObject {
            id: 11,
            shape_id: 12,
            position: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            geometry: Geometry::Sphere { radius: 1.0 },
            dynamic: false,
            sleeping: false,
        });
        assert!(viewport.select_at(rect.center(), rect).is_none());
        assert!(viewport.project(Vec3::ZERO, rect).is_none());
        viewport.far = 20.0;
        assert_eq!(viewport.select_at(rect.center(), rect), Some(11));
        assert!(viewport.project(Vec3::ZERO, rect).is_some());
        viewport.near = 12.0;
        assert!(viewport.select_at(rect.center(), rect).is_none());
        assert!(viewport.project(Vec3::ZERO, rect).is_none());
    }

    #[test]
    fn repeated_query_shapes_obey_global_edge_budget_and_capsule_axis() {
        use pvd_core::queries::{ObjectReference, Overlap, QueryGeometry, QueryScene};
        let geometry = QueryGeometry {
            reference: ObjectReference {
                id: 1,
                generation: Some(0),
            },
            geometry: Geometry::Capsule {
                radius: 0.5,
                half_height: 2.0,
            },
            position: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
        };
        let mut queries = QueryScene {
            overlaps: vec![Overlap {
                scene_id: 2,
                index: 0,
                batched: false,
                kind: 5,
                filter_data: [0; 4],
                filter_flags: 0,
                geometry,
                hits: Vec::new(),
            }],
            ..default()
        };
        let (lines, limited) = query_lines(&queries);
        assert!(!limited);
        let max = lines
            .iter()
            .flat_map(|line| line.positions)
            .fold(Vec3::ZERO, |max, point| max.max(point.abs()));
        assert!((max.x - 2.5).abs() < 0.0001 && max.y <= 0.501 && max.z <= 0.501);
        queries.overlaps = vec![queries.overlaps[0].clone(); 1000];
        let (lines, limited) = query_lines(&queries);
        assert!(limited);
        assert_eq!(lines.len(), MAX_OVERLAY_LINES);
    }

    #[test]
    fn filled_debug_mesh_preserves_vertex_colors_and_rejects_degenerate_triangles() {
        use pvd_core::debug_draw::Primitive;
        let mut state = pvd_core::SceneState::default();
        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        state.debug_draw.insert(
            1,
            std::sync::Arc::new(vec![
                Primitive::Triangle {
                    instance: 1,
                    positions,
                    colors: [0xffff0000, 0xff00ff00, 0xff0000ff],
                },
                Primitive::Triangle {
                    instance: 1,
                    positions: [[0.0; 3]; 3],
                    colors: [0; 3],
                },
                Primitive::Triangle {
                    instance: 1,
                    positions: [[f32::NAN; 3]; 3],
                    colors: [0; 3],
                },
            ]),
        );
        let mesh = debug_triangles(&state).unwrap();
        assert_eq!(mesh.count_vertices(), 3);
        let Some(VertexAttributeValues::Float32x4(colors)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("missing colors")
        };
        assert_eq!(
            colors,
            &[
                [1.0, 0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 1.0]
            ]
        );
    }

    #[test]
    fn debug_drawing_limits_work_across_renderers_and_reject_invalid_numeric_data() {
        use pvd_core::debug_draw::{Pose, Primitive};
        let point = Primitive::Point {
            instance: 1,
            position: [0.0; 3],
            color: 0xffffffff,
        };
        let mut state = pvd_core::SceneState::default();
        state
            .debug_draw
            .insert(1, std::sync::Arc::new(vec![point.clone(); 60_000]));
        state
            .debug_draw
            .insert(2, std::sync::Arc::new(vec![point; 60_000]));
        assert_eq!(debug_primitives(&state).count(), MAX_DEBUG_PRIMITIVES);
        assert!(!drawable_debug(&Primitive::Point {
            instance: 1,
            position: [f32::NAN, 0.0, 0.0],
            color: 0
        }));
        let pose = Pose {
            position: [0.0; 3],
            rotation: [0.0; 4],
        };
        assert!(!drawable_debug(&Primitive::JointFrames {
            instance: 1,
            parent: pose,
            child: pose
        }));
        assert!(!drawable_debug(&Primitive::Text {
            instance: 1,
            position: [0.0; 3],
            size: f32::INFINITY,
            color: 0,
            text: "captured".into()
        }));
    }

    #[test]
    fn malformed_numeric_capture_data_never_builds_a_mesh() {
        for geometry in [
            Geometry::Sphere { radius: f32::NAN },
            Geometry::Box {
                half_extents: [1.0, -1.0, 1.0],
            },
            Geometry::Capsule {
                radius: 1.0,
                half_height: f32::INFINITY,
            },
            Geometry::TriangleMesh {
                vertices: vec![[f32::NAN, 0.0, 0.0]].into(),
                indices: vec![0, 0, 0].into(),
            },
        ] {
            assert!(geometry_mesh(&geometry).is_none());
        }
        let object = RenderObject {
            id: 1,
            shape_id: 2,
            position: [0.0; 3],
            rotation: [0.0; 4],
            geometry: Geometry::Sphere { radius: 1.0 },
            dynamic: true,
            sleeping: false,
        };
        assert!(!valid_object(&object));
    }

    #[test]
    fn scene_pick_uses_recorded_transform_and_nearest_surface() {
        let mut viewport = Viewport {
            yaw: 0.0,
            pitch: 0.0,
            distance: 10.0,
            ..default()
        };
        viewport.objects = vec![
            RenderObject {
                id: 11,
                shape_id: 12,
                position: [0.0, 0.0, 0.0],
                rotation: [0.0, 0.0, 0.0, 1.0],
                geometry: Geometry::Box {
                    half_extents: [1.0; 3],
                },
                dynamic: true,
                sleeping: false,
            },
            RenderObject {
                id: 21,
                shape_id: 22,
                position: [0.0, 0.0, 4.0],
                rotation: Quat::from_rotation_y(0.7).to_array(),
                geometry: Geometry::Box {
                    half_extents: [0.5; 3],
                },
                dynamic: false,
                sleeping: false,
            },
        ];
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        assert_eq!(viewport.select_at(rect.center(), rect), Some(21));
        assert_eq!(viewport.select_at(rect.left_top(), rect), None);
    }

    #[test]
    fn physx_capsule_and_plane_use_the_local_x_axis() {
        let capsule = Geometry::Capsule {
            radius: 0.5,
            half_height: 2.0,
        };
        let hit = intersect_geometry(Vec3::new(1.5, 0.0, 3.0), -Vec3::Z, &capsule).unwrap();
        assert!((hit - 2.5).abs() < 1e-5);
        assert_eq!(
            intersect_geometry(Vec3::new(0.0, 1.5, 3.0), -Vec3::Z, &capsule),
            None
        );
        assert_eq!(
            intersect_geometry(Vec3::X * 3.0, -Vec3::X, &Geometry::Plane),
            Some(3.0)
        );
        assert_eq!(intersect_geometry(Vec3::X, Vec3::Z, &Geometry::Plane), None);
    }

    #[test]
    fn mesh_pick_tests_triangles_instead_of_only_bounds() {
        let mesh = Geometry::TriangleMesh {
            vertices: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]].into(),
            indices: vec![0, 1, 2].into(),
        };
        assert_eq!(
            intersect_geometry(Vec3::new(0.1, 0.1, 2.0), -Vec3::Z, &mesh),
            Some(2.0)
        );
        assert_eq!(
            intersect_geometry(Vec3::new(0.9, 0.9, 2.0), -Vec3::Z, &mesh),
            None
        );
        assert_eq!(
            intersect_geometry(Vec3::new(0.1, 0.1, -2.0), Vec3::Z, &mesh),
            None
        );
        assert_eq!(
            intersect_geometry_sided(Vec3::new(0.1, 0.1, -2.0), Vec3::Z, &mesh, false),
            Some(2.0)
        );
    }

    #[test]
    fn point_clouds_follow_display_coordinates_visibility_and_depth() {
        use crate::{
            coordinates::{Handedness, UpAxis},
            point_clouds::{Cloud, Kind, PointClouds},
        };
        for scale in [1e-6, 1.0, 1e6] {
            let mut viewport = Viewport {
                coordinates: Coordinates::new(UpAxis::Z, Handedness::Left, scale),
                near: 0.0001 * scale,
                far: 1000.0 * scale,
                point_clouds: PointClouds {
                    clouds: vec![Cloud {
                        id: 81,
                        kind: Kind::Fluid,
                        inactive: false,
                        points: vec![Vec3::new(2.0, 3.0, 4.0)],
                    }],
                    ..default()
                },
                ..default()
            };
            viewport.focus(Some(81));
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
            assert_eq!(viewport.select_at(rect.center(), rect), Some(81));
            viewport.show_particles = false;
            assert_eq!(viewport.select_at(rect.center(), rect), None);
        }
    }
}

#[cfg(test)]
mod tcp_render_diagnostic {
    use super::*;
    use std::{
        collections::HashMap,
        process::{Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    #[derive(Resource)]
    struct DiagnosticEgui(egui::Context);
    fn receive_in_ui_pass(mut studio: ResMut<Studio>, context: Res<DiagnosticEgui>) {
        let _ = context
            .0
            .run(egui::RawInput::default(), |ctx| studio.poll(ctx));
    }
    #[test]
    #[ignore = "requires the real PhysX 3.4 SDK producer"]
    fn real_paced_tcp_to_persistent_bevy_transforms() {
        let root = std::env::var_os("PVD_PROBE_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
        let output = std::env::var_os("PVD_PROBE_OUTPUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join("output/live-probe"));
        std::fs::create_dir_all(&output).unwrap();
        let sample = std::env::var_os("PVD_SDK34_SAMPLE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.join(".deps/physx-sample-build/Release/pvd-physx-sample.exe"));
        let context = egui::Context::default();
        let wake = Arc::new(AtomicBool::new(true));
        let requests = Arc::new(AtomicUsize::new(0));
        let w = wake.clone();
        let n = requests.clone();
        context.set_request_repaint_callback(move |r| {
            if r.delay.is_zero() {
                w.store(true, Ordering::Release);
                n.fetch_add(1, Ordering::Relaxed);
            }
        });
        let session = crate::network::NetworkSession::start_with_options(
            pvd_transport::ListenConfig {
                address: "127.0.0.1:0".parse().unwrap(),
                ..Default::default()
            },
            crate::network::NetworkOptions {
                history: pvd_core::live::HistoryConfig {
                    directory: output.join("scene-history"),
                    ..Default::default()
                },
                ..Default::default()
            },
            context.clone(),
        )
        .unwrap();
        let port = session.address().port();
        let mut studio = Studio::default();
        studio.server = Some(session);
        studio.initialized = true;
        let mut app = App::new();
        app.add_plugins(bevy::transform::TransformPlugin)
            .insert_resource(studio)
            .insert_resource(Viewport::default())
            .insert_resource(Assets::<Mesh>::default())
            .insert_resource(Assets::<StandardMaterial>::default())
            .insert_resource(DiagnosticEgui(context))
            .add_systems(Update, sync_scene)
            .add_systems(PostUpdate, receive_in_ui_pass);
        let mut command = Command::new(sample);
        command
            .args([
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--frames",
                "180",
                "--flags",
                "all",
                "--rich",
                "--interval-ms",
                "16",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let started = Instant::now();
        let mut producer = command.spawn().unwrap();
        let mut producer_exit = None;
        let mut producer_elapsed = Duration::ZERO;
        let mut app_updates = 0;
        let mut scene_updates = 0;
        let mut revision = u64::MAX;
        let mut previous = HashMap::new();
        let mut movement = 0;
        let mut last_update = Instant::now() - Duration::from_secs(10);
        let mut trace = String::from(
            "elapsed_ms,studio_frame,studio_revision,viewport_revision,rendered,movement\n",
        );
        while started.elapsed() < Duration::from_secs(20) {
            if wake.swap(false, Ordering::AcqRel) || last_update.elapsed() > Duration::from_secs(5)
            {
                app.update();
                app_updates += 1;
                last_update = Instant::now();
                let viewport = app.world().resource::<Viewport>();
                if revision != viewport.revision {
                    scene_updates += 1;
                    revision = viewport.revision;
                    for (key, object) in &viewport.rendered {
                        let global = app.world().get::<GlobalTransform>(object.entity).unwrap();
                        assert!(
                            global
                                .translation()
                                .abs_diff_eq(object.transform.translation, 0.0001),
                            "stale GlobalTransform {key:?}"
                        );
                        if previous
                            .get(key)
                            .is_some_and(|p| p != &global.translation())
                        {
                            movement += 1;
                        }
                        previous.insert(*key, global.translation());
                    }
                    let studio = app.world().resource::<Studio>();
                    trace.push_str(&format!(
                        "{},{},{},{},{},{}\n",
                        started.elapsed().as_millis(),
                        studio.frame,
                        studio.revision,
                        viewport.revision,
                        viewport.objects.len(),
                        movement
                    ));
                }
            }
            if producer_exit.is_none()
                && let Some(exit) = producer.try_wait().unwrap()
            {
                producer_exit = Some(exit);
                producer_elapsed = started.elapsed();
            }
            if producer_exit.is_some()
                && !app.world().resource::<Studio>().live_connected
                && app.world().resource::<Studio>().count() >= 180
            {
                app.update();
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if producer_exit.is_none() {
            producer.kill().unwrap();
            producer.wait().unwrap();
        }
        let _ = producer.wait();
        std::fs::write(output.join("tcp-scene-trace.csv"), trace).unwrap();
        let studio = app.world().resource::<Studio>();
        let viewport = app.world().resource::<Viewport>();
        println!(
            "TCP_SCENE producer={producer_exit:?} producer_elapsed_s={:.3} app_updates={app_updates} scene_updates={scene_updates} movement={movement} repaint_requests={} count={} studio_frame={} studio_revision={} viewport_revision={} viewport_shapes={}",
            producer_elapsed.as_secs_f64(),
            requests.load(Ordering::Relaxed),
            studio.count(),
            studio.frame,
            studio.revision,
            viewport.revision,
            viewport.objects.len()
        );
        assert!(producer_exit.is_some_and(|s| s.success()));
        assert!(scene_updates > 20);
        assert!(movement > 20);
        assert_eq!(studio.count(), 180);
        assert_eq!(studio.revision, viewport.revision);
    }
}

#[cfg(test)]
mod retained_render_tests {
    use super::render_check_clip_range;

    #[test]
    fn clip_render_ranges_stay_inside_the_public_frame_interval() {
        assert_eq!(render_check_clip_range(20, 61), Some((20, 60)));
        assert_eq!(render_check_clip_range(60, 61), Some((60, 60)));
        assert_eq!(render_check_clip_range(0, 120), Some((30, 119)));
        assert_eq!(render_check_clip_range(0, 0), None);
        assert_eq!(render_check_clip_range(61, 61), None);
    }
}
