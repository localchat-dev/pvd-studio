use super::{MAX_RECORDS, RaycastRecord, array, hit_fields, query_range, raycast_record, vector};
use crate::{Object, RenderRegion, SceneState, Value, frame_sources::FrameObject};

pub struct DisplayRay {
    pub start: [f32; 3],
    pub end: [f32; 3],
    pub has_hit: bool,
    pub marker: Option<[f32; 3]>,
}

#[derive(Default)]
pub struct DisplayRays {
    pub rays: Vec<DisplayRay>,
    pub truncated: bool,
}

const RAY_PROPERTIES: [(&str, &str); 2] = [
    ("SceneQueries.Raycasts", "SceneQueries.Hits"),
    ("BatchedQueries.Raycasts", "BatchedQueries.Hits"),
];
// Keep malformed coordinates from overflowing the renderer's bounds and vector math.
const MAX_DISPLAY_COORDINATE: f32 = 1e12;

/// Collects regular and batched rays from the requested scene lifetime, or from all scenes.
/// `global_distance_limit` limits drawn length in the full view. A regional view instead clips
/// the entire recorded ray to the display sphere. Hit status always uses the recorded query.
pub fn collect_display_rays(
    state: &SceneState,
    scene: Option<FrameObject>,
    region: Option<RenderRegion>,
    limit: usize,
    global_distance_limit: f32,
) -> DisplayRays {
    let current = scene.and_then(|identity| {
        state
            .objects
            .get(&identity.id)
            .filter(|object| object.generation == identity.generation)
    });
    let all = scene.is_none().then(|| state.objects.values());
    collect_display_rays_in_scenes(
        current.into_iter().chain(all.into_iter().flatten()),
        region,
        limit,
        global_distance_limit,
    )
}

/// Like `collect_display_rays`, using already selected scene objects from the restored state.
pub fn collect_display_rays_in_scenes<'a>(
    scenes: impl Iterator<Item = &'a Object>,
    region: Option<RenderRegion>,
    limit: usize,
    global_distance_limit: f32,
) -> DisplayRays {
    let mut result = DisplayRays::default();
    let mut ray_budget = MAX_RECORDS;
    let mut hit_budget = MAX_RECORDS;
    for scene in scenes {
        if scene.type_name != "physx3::PxScene" {
            continue;
        }
        for (rays_key, hits_key) in RAY_PROPERTIES {
            let Some(records) = scene.properties.get(rays_key).and_then(|v| array(v).ok()) else {
                continue;
            };
            for record in records {
                if ray_budget == 0 {
                    result.truncated = true;
                    return result;
                }
                ray_budget -= 1;
                let Ok(ray) = raycast_record(record) else {
                    continue;
                };
                let Some([start, end]) = segment(&ray, region, global_distance_limit) else {
                    continue;
                };
                let Ok(hits) = query_range(scene, ray.hits, hits_key, "Hits") else {
                    continue;
                };
                if hits.len() > hit_budget {
                    result.truncated = true;
                    return result;
                }
                hit_budget -= hits.len();
                let marker_visible = |distance, position| {
                    display_position(position)
                        && match region {
                            Some(region) => region.contains(position),
                            None => distance <= global_distance_limit,
                        }
                };
                let Some((has_hit, marker)) = hit_summary(hits, ray.distance, marker_visible)
                else {
                    continue;
                };
                if result.rays.len() == limit {
                    result.truncated = true;
                    return result;
                }
                result.rays.push(DisplayRay {
                    start,
                    end,
                    has_hit,
                    marker,
                });
            }
        }
    }
    result
}

fn segment(
    ray: &RaycastRecord<'_>,
    region: Option<RenderRegion>,
    global_distance_limit: f32,
) -> Option<[[f32; 3]; 2]> {
    let origin = ray.origin.map(f64::from);
    let direction = ray.direction.map(f64::from);
    let distances = match region {
        Some(region) => region.ray_interval(origin, direction, f64::from(ray.distance))?,
        None => [0.0, f64::from(ray.distance.min(global_distance_limit))],
    };
    let points = distances.map(|distance| {
        std::array::from_fn(|axis| (origin[axis] + direction[axis] * distance) as f32)
    });
    points.into_iter().all(display_position).then_some(points)
}

pub(super) fn display_position(point: [f32; 3]) -> bool {
    point.into_iter().all(|v| v.abs() <= MAX_DISPLAY_COORDINATE)
}

fn hit_summary(
    hits: &[Value],
    maximum: f32,
    marker_visible: impl Fn(f32, [f32; 3]) -> bool,
) -> Option<(bool, Option<[f32; 3]>)> {
    let mut has_hit = false;
    let mut nearest = None;
    for hit in hits {
        let Ok((fields, flags, distance)) = hit_fields(hit, Some(maximum), false) else {
            continue;
        };
        if !matches!((&fields[0], &fields[1]), (Value::Ref(_), Value::Ref(_))) {
            continue;
        }
        has_hit = true;
        if flags & 1 != 0
            && nearest.is_none_or(|(previous, _)| distance < previous)
            && let Ok(position) = vector(&fields[4])
            && marker_visible(distance, position)
        {
            nearest = Some((distance, position));
        }
    }
    (has_hit || hits.is_empty()).then(|| (has_hit, nearest.map(|(_, position)| position)))
}
