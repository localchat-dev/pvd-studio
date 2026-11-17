//! Static navigation overlays are independent of captured physics and replay frames.

use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use bevy::prelude::*;
use pvd_core::{frame_sources::FrameObject, navmesh::NavMesh};

use crate::{
    file_picker::{FilePicker, PickerKind},
    i18n::Message,
    studio::Studio,
};

mod panel;
pub use panel::window;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Alignment {
    /// Translation in captured physics coordinates, before the viewport transform.
    pub offset: [f32; 3],
    pub rotation_degrees: [f32; 3],
    pub scale: f32,
    pub flip_x: bool,
}

impl Default for Alignment {
    fn default() -> Self {
        Self {
            offset: [0.0; 3],
            rotation_degrees: [0.0; 3],
            scale: 1.0,
            flip_x: true,
        }
    }
}

impl Alignment {
    pub fn sanitized(self) -> Self {
        let finite = |value: f32, limit: f32, fallback: f32| {
            if value.is_finite() {
                value.clamp(-limit, limit)
            } else {
                fallback
            }
        };
        Self {
            offset: self.offset.map(|value| finite(value, 1e9, 0.0)),
            rotation_degrees: self.rotation_degrees.map(|value| finite(value, 360.0, 0.0)),
            scale: if self.scale.is_finite() {
                self.scale.clamp(1e-6, 1e6)
            } else {
                1.0
            },
            flip_x: self.flip_x,
        }
    }

    pub fn transform(self) -> Mat4 {
        let alignment = self.sanitized();
        let [x, y, z] = alignment.rotation_degrees.map(f32::to_radians);
        // Apply X, then Y, then Z rotations in the physics source coordinate system.
        let rotation =
            Quat::from_rotation_z(z) * Quat::from_rotation_y(y) * Quat::from_rotation_x(x);
        Mat4::from_scale_rotation_translation(
            Vec3::new(if alignment.flip_x { -1.0 } else { 1.0 }, 1.0, 1.0) * alignment.scale,
            rotation,
            Vec3::from_array(alignment.offset),
        )
    }

    pub fn point(self, point: Vec3) -> Vec3 {
        self.transform().transform_point3(point)
    }

