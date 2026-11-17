# Gizmo line shader

`lines.wgsl` derives from `bevy_gizmos_render` 0.18.1, `src/lines.wgsl`, under its MIT OR Apache-2.0 license. Both upstream licenses are included. Upstream source: <https://github.com/bevyengine/bevy/blob/v0.18.1/crates/bevy_gizmos_render/src/lines.wgsl>.

The shader computes line-edge coverage in physical pixels. Screen-linear interpolation and fragment derivatives keep the transition consistent along perspective-projected lines. An extra pixel on each side contains the transition; invisible fragments are discarded to protect depth occlusion. Solid, dotted and dashed lines share this treatment.

Negative depth bias applies only within `0 < clip.z <= clip.w`. Vertices outside this finite near/far depth interval preserve their clip-space depth, keeping clipping defined and the logarithmic bias calculation within its valid domain.

The desktop supplies the `SMOOTH_EDGES` constant when registering the embedded source. Disabling line antialiasing removes the edge expansion and coverage calculation while retaining the same clipping and depth-bias protection.

The desktop registers the shader at the pinned renderer's `embedded://bevy_gizmos_render/lines.wgsl` asset path after default plugins are built and before `RenderStartup`. Line vertices, colors, nominal widths, clipping and pattern calculations retain the upstream interface. The shader is embedded in the executable and adds no external runtime asset.

This integration depends on Bevy 0.18.1's shader interface and embedded path. Dependency upgrades require rechecking actual GPU output, occlusion, line patterns and playback timings. Edge coverage expands line fragment work; it does not add a full-screen pass or enlarge scene render textures.
