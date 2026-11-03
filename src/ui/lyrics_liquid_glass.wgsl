// YinQiDao desktop-lyrics Liquid Glass material.
//
// This shader intentionally lives in the music application rather than GPUI. A transparent
// top-level window cannot sample arbitrary pixels owned by the desktop compositor through GPUI's
// custom-mesh binding, so this material provides the optical rim/specular/caustic response locally
// while preserving the transparent swapchain. The base color is neutral gray and premultiplied.

struct EffectUniforms {
    params: mat4x4<f32>,
    bounds: vec4<f32>,
    viewport_time: vec4<f32>,
};
@group(0) @binding(0) var<uniform> effect: EffectUniforms;

struct LyricsGlassVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) @interpolate(flat) material: vec3<f32>,
};

@vertex
fn vs_lyrics_liquid_glass(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> LyricsGlassVarying {
    let uv = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    )[vertex_index];
    let pixel_position = effect.bounds.xy + uv * effect.bounds.zw;
    let viewport = max(effect.viewport_time.xy, vec2<f32>(1.0));
    let device_position = pixel_position / viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: LyricsGlassVarying;
    out.position = vec4<f32>(device_position, 0.5, 1.0);
    out.uv = uv;
    out.size = effect.bounds.zw;
    // The application packs material opacity, interaction intensity and radius into column 0.
    out.material = effect.params[0].xyz;
    return out;
}

fn rounded_rect_sdf(point: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let r = min(radius, max(min(half_size.x, half_size.y) - 0.5, 0.0));
    let q = abs(point) - half_size + vec2<f32>(r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0))) - r;
}

fn soft_lens(uv: vec2<f32>, center: vec2<f32>, radius: f32) -> f32 {
    let distance_to_center = distance(uv, center) / max(radius, 0.0001);
    let x = clamp(1.0 - distance_to_center, 0.0, 1.0);
    return x * x * (3.0 - 2.0 * x);
}

@fragment
fn fs_lyrics_liquid_glass(input: LyricsGlassVarying) -> @location(0) vec4<f32> {
    let opacity = clamp(input.material.x, 0.0, 0.85);
    let interaction = clamp(input.material.y, 0.0, 1.0);
    let radius = max(input.material.z, 0.0);
    if (opacity <= 0.0001) {
        discard;
    }

    let half_size = max(input.size * 0.5, vec2<f32>(1.0));
    let point = input.uv * input.size - half_size;
    let distance_to_edge = rounded_rect_sdf(point, half_size, radius);
    let antialias = max(fwidth(distance_to_edge), 0.75);
    let mask = 1.0 - smoothstep(-antialias, antialias, distance_to_edge);
    if (mask <= 0.0001) {
        discard;
    }

    // Liquid Glass approximation: neutral translucent substrate, a strong near-edge Fresnel-like
    // highlight, broad upper sheen, and two soft caustic/lens lobes that become clearer during
    // interaction. This remains stable and cheap: no texture reads and no per-fragment exp().
    let edge_distance = max(-distance_to_edge, 0.0);
    let rim = 1.0 - smoothstep(0.0, 3.5, edge_distance);
    let upper_sheen = pow(clamp(1.0 - input.uv.y, 0.0, 1.0), 3.0)
        * (0.35 + 0.65 * smoothstep(0.0, 0.45, input.uv.x));
    let leading_lens = soft_lens(input.uv, vec2<f32>(0.18, 0.10), 0.42);
    let trailing_lens = soft_lens(input.uv, vec2<f32>(0.84, 0.78), 0.58);
    let diagonal = pow(
        clamp(1.0 - abs((input.uv.x + input.uv.y) - 0.92), 0.0, 1.0),
        5.0,
    );

    let neutral = vec3<f32>(0.20, 0.205, 0.22);
    let cool_specular = vec3<f32>(0.88, 0.94, 1.0);
    let warm_specular = vec3<f32>(1.0, 0.94, 0.86);
    var color = neutral;
    color += cool_specular * rim * (0.08 + interaction * 0.12);
    color += cool_specular * upper_sheen * (0.035 + interaction * 0.045);
    color += cool_specular * leading_lens * interaction * 0.045;
    color += warm_specular * trailing_lens * interaction * 0.020;
    color += vec3<f32>(1.0) * diagonal * interaction * 0.018;

    // Slightly denser while the user operates the overlay, matching the interaction behavior of
    // modern liquid-glass controls without turning the desktop lyrics into an opaque panel.
    let alpha = clamp(opacity + interaction * 0.025, 0.0, 0.88) * mask;
    return vec4<f32>(color * alpha, alpha);
}