    pub fn mirrored(self) -> bool {
        self.flip_x
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlaySettings {
    pub visible: bool,
    pub show_surface: bool,
    pub show_edges: bool,
    pub opacity: f32,
    pub alignment: Alignment,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            visible: true,
            show_surface: true,
            show_edges: true,
            opacity: 0.4,
            alignment: Alignment::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OverlayBinding {
    #[default]
    Global,
    Room {
        epoch: u64,
        room: FrameObject,
    },
}

impl OverlayBinding {
    pub fn current(studio: &Studio) -> Self {
        studio
            .displayed_room()
            .map_or(Self::Global, |room| Self::Room {
                epoch: studio.room_session_epoch(),
                room,
            })
    }

    pub fn matches(self, epoch: u64, room: Option<FrameObject>) -> bool {
        match self {
            Self::Global => true,
            Self::Room {
                epoch: bound_epoch,
                room: bound_room,
            } => bound_epoch == epoch && Some(bound_room) == room,
        }
    }

    fn belongs_to_epoch(self, epoch: u64) -> bool {
        matches!(self, Self::Global)
            || matches!(self, Self::Room { epoch: bound, .. } if bound == epoch)
    }

    pub fn visible_in(self, studio: &Studio) -> bool {
        match self {
            Self::Global => true,
            Self::Room { epoch, room } => {
                epoch == studio.room_session_epoch()
                    && match studio.displayed_room() {
                        Some(displayed) => self.matches(epoch, Some(displayed)),
                        None => studio.state.objects.get(&room.id).is_some_and(|object| {
                            object.generation == room.generation
                                && object.type_name.ends_with("::PxScene")
                        }),
                    }
            }
        }
    }
}

#[derive(Debug)]
pub struct LoadedNavMesh {
    pub mesh: Arc<NavMesh>,
    pub path: PathBuf,
    pub binding: OverlayBinding,
}

#[derive(Debug)]
pub struct OverlayEntry {
    pub loaded: LoadedNavMesh,
    pub settings: OverlaySettings,
    pub revision: u64,
}

impl OverlayEntry {
    pub fn visible_for(&self, studio: &Studio) -> bool {
        self.settings.visible && self.loaded.binding.visible_in(studio)
    }
}

pub const MAX_OVERLAYS: usize = 64;
pub const MAX_OVERLAY_GEOMETRY_BYTES: usize = 512 * 1024 * 1024;

fn geometry_bytes(mesh: &NavMesh) -> usize {
    // Source arrays, render assets and a GPU copy; worker input is bounded separately.
    mesh.vertices.capacity() * size_of::<[f32; 3]>()
        + mesh.indices.capacity() * size_of::<u32>()
        + mesh.edges.capacity() * size_of::<[[f32; 3]; 2]>()
        + 2 * (mesh.vertices.len() * 24 + mesh.indices.len() * 4 + mesh.edges.len() * 48)
}

struct PendingImport {
    binding: OverlayBinding,
    receiver: Mutex<mpsc::Receiver<Result<LoadedNavMesh, Message>>>,
    cancelled: Arc<AtomicBool>,
}

impl Drop for PendingImport {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Resource, Default)]
pub struct NavMeshOverlay {
    pub entries: Vec<OverlayEntry>,
    pub selected_binding: OverlayBinding,
    pub focus_requested: Option<OverlayBinding>,
    pub open: bool,
    path_input: String,
    message: Option<Message>,
    pending: Option<PendingImport>,
    picker: FilePicker,
    picker_binding: Option<OverlayBinding>,
    startup_path: Option<PathBuf>,
    displayed_binding: Option<OverlayBinding>,
    session_epoch: Option<u64>,
    next_revision: u64,
}

impl NavMeshOverlay {
    pub fn visible_for(&self, studio: &Studio) -> bool {
        self.selected()
            .is_some_and(|entry| entry.visible_for(studio))
    }

    pub fn entry(&self, binding: OverlayBinding) -> Option<&OverlayEntry> {
        self.entries
            .iter()
            .find(|entry| entry.loaded.binding == binding)
    }

    pub fn selected(&self) -> Option<&OverlayEntry> {
        self.entry(self.selected_binding)
    }

    pub fn selected_mut(&mut self) -> Option<&mut OverlayEntry> {
        self.entries
            .iter_mut()
            .find(|entry| entry.loaded.binding == self.selected_binding)
    }

    pub fn select_binding(&mut self, binding: OverlayBinding) {
        if self.selected_binding != binding {
            self.selected_binding = binding;
            self.path_input = self.selected().map_or_else(String::new, |entry| {
                entry.loaded.path.to_string_lossy().into_owned()
            });
        }
    }

    fn sync_selection(&mut self, studio: &Studio) {
        let epoch = studio.room_session_epoch();
        if self.session_epoch.is_some_and(|previous| previous != epoch) {
            self.displayed_binding = None;
            self.message = None;
            self.entries
                .retain(|entry| entry.loaded.binding.belongs_to_epoch(epoch));
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| !pending.binding.belongs_to_epoch(epoch))
            {
                self.pending = None;
            }
            if self
                .picker_binding
                .is_some_and(|binding| !binding.belongs_to_epoch(epoch))
            {
                self.picker_binding = None;
            }
            self.focus_requested = None;
        }
        self.session_epoch = Some(epoch);
        let current = OverlayBinding::current(studio);
        if self.displayed_binding != Some(current) {
            self.select_binding(current);
            self.displayed_binding = Some(current);
        }
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn has_pending_work(&self) -> bool {
        self.is_loading() || self.picker.is_busy() || self.startup_path.is_some()
    }

    pub fn is_busy(&self) -> bool {
        self.has_pending_work()
    }

    /// Defer command-line imports until the initial capture has selected its room.
    pub fn queue_import(&mut self, path: PathBuf) {
        self.startup_path = Some(path);
        self.open = true;
    }

    fn take_ready_startup_path(&mut self, studio: &Studio) -> Option<PathBuf> {
        if studio.initialized
            && studio.job.is_none()
            && studio.seeking.is_none()
            && !studio.room_selection_pending()
        {
            self.startup_path.take()
        } else {
            None
        }
    }

    pub fn request_import(
        &mut self,
        path: PathBuf,
        studio: &Studio,
        context: Option<egui::Context>,
    ) {
        self.sync_selection(studio);
        self.select_binding(OverlayBinding::current(studio));
        self.request_selected_import(path, studio, context);
    }

    pub fn request_selected_import(
        &mut self,
        path: PathBuf,
        studio: &Studio,
        context: Option<egui::Context>,
    ) {
        self.start_import(path, self.selected_binding, studio, context);
    }

    fn start_import(
        &mut self,
        path: PathBuf,
        binding: OverlayBinding,
        studio: &Studio,
        context: Option<egui::Context>,
    ) {
        if self.pending.is_some() {
            self.message = Some(Message::new(
                "A NavMesh import is still running. Wait for it to finish or cancel it first.",
                "已有 NavMesh 正在导入，请等待完成或先取消。",
            ));
            return;
        }
        // A direct import supersedes any outstanding file-picker result.
        self.picker_binding = None;
        self.startup_path = None;
        self.message = None;
        if self.selected_binding == binding {
            self.path_input = path.to_string_lossy().into_owned();
        }
        if !binding.belongs_to_epoch(studio.room_session_epoch()) {
            self.message = Some(Message::new(
                "Select a scene in the current capture.",
                "请选择当前捕获中的场景。",
            ));
            return;
        }
        if self.entry(binding).is_none() && self.entries.len() >= MAX_OVERLAYS {
            self.message = Some(Message::new(
                "NavMesh layer limit reached (64). Unload an unused layer first.",
                "NavMesh 图层已达到 64 个上限，请先卸载不用的图层。",
            ));
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawn =
            studio
                .background_jobs
                .spawn("pvd-navmesh-loader", cancelled.clone(), move || {
                    let outcome =
                        read_navmesh(&path, &worker_cancelled).map(|mesh| LoadedNavMesh {
                            mesh: Arc::new(mesh),
                            path,
                            binding,
                        });
                    if !worker_cancelled.load(Ordering::Acquire) {
                        let _ = sender.send(outcome);
                    }
                    if let Some(context) = context {
                        context.request_repaint();
                    }
                });
        match spawn {
            Ok(_) => {
                self.pending = Some(PendingImport {
                    binding,
                    receiver: Mutex::new(receiver),
                    cancelled,
                });
            }
            Err(error) => {
                self.message = Some(Message::detail(
                    "Cannot start NavMesh import",
                    "无法启动 NavMesh 导入",
                    error,
                ));
            }
        }
    }

    fn request_picker(&mut self, studio: &Studio, context: egui::Context) {
        if self.picker.is_busy() || self.is_loading() {
            return;
        }
        match self.picker.request(
            PickerKind::OpenNavMesh,
            &self.path_input,
            studio.prefs.language,
            context,
        ) {
            Ok(()) => self.picker_binding = Some(self.selected_binding),
            Err(error) => self.message = Some(error),
        }
    }

    pub fn cancel_import(&mut self) {
        self.pending = None;
        self.startup_path = None;
        self.picker_binding = None;
        self.message = Some(Message::new(
            "NavMesh import cancelled",
            "已取消 NavMesh 导入",
        ));
    }

    pub fn unload(&mut self) {
        let binding = self.selected_binding;
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.binding == binding)
        {
            self.pending = None;
        }
        if self.picker_binding == Some(binding) {
            self.picker_binding = None;
        }
        self.entries.retain(|entry| entry.loaded.binding != binding);
        if self.focus_requested == Some(binding) {
            self.focus_requested = None;
        }
        self.message = None;
    }

    fn check_capacity(
        &self,
        binding: OverlayBinding,
        incoming_bytes: usize,
    ) -> Result<(), Message> {
        let previous = self.entry(binding);
        if previous.is_none() && self.entries.len() >= MAX_OVERLAYS {
            return Err(Message::new(
                "NavMesh layer limit reached (64).",
                "NavMesh 图层已达到 64 个上限。",
            ));
        }
        let retained: usize = self
            .entries
            .iter()
            .filter(|entry| entry.loaded.binding != binding)
            .map(|entry| geometry_bytes(&entry.loaded.mesh))
            .sum();
        if retained.saturating_add(incoming_bytes) > MAX_OVERLAY_GEOMETRY_BYTES {
            return Err(Message::new(
                "NavMesh geometry budget exceeded (512 MiB). Unload an unused layer first.",
                "NavMesh 几何预算超过 512 MiB，请先卸载不用的图层。",
            ));
        }
        Ok(())
    }

    pub fn publish(&mut self, loaded: LoadedNavMesh) -> Result<(), Message> {
        let binding = loaded.binding;
        self.check_capacity(binding, geometry_bytes(&loaded.mesh))?;
        self.next_revision = self.next_revision.wrapping_add(1);
        let revision = self.next_revision;
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.loaded.binding == binding)
        {
            entry.loaded = loaded;
            entry.revision = revision;
            entry.settings.visible = true;
        } else {
            self.entries.push(OverlayEntry {
                loaded,
                settings: OverlaySettings::default(),
                revision,
            });
        }
        Ok(())
    }

    fn poll_import(&mut self) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let result = match pending
            .receiver
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .try_recv()
        {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err(Message::new(
                "NavMesh import stopped without a result",
                "NavMesh 导入已停止，未返回结果",
            )),
        };
        self.pending = None;
        match result {
            Ok(loaded) => {
                self.message = self.publish(loaded).err();
            }
            Err(error) => self.message = Some(error),
        }
    }
}

