use super::*;
use std::collections::BTreeMap;

const MAX_ROOM_CAMERAS: usize = 256;
type Key = Option<(u64, u32)>;

#[derive(Clone, Copy)]
struct Pose {
    target: Vec3,
    direction: Vec3,
    distance: f32,
}

impl Pose {
    fn capture(viewport: &Viewport) -> Self {
        let direction = Vec3::new(
            viewport.yaw.sin() * viewport.pitch.cos(),
            viewport.pitch.sin(),
            viewport.yaw.cos() * viewport.pitch.cos(),
        );
        Self {
            target: viewport.coordinates.source_point(viewport.target),
            direction: viewport.coordinates.source_direction(direction),
            distance: viewport.distance / viewport.coordinates.scale,
        }
    }

    fn restore(self, viewport: &mut Viewport) {
        let direction = viewport.coordinates.direction(self.direction).normalize();
        viewport.target = viewport.coordinates.point(self.target);
        viewport.yaw = direction.x.atan2(direction.z);
        viewport.pitch = direction.y.clamp(-1.0, 1.0).asin();
        viewport.distance = self.distance * viewport.coordinates.scale;
    }
}

enum Pending {
    Restore(Pose),
    Focus,
}

#[derive(Default)]
pub(super) struct Cameras {
    epoch: Option<u64>,
    active: Option<Key>,
    poses: BTreeMap<Key, (u64, Pose)>,
    sequence: u64,
    pending: Option<Pending>,
}

impl Cameras {
    fn switch(&mut self, epoch: u64, key: Key, current: Pose) -> bool {
        let same_session = self.epoch == Some(epoch);
        if same_session && self.active == Some(key) {
            return false;
        }
        let had_active = self.active.is_some();
        if same_session {
            if let Some(previous) = self.active
                && self.pending.is_none()
            {
                self.sequence = self.sequence.wrapping_add(1);
                self.poses.insert(previous, (self.sequence, current));
                if self.poses.len() > MAX_ROOM_CAMERAS {
                    let oldest = self
                        .poses
                        .iter()
                        .min_by_key(|(_, (age, _))| age)
                        .map(|(key, _)| *key);
                    if let Some(oldest) = oldest {
                        self.poses.remove(&oldest);
                    }
                }
            }
        } else {
            self.poses.clear();
            self.epoch = Some(epoch);
        }
        self.active = Some(key);
        self.pending = self
            .poses
            .get(&key)
            .map(|(_, pose)| Pending::Restore(*pose))
            .or_else(|| (key.is_some() || had_active).then_some(Pending::Focus));
        self.pending.is_some()
    }
}

impl Viewport {
    pub(super) fn prepare_room_camera(&mut self, studio: &mut Studio) -> bool {
        let pose = Pose::capture(self);
        let key = studio
            .room_scope
            .room()
            .map(|room| (room.id, room.generation));
        let changed = self
            .room_cameras
            .switch(studio.room_session_epoch(), key, pose);
        if changed {
            self.captured_pose = None;
            studio.captured_camera = None;
            studio.follow_selected = false;
            studio.focus_selected = false;
        }
        changed
    }

    pub(super) fn finish_room_camera(&mut self) {
        match self.room_cameras.pending.take() {
            Some(Pending::Restore(pose)) => pose.restore(self),
            Some(Pending::Focus) if self.focus_bounds(None).is_some() => self.focus(None),
            Some(Pending::Focus) => self.room_cameras.pending = Some(Pending::Focus),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_camera_restores_source_space_pose_across_coordinates_and_resets_per_session() {
        let mut viewport = Viewport {
            target: Vec3::new(3.0, 4.0, 5.0),
            yaw: 1.2,
            pitch: 0.3,
            distance: 17.0,
            ..default()
        };
        let original = Pose::capture(&viewport);
        let mut cache = Cameras::default();
        assert!(cache.switch(10, Some((100, 0)), original));
        assert!(!cache.switch(10, Some((100, 0)), original));
        cache.pending = None;
        assert!(cache.switch(10, Some((200, 0)), original));
        cache.pending = None;
        viewport.target = Vec3::splat(100.0);
        assert!(cache.switch(10, Some((100, 0)), Pose::capture(&viewport)));
        let Some(Pending::Restore(saved)) = cache.pending.take() else {
            panic!("camera not retained")
        };
        viewport.coordinates = Coordinates::new(
            crate::coordinates::UpAxis::Z,
            crate::coordinates::Handedness::Left,
            0.1,
        );
        saved.restore(&mut viewport);
        let restored = Pose::capture(&viewport);
        assert!(original.target.abs_diff_eq(restored.target, 0.0001));
        assert!(original.direction.abs_diff_eq(restored.direction, 0.0001));
        assert!((original.distance - restored.distance).abs() < 0.0001);
        assert!(cache.switch(11, Some((100, 0)), restored));
        assert!(matches!(cache.pending, Some(Pending::Focus)));
        assert!(cache.poses.is_empty());
    }

    #[test]
    fn camera_budget_evicts_oldest_room_without_aliasing_a_reused_scene() {
        let pose = Pose::capture(&Viewport::default());
        let mut cache = Cameras::default();
        for id in 0..MAX_ROOM_CAMERAS as u64 + 2 {
            cache.switch(1, Some((id, 0)), pose);
            cache.pending = None;
        }
        assert_eq!(cache.poses.len(), MAX_ROOM_CAMERAS);
        assert!(!cache.poses.contains_key(&Some((0, 0))));
        cache.switch(1, Some((1, 1)), pose);
        assert!(matches!(cache.pending, Some(Pending::Focus)));
    }
}
