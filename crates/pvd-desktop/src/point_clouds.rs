use bevy::prelude::*;

use crate::i18n::Message;

const MAX_POINTS: usize = 50_000;
const MAX_CONSTRAINTS: usize = 100_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cloth,
    Particles,
    Fluid,
}

pub struct Cloud {
    pub id: u64,
    pub kind: Kind,
    pub points: Vec<Vec3>,
    pub inactive: bool,
}

#[derive(Default)]
pub struct PointClouds {
    pub clouds: Vec<Cloud>,
    pub constraints: Vec<(u64, [Vec3; 2])>,
    pub diagnostics: Vec<Message>,
    pub point_count: usize,
}

impl PointClouds {
    pub fn collect(state: &pvd_core::SceneState) -> Self {
        Self::from_collections(
            pvd_core::cloth::collect(state),
            pvd_core::particles::collect(state),
        )
    }

    pub fn collect_from_ids(
        state: &pvd_core::SceneState,
        cloths: &std::collections::BTreeSet<u64>,
        particles: &std::collections::BTreeSet<u64>,
    ) -> Self {
        Self::from_collections(
            pvd_core::cloth::collect_from_ids(state, cloths),
            pvd_core::particles::collect_from_ids(state, particles),
        )
    }

    fn from_collections(
        frame: pvd_core::cloth::ClothFrame,
        particles: pvd_core::particles::Collection,
    ) -> Self {
        let mut result = Self::default();
        result.diagnostics.extend(
            frame
                .diagnostics
                .into_iter()
                .map(|detail| Message::detail("Cloth data", "布料数据", detail)),
        );
        let mut points_left = MAX_POINTS;
        let mut constraints_left = MAX_CONSTRAINTS;
        let mut limited = false;
        let mut invalid = 0usize;
        result
            .diagnostics
            .extend(particles.diagnostics.into_iter().map(|detail| {
                Message::detail(
                    "Particle data",
                    "粒子数据",
                    format!("0x{:x}: {}", detail.actor, detail.message),
                )
            }));
        for system in particles.systems {
            let taken = points_left.min(system.particles.len());
            let positions: Vec<_> = system
                .particles
                .iter()
                .take(points_left)
                .map(|particle| Vec3::from(particle.position))
                .filter(|point| valid_point(*point))
                .collect();
            limited |= taken < system.particles.len();
            invalid += taken - positions.len();
            points_left -= taken;
            result.point_count += positions.len();
            let kind = match system.kind {
                pvd_core::particles::Kind::System => Kind::Particles,
                pvd_core::particles::Kind::Fluid => Kind::Fluid,
            };
            result.clouds.push(Cloud {
                id: system.actor,
                kind,
                points: positions,
                inactive: !system.enabled(),
            });
        }
        for cloth in frame.cloths {
            let taken = points_left.min(cloth.particles.len());
            let positions: Vec<_> = cloth
                .world_positions()
                .take(points_left)
                .map(Vec3::from)
                .filter(|point| valid_point(*point))
                .collect();
            limited |= taken < cloth.particles.len();
            invalid += taken - positions.len();
            points_left -= taken;
            result.point_count += positions.len();
            result.clouds.push(Cloud {
                id: cloth.id,
                kind: Kind::Cloth,
                points: positions,
                inactive: cloth.sleeping,
            });
            let taken = constraints_left.min(cloth.fabric.constraints.len());
            let edges: Vec<_> = cloth
                .world_constraints()
                .take(constraints_left)
                .map(|pair| (cloth.id, pair.map(Vec3::from)))
                .filter(|(_, pair)| pair.iter().all(|point| valid_point(*point)))
                .collect();
            limited |= taken < cloth.fabric.constraints.len();
            invalid += taken - edges.len();
            constraints_left -= taken;
            result.constraints.extend(edges);
        }
        if limited {
            result.diagnostics.push(Message::new(format!("Particle/cloth display is limited to {MAX_POINTS} points and {MAX_CONSTRAINTS} constraint edges; recorded data is retained."), format!("粒子/布料显示最多 {MAX_POINTS} 个点和 {MAX_CONSTRAINTS} 条约束边；录制数据仍保留。")));
        }
        if invalid > 0 {
            result.diagnostics.push(Message::new(format!("{invalid} particle/cloth display elements have non-finite coordinates or exceed the 1e12 source-coordinate display range."), format!("{invalid} 个粒子/布料显示元素包含非有限坐标或超出源坐标 1e12 的显示范围。")));
        }
        result
    }
}

fn valid_point(point: Vec3) -> bool {
    point.is_finite() && point.abs().max_element() <= 1e12
}