/// Run before overlay rendering, independently of whether its settings window is open.
pub fn poll(mut overlay: ResMut<NavMeshOverlay>, studio: Res<Studio>) {
    overlay.sync_selection(&studio);
    if let Some(path) = overlay.take_ready_startup_path(&studio) {
        overlay.request_import(path, &studio, None);
    }
    if let Some(result) = overlay.picker.poll() {
        let binding = overlay.picker_binding.take();
        if let Some(binding) = binding {
            match result.outcome {
                Ok(Some(path)) => overlay.start_import(path, binding, &studio, None),
                Ok(None) => {}
                Err(error) => overlay.message = Some(error),
            }
        }
    }
    overlay.poll_import();
}

fn read_navmesh(path: &Path, cancelled: &AtomicBool) -> Result<NavMesh, Message> {
    let result = (|| -> Result<NavMesh, Box<dyn std::error::Error + Send + Sync>> {
        let mut file = File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "select a regular file").into(),
            );
        }
        let bytes = read_bounded(&mut file, metadata.len(), cancelled)?;
        Ok(pvd_core::navmesh::parse(&bytes, || {
            cancelled.load(Ordering::Acquire)
        })?)
    })();
    result.map_err(|error| Message::detail("NavMesh import failed", "NavMesh 导入失败", error))
}

fn read_bounded(
    reader: &mut impl Read,
    expected_len: u64,
    cancelled: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let limit = pvd_core::navmesh::MAX_FILE_BYTES;
    if expected_len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("NavMesh exceeds {} MiB", limit / (1024 * 1024)),
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_len as usize)
        .map_err(io::Error::other)?;
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "NavMesh import cancelled",
            ));
        }
        let count = reader.read(&mut chunk)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len() as u64 + count as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "NavMesh grew beyond the import limit",
            ));
        }
        bytes.try_reserve(count).map_err(io::Error::other)?;
        bytes.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(test)]
mod tests;
