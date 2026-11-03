// Optical glass for the transparent desktop-lyrics popup.
//
// This is a *material* shader. Nova's renderer extension has no access to
// pixels behind an unrelated desktop HWND; true backdrop refraction requires
// a native CompositionBackdropBrush/CompositionEffectBrush path. Do not pretend
// to refract the desktop using synthetic noise.
//
// One static fullscreen-quad draw, no textures, no temporal noise, no frame
// polling, premultiplied alpha. Geometry is checked against the saved widget
// dimensions so accidental Win32 Snap/maximize frames are discarded instead
// of emitting a huge tinted rectangle.
struct EffectUniforms {
    params: mat4x4<f32>,
    bounds: vec4<f32>,
    viewport_time: vec4<f32>,
};
@group(0) @binding(0) var<uniform> effect: EffectUniforms;

struct OpticalGlassVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) size_px: vec2<f32>,
};

@vertex
fn vs_lyrics_liquid_glass(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> OpticalGlassVarying {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let uv = corners[vertex_index];
    let viewport = max(effect.viewport_time.xy, vec2<f32>(1.0, 1.0));
    let pixel_position = effect.bounds.xy + uv * effect.bounds.zw;

    var out: OpticalGlassVarying;
    out.position = vec4<f32>(
        pixel_position / viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0),
        0.0,
        1.0,
    );
    out.uv = uv;
    out.size_px = effect.bounds.zw;
    return out;
}

fn rounded_rectangle_distance(p: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let r = clamp(radius, 0.0, max(min(half_size.x, half_size.y) - 1.0, 0.0));
    let d = abs(p) - half_size + vec2<f32>(r);
    return length(max(d, vec2<f32>(0.0))) + min(max(d.x, d.y), 0.0) - r;
}

fn ease01(value: f32) -> f32 {
    let x = clamp(value, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

@fragment
fn fs_lyrics_liquid_glass(input: OpticalGlassVarying) -> @location(0) vec4<f32> {
    // Packed params: (opacity, hover intensity, logical radius, logical width),
    // (logical height, DPI scale, unused, unused).
    let opacity = clamp(effect.params[0].x, 0.0, 0.85);
    let interaction = clamp(effect.params[0].y, 0.0, 1.0);
    let expected_size = vec2<f32>(effect.params[0].w, effect.params[1].x);
    let device_scale = max(effect.params[1].y, 1.0);
    let expected_physical = expected_size * device_scale;
    let tolerance = max(expected_size * 0.08, vec2<f32>(8.0));
    let logical_delta = abs(input.size_px - expected_size);
    let physical_delta = abs(input.size_px - expected_physical);
    let logical_match = all(logical_delta <= tolerance);
    let physical_match = all(physical_delta <= tolerance * device_scale);

    // Nova backends can report renderer bounds in logical or device pixels.
    // Accept either, but reject native Snap/maximize geometry from stale DComp.
    if (expected_size.x > 0.0 && !(logical_match || physical_match)) {
        discard;
    }
    let physical_units = physical_match && !logical_match;
    let radius = effect.params[0].z * select(1.0, device_scale, physical_units);
    if (opacity <= 0.0001 || any(input.size_px <= vec2<f32>(1.0))) {
        discard;
    }

    let half_size = input.size_px * 0.5;
    let center = (input.uv - vec2<f32>(0.5)) * input.size_px;
    let distance_px = rounded_rectangle_distance(center, half_size, radius);
    let pixel_aa = max(fwidth(distance_px), 0.7);
    let coverage = 1.0 - smoothstep(-pixel_aa, pixel_aa, distance_px);
    if (coverage <= 0.0001) {
        discard;
    }

    // A thin bevel creates Fresnel-like reflections without pretending that
    // we sampled/refocused content behind this window.
    let inside_px = max(-distance_px, 0.0);
    let bevel_px = clamp(radius * 0.52, 4.0, 11.0);
    let edge_view = 1.0 - ease01(inside_px / bevel_px);
    let fresnel = 0.04 + 0.96 * edge_view * edge_view * edge_view * edge_view * edge_view;
    let sharp_rim = 1.0 - ease01(inside_px / 1.8);
    let inner_rim = ease01(inside_px / 1.8) * (1.0 - ease01(inside_px / 7.5));

    // Soft, deterministic environment highlights: no sine waves, noise
    // texture, Gaussian blur, pow(), or desktop readback in this fragment.
    let top_sheen = (1.0 - ease01(input.uv.y * 3.0))
        * (0.35 + 0.65 * ease01(input.uv.x * 1.35));
    let diagonal = 1.0 - ease01(abs(input.uv.x + input.uv.y * 0.74 - 0.64) * 5.0);
    let bottom_shadow = ease01((input.uv.y - 0.55) * 2.2);
    let corner_light = (1.0 - ease01(length((input.uv - vec2<f32>(0.13, 0.10))
        * vec2<f32>(1.15, 1.70)) * 2.5));

    let white = vec3<f32>(0.96, 0.975, 1.0);
    let cool = vec3<f32>(0.71, 0.84, 1.0);
    let warm = vec3<f32>(1.0, 0.92, 0.80);

    // Low-density, optically neutral interior; all energetic reflections are
    // confined near the edge. Hover changes only the reflection, never the
    // panel opacity, avoiding large-area hover flashes.
    var color = vec3<f32>(0.29, 0.32, 0.37);
    color += white * top_sheen * 0.18;
    color += cool * (sharp_rim * 0.48 + fresnel * 0.25);
    color += warm * inner_rim * (0.13 + interaction * 0.08);
    color += white * diagonal * edge_view * (0.06 + interaction * 0.06);
    color += cool * corner_light * (0.045 + interaction * 0.035);
    color -= vec3<f32>(0.06) * bottom_shadow;
    color = clamp(color, vec3<f32>(0.0), vec3<f32>(1.0));

    // Premultiplied alpha is mandatory for GPUI's transparent HWND swapchain.
    // The center remains clear; slightly denser edges preserve visual shape.
    let body_alpha = opacity * (0.66 + 0.10 * top_sheen);
    let edge_alpha = min(opacity * 0.46, 0.18) * edge_view;
    let alpha = clamp((body_alpha + edge_alpha) * coverage, 0.0, 0.90);
    return vec4<f32>(color * alpha, alpha);
}
