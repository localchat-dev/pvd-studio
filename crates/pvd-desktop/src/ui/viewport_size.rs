use bevy::math::{DVec2, UVec2};

const MAX_TEXTURE_DIMENSION: u32 = 4096;

pub(super) fn texture_size(logical_size: egui::Vec2, pixels_per_point: f32) -> UVec2 {
    if !logical_size.is_finite()
        || logical_size.x <= 0.0
        || logical_size.y <= 0.0
        || !pixels_per_point.is_finite()
        || pixels_per_point <= 0.0
    {
        return UVec2::ONE;
    }
    let physical_size = DVec2::new(f64::from(logical_size.x), f64::from(logical_size.y))
        * f64::from(pixels_per_point);
    let limit = f64::from(MAX_TEXTURE_DIMENSION);
    let scale = (limit / physical_size.max_element()).min(1.0);
    let size = physical_size * scale;
    UVec2::new(
        size.x.round().clamp(1.0, limit) as u32,
        size.y.round().clamp(1.0, limit) as u32,
    )
}

#[cfg(test)]
mod tests {
    use bevy::{camera::CameraProjection, prelude::*};
    use pvd_core::{Geometry, RenderObject};

    use super::*;
    use crate::{camera_projection::FinitePerspective, scene::Viewport};

    #[test]
    fn high_dpi_wide_and_tall_views_keep_their_aspect() {
        assert_eq!(
            texture_size(egui::vec2(2560.0, 720.0), 2.0),
            UVec2::new(4096, 1152)
        );
        assert_eq!(
            texture_size(egui::vec2(720.0, 2560.0), 2.0),
            UVec2::new(1152, 4096)
        );
        assert_eq!(
            texture_size(egui::vec2(8192.0, 4096.0), 1.0),
            UVec2::new(4096, 2048)
        );
    }

    #[test]
    fn normal_views_use_nearest_physical_pixels() {
        assert_eq!(
            texture_size(egui::vec2(846.0, 537.0), 1.0),
            UVec2::new(846, 537)
        );
        assert_eq!(
            texture_size(egui::vec2(1024.0, 768.0), 1.25),
            UVec2::new(1280, 960)
        );
        assert_eq!(
            texture_size(egui::vec2(843.4, 536.6), 1.25),
            UVec2::new(1054, 671)
        );
        assert_eq!(
            texture_size(egui::vec2(4096.0, 4096.0), 1.0),
            UVec2::splat(4096)
        );
    }

    #[test]
    fn invalid_dimensions_or_dpi_have_a_finite_minimum_target() {
        for invalid in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(texture_size(egui::vec2(invalid, 720.0), 1.0), UVec2::ONE);
            assert_eq!(texture_size(egui::vec2(1280.0, invalid), 1.0), UVec2::ONE);
            assert_eq!(texture_size(egui::vec2(1280.0, 720.0), invalid), UVec2::ONE);
        }
    }

    #[test]
    fn extreme_positive_values_stay_bounded_without_multiplication_overflow() {
        assert_eq!(
            texture_size(egui::Vec2::splat(f32::MAX), f32::MAX),
            UVec2::splat(4096)
        );
        assert_eq!(
            texture_size(egui::vec2(f32::MAX, f32::MAX * 0.5), f32::MAX),
            UVec2::new(4096, 2048)
        );
        assert_eq!(
            texture_size(egui::vec2(f32::MAX, f32::MIN_POSITIVE), 1.0),
            UVec2::new(4096, 1)
        );
        assert_eq!(
            texture_size(egui::Vec2::splat(f32::MIN_POSITIVE), f32::MIN_POSITIVE),
            UVec2::ONE
        );
        assert_eq!(texture_size(egui::vec2(0.1, 0.2), 0.5), UVec2::ONE);
    }

    #[test]
    fn uniform_scale_differs_by_at_most_half_a_pixel_after_quantization() {
        for logical in [
            egui::vec2(846.3, 537.7),
            egui::vec2(2560.5, 720.25),
            egui::vec2(720.25, 2560.5),
            egui::vec2(8191.5, 1234.75),
            egui::vec2(1024.0, 4096.0),
        ] {
            for dpi in [0.5, 1.0, 1.25, 1.5, 2.0, 3.5] {
                let actual = texture_size(logical, dpi);
                let physical = bevy::math::DVec2::new(f64::from(logical.x), f64::from(logical.y))
                    * f64::from(dpi);
                let ideal = physical * (4096.0 / physical.max_element()).min(1.0);
                assert!(actual.min_element() >= 1 && actual.max_element() <= 4096);
                assert!((actual.as_dvec2() - ideal).abs().max_element() <= 0.500_001);
            }
        }
    }

    #[test]
    fn rendered_off_center_box_is_selected_in_capped_high_dpi_view() {
        let mut viewport = Viewport::default();
        viewport.objects.push(RenderObject {
            id: 17,
            shape_id: 18,
            dynamic: true,
            sleeping: false,
            position: [8.0, 2.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            geometry: Geometry::Box {
                half_extents: [0.1; 3],
            },
        });
        let source_position = Vec3::from(viewport.objects[0].position);
        let camera =
            Transform::from_translation(viewport.position()).looking_at(viewport.target, Vec3::Y);
        for (logical, dpi) in [
            (egui::vec2(846.0, 537.0), 1.0),
            (egui::vec2(2560.0, 720.0), 2.0),
            (egui::vec2(2560.5, 720.25), 2.0),
            (egui::vec2(5120.0, 1440.0), 1.25),
        ] {
            let pixels = texture_size(logical, dpi);
            let mut projection = FinitePerspective::new(viewport.near, viewport.far);
            projection.update(pixels.x as f32, pixels.y as f32);
            let clip_from_world = projection.get_clip_from_view() * camera.to_matrix().inverse();
            let ndc = clip_from_world.project_point3(source_position);
            assert!(ndc.x.abs() < 1.0 && ndc.y.abs() < 1.0 && (0.0..=1.0).contains(&ndc.z));
            let rect = egui::Rect::from_min_size(egui::pos2(280.0, 170.0), logical);
            let pixel = rect.min
                + egui::vec2(
                    (ndc.x + 1.0) * logical.x * 0.5,
                    (1.0 - ndc.y) * logical.y * 0.5,
                );
            assert_eq!(
                viewport.select_at(pixel, rect),
                Some(17),
                "rendered point {pixel:?} in {logical:?} at DPI {dpi}"
            );
        }
    }
}
