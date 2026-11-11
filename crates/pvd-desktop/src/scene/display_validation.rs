use super::*;

impl Viewport {
    pub(crate) fn display_probe_stats(&self) -> (u64, usize, usize) {
        (
            self.revision,
            self.objects.len(),
            self.incremental.last_actor_count,
        )
    }

    pub(crate) fn validate_display(
        &self,
        studio: &Studio,
        entities: &Query<(
            &Transform,
            &GlobalTransform,
            &Visibility,
            &Mesh3d,
            &MeshMaterial3d<StandardMaterial>,
        )>,
        meshes: &Assets<Mesh>,
    ) -> Result<usize, String> {
        let mut expected: Vec<_> = studio
            .state
            .render_scene()
            .objects
            .into_iter()
            .filter(|object| {
                valid_pose(object)
                    && valid_geometry(&object.geometry)
                    && incremental::object_visible(studio, object)
            })
            .collect();
        expected.sort_by_key(|object| (object.id, object.shape_id));
        let mut actual: Vec<_> = self.objects.iter().collect();
        actual.sort_by_key(|object| (object.id, object.shape_id));
        if !expected.iter().eq(actual) {
            return Err("incremental Viewport differs from complete render_scene".into());
        }
        if self.rendered.len() != expected.len() {
            return Err("render entity count differs from the filtered capture".into());
        }
        for object in &expected {
            let key = (object.id, object.shape_id);
            let rendered = self
                .rendered
                .get(&key)
                .ok_or_else(|| format!("render entity missing: {key:?}"))?;
            let (transform, global, visibility, mesh, material) = entities
                .get(rendered.entity)
                .map_err(|error| error.to_string())?;
            let mut rotation = Quat::from_array(object.rotation).normalize();
            if matches!(object.geometry, Geometry::Capsule { .. }) {
                rotation *= Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
            }
            let expected_transform = self.coordinates.transform(
                Transform::from_translation(Vec3::from(object.position)).with_rotation(rotation),
            );
            let expected_visibility = if studio.prefs.wireframe {
                Visibility::Hidden
            } else {
                Visibility::Inherited
            };
            if *transform != expected_transform
                || !global
                    .to_matrix()
                    .abs_diff_eq(expected_transform.to_matrix(), 0.0001)
                || *visibility != expected_visibility
            {
                return Err(format!("captured transform/visibility differs: {key:?}"));
            }
            if mesh.0 != rendered.mesh || material.0 != rendered.material {
                return Err(format!("render handle differs: {key:?}"));
            }
            let actual_mesh = meshes.get(&mesh.0).ok_or("missing mesh asset")?;
            let mut expected_mesh =
                geometry_mesh(&object.geometry).ok_or("oracle cannot build geometry")?;
            orient_winding(&mut expected_mesh, self.coordinates.mirrored);
            if !actual_mesh.attributes().eq(expected_mesh.attributes())
                || actual_mesh.indices() != expected_mesh.indices()
                || actual_mesh.primitive_topology() != expected_mesh.primitive_topology()
            {
                return Err(format!("render geometry differs: {key:?}"));
            }
        }
        Ok(expected.len())
    }
}
