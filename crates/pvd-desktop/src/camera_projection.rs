use bevy::{
    camera::{CameraProjection, SubCameraView},
    math::Vec3A,
    prelude::*,
};

#[derive(Clone, Debug, Default)]
pub struct FinitePerspective {
    pub inner: PerspectiveProjection,
}

impl FinitePerspective {
    pub fn new(near: f32, far: f32) -> Self {
        let mut projection = Self::default();
        projection.set_clip(near, far);
        projection
    }

    pub fn set_clip(&mut self, near: f32, far: f32) -> bool {
        if !near.is_finite() || !far.is_finite() || near <= 0.0 || far <= near {
            return false;
        }
        self.inner.near = near;
        self.inner.far = far;
        self.inner.near_clip_plane = Vec4::new(0.0, 0.0, -1.0, -near);
        true
    }

    fn aligned(&self) -> PerspectiveProjection {
        let mut projection = self.inner.clone();
        projection.near_clip_plane = Vec4::new(0.0, 0.0, -1.0, -projection.near);
        projection
    }

    fn finite_depth(&self, mut matrix: Mat4) -> Mat4 {
        // Finite depth preserves Bevy's view/subview rays while enforcing both clipping planes.
        let near = self.inner.near as f64;
        let far = self.inner.far as f64;
        matrix.z_axis.z = (near / (far - near)) as f32;
        matrix.w_axis.z = (near * far / (far - near)) as f32;
        matrix
    }
}

impl CameraProjection for FinitePerspective {
    fn get_clip_from_view(&self) -> Mat4 {
        self.finite_depth(self.aligned().get_clip_from_view())
    }

    fn get_clip_from_view_for_sub(&self, sub_view: &SubCameraView) -> Mat4 {
        self.finite_depth(self.aligned().get_clip_from_view_for_sub(sub_view))
    }

    fn update(&mut self, width: f32, height: f32) {
        if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
            self.inner.update(width, height);
        }
    }

    fn far(&self) -> f32 {
        self.inner.far
    }

    fn get_frustum_corners(&self, z_near: f32, z_far: f32) -> [Vec3A; 8] {
        self.inner.get_frustum_corners(z_near, z_far)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn projected(matrix: Mat4, point: Vec3) -> Vec3 {
        let clip = matrix * point.extend(1.0);
        clip.truncate() / clip.w
    }

    #[test]
    fn real_projection_matrix_clips_both_planes_across_display_scales() {
        for scale in [1e-6, 0.001, 1.0, 1000.0, 1e6] {
            let near = 0.01 * scale;
            let far = 100.0 * scale;
            let projection = FinitePerspective::new(near, far);
            let matrix = projection.get_clip_from_view();
            assert!(matrix.is_finite());
            assert!((projected(matrix, Vec3::new(0.0, 0.0, -near)).z - 1.0).abs() < 1e-6);
            assert!(projected(matrix, Vec3::new(0.0, 0.0, -far)).z.abs() < 1e-6);
            assert!(projected(matrix, Vec3::new(0.0, 0.0, -near * 0.9)).z > 1.0);
            assert!(projected(matrix, Vec3::new(0.0, 0.0, -far * 1.1)).z < 0.0);
            let middle = projected(matrix, Vec3::new(0.0, 0.0, -scale));
            assert!((0.0..1.0).contains(&middle.z));
            assert_eq!(projection.inner.near_clip_plane.w, -near);
        }
    }

    #[test]
    fn resize_preserves_frustum_and_subview_pixels_with_finite_depth() {
        let mut projection = FinitePerspective::new(0.03, 90.0);
        projection.update(800.0, 600.0);
        let full = projection.get_clip_from_view();
        let same_view = SubCameraView {
            full_size: UVec2::new(800, 600),
            size: UVec2::new(800, 600),
            offset: Vec2::ZERO,
        };
        assert!(full.abs_diff_eq(projection.get_clip_from_view_for_sub(&same_view), 1e-6));
        let sub = SubCameraView {
            full_size: UVec2::new(800, 600),
            size: UVec2::new(320, 180),
            offset: Vec2::new(320.0, 90.0),
        };
        let sub_matrix = projection.get_clip_from_view_for_sub(&sub);
        for point in [
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.3, 0.2, -2.0),
            Vec3::new(-0.1, -0.2, -3.0),
        ] {
            let a = projected(full, point);
            let b = projected(sub_matrix, point);
            let full_pixel = Vec2::new((a.x + 1.0) * 400.0, (1.0 - a.y) * 300.0);
            let sub_pixel = Vec2::new((b.x + 1.0) * 160.0, (1.0 - b.y) * 90.0);
            assert!(full_pixel.abs_diff_eq(sub_pixel + sub.offset, 1e-4));
            assert!((a.z - b.z).abs() < 1e-6);
        }
        assert!(projected(sub_matrix, Vec3::new(0.0, 0.0, -100.0)).z < 0.0);
        assert!(projected(sub_matrix, Vec3::new(0.0, 0.0, -0.01)).z > 1.0);
        let corners = projection.get_frustum_corners(-projection.inner.near, -projection.inner.far);
        for (i, point) in corners.into_iter().enumerate() {
            let ndc = projected(full, point.into());
            assert!((ndc.x.abs() - 1.0).abs() < 1e-6 && (ndc.y.abs() - 1.0).abs() < 1e-6);
            assert!((ndc.z - if i < 4 { 1.0 } else { 0.0 }).abs() < 1e-6);
        }
        let aspect = projection.inner.aspect_ratio;
        projection.update(0.0, 0.0);
        assert_eq!(projection.inner.aspect_ratio, aspect);
        projection.update(1600.0, 600.0);
        assert!((projection.get_clip_from_view().x_axis.x * 2.0 - full.x_axis.x).abs() < 1e-6);
    }

    #[test]
    fn invalid_clipping_requests_preserve_valid_configuration() {
        let mut projection = FinitePerspective::new(0.05, 500.0);
        let before = projection.get_clip_from_view();
        for (near, far) in [
            (0.0, 1.0),
            (-1.0, 1.0),
            (1.0, 1.0),
            (2.0, 1.0),
            (f32::NAN, 5.0),
            (1.0, f32::INFINITY),
        ] {
            assert!(!projection.set_clip(near, far));
            assert_eq!(projection.get_clip_from_view(), before);
        }
        assert!(projection.set_clip(1e-8, 1e12));
        assert!(projection.get_clip_from_view().is_finite());
        assert_eq!(projection.inner.near_clip_plane.w, -1e-8);
    }
}
