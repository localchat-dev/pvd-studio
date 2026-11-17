#import bevy_pbr::mesh_view_bindings::view

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> settings: vec4<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> grid_color: vec4<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vertex(@location(0) position: vec3<f32>) -> VertexOutput {
    // Geometry only covers the screen. Its local bounds are never a world-space grid boundary.
    return VertexOutput(vec4(position.xy, 0.5, 1.0), position.xy);
}

// Evaluate line distance in grid-cell coordinates using the world-space pixel footprint.
fn lines(coordinates: vec2<f32>, footprint: vec2<f32>, spacing: f32) -> f32 {
    let cell = coordinates / spacing;
    let pixel = max(footprint / spacing, vec2(0.000001));
    let distance = abs(fract(cell + 0.5) - 0.5);
    let half_width = min(pixel * (settings.y * 0.5), vec2(0.25));
    var coverage: vec2<f32>;
    if settings.w > 0.5 {
        coverage = clamp((half_width - distance) / pixel + 0.5, vec2(0.0), vec2(1.0));
    } else {
        coverage = select(vec2(0.0), vec2(1.0), distance <= half_width);
    }
    // Unresolvable lines fade instead of aliasing into a solid or shimmering surface.
    coverage *= 1.0 - smoothstep(vec2(0.25), vec2(1.0), pixel);
    return max(coverage.x, coverage.y);
}

struct FragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};

@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    // A perspective ray from the actual Bevy view handles orbit, fly, and captured cameras.
    // The homogeneous XYZ is a direction; avoiding division by W also avoids far-plane infinity.
    let view_ray = view.view_from_clip * vec4(in.ndc, 1.0, 1.0);
    let direction = normalize((view.world_from_view * vec4(view_ray.xyz, 0.0)).xyz);
    let origin = view.world_position;
    let denominator = select(select(-0.000001, 0.000001, direction.y >= 0.0), direction.y, abs(direction.y) >= 0.000001);
    let distance = -origin.y / denominator;
    let world = origin + direction * distance;
    let coordinates = world.xz / settings.x;

    // Derivatives run before any nonuniform discard, including sky and near-parallel rays.
    let footprint = max(fwidth(coordinates), vec2(0.000001));
    let level = clamp(log2(max(max(footprint.x, footprint.y) * 8.0, 1.0)) / log2(5.0), 0.0, 24.0);
    let spacing = pow(5.0, floor(level));
    let fine = lines(coordinates, footprint, spacing);
    let major = lines(coordinates, footprint, spacing * 5.0);
    let coarse = lines(coordinates, footprint, spacing * 25.0);
    let alpha = mix(max(fine * 0.2, major * 0.45), max(major * 0.2, coarse * 0.45), fract(level))
        * smoothstep(0.005, 0.05, abs(direction.y));

    let clip = view.clip_from_world * vec4(world.x, 0.0, world.z, 1.0);
    let depth = clip.z / clip.w;
    if abs(direction.y) < 0.000001 || distance <= 0.0 || clip.w <= 0.0
        || depth < 0.0 || depth > 1.0 || alpha <= 0.0 {
        discard;
    }

    // Keep the existing gizmo bias convention. Bias is display-only and applied after clipping.
    var biased_depth = depth;
    if settings.z >= 0.0 {
        biased_depth = depth * (1.0 - settings.z);
    } else if depth > 0.0 {
        biased_depth = pow(depth, 1.0 + settings.z);
    }
    return FragmentOutput(vec4(grid_color.rgb, alpha), clamp(biased_depth, 0.0, 1.0));
}
