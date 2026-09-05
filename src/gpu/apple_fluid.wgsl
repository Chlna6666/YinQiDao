struct ShaderEffectDrawParameters {
    bounds_origin: vec2<f32>,
    bounds_size: vec2<f32>,
    content_mask_origin: vec2<f32>,
    content_mask_size: vec2<f32>,
    params: mat4x4<f32>,
};

struct GlobalParams {
    viewport_size: vec2<f32>,
    premultiplied_alpha: u32,
    pad: u32,
};

struct ShaderEffectVertex {
    position_x: f32,
    position_y: f32,
    position_z: f32,
    color_rgba8: u32,
};

struct ShaderEffectVarying {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) params0: vec4<f32>,
    @location(2) params1: vec4<f32>,
    @location(3) params2: vec4<f32>,
    @location(4) params3: vec4<f32>,
    @location(5) bounds_size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: GlobalParams;
@group(0) @binding(20) var<storage, read> effect_draw_parameters: array<ShaderEffectDrawParameters>;
@group(0) @binding(21) var<storage, read> effect_vertices: array<ShaderEffectVertex>;

const TAU: f32 = 6.28318530718;

fn rotate2(value: vec2<f32>, angle: f32) -> vec2<f32> {
    let s = sin(angle);
    let c = cos(angle);
    return vec2<f32>(c * value.x - s * value.y, s * value.x + c * value.y);
}

fn gaussian_field(point: vec2<f32>, center: vec2<f32>, falloff: f32) -> f32 {
    let delta = point - center;
    return exp(-dot(delta, delta) * falloff);
}

fn domain_warp(point: vec2<f32>, time: f32, seed: f32, amount: f32) -> vec2<f32> {
    let wave_x =
        sin(point.y * 3.15 + time * 0.23 + seed * 1.7) +
        sin((point.x + point.y) * 2.05 - time * 0.17 + seed * 2.3);
    let wave_y =
        cos(point.x * 2.75 - time * 0.21 + seed * 2.9) +
        cos((point.x - point.y) * 1.85 + time * 0.13 + seed * 3.7);
    return vec2<f32>(wave_x, wave_y) * (0.055 * amount);
}

@vertex
fn vs_shader_effect(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> ShaderEffectVarying {
    let vertex = effect_vertices[vertex_index];
    let draw = effect_draw_parameters[instance_index];
    let local = vec2<f32>(vertex.position_x, vertex.position_y) * 0.5 + vec2<f32>(0.5, 0.5);
    let pixel_position = draw.bounds_origin + local * draw.bounds_size;
    let viewport = max(globals.viewport_size, vec2<f32>(1.0, 1.0));
    let device_position = pixel_position / viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: ShaderEffectVarying;
    out.position = vec4<f32>(device_position, 0.999, 1.0);
    out.uv = local;
    out.params0 = draw.params[0];
    out.params1 = draw.params[1];
    out.params2 = draw.params[2];
    out.params3 = draw.params[3];
    out.bounds_size = draw.bounds_size;
    return out;
}

@fragment
fn fs_apple_fluid_opaque(input: ShaderEffectVarying) -> @location(0) vec4<f32> {
    let dominant = input.params0.xyz;
    let secondary = input.params1.xyz;
    let tertiary = input.params2.xyz;
    let dark = input.params3.xyz;
    let time = input.params0.w;
    let motion = input.params1.w;
    let seed = input.params2.w * TAU;
    let dim = input.params3.w;

    let aspect = input.bounds_size.x / max(input.bounds_size.y, 1.0);
    var point = (input.uv - vec2<f32>(0.5, 0.5)) * vec2<f32>(aspect, 1.0);

    // Refined Now Playing uses a ~150 s counter-rotating container around four ~60 s rotating
    // source quadrants. Keep that motion model, but execute it as one fragment program.
    let container_angle = -time * TAU / 150.0 * motion;
    let block_angle = time * TAU / 60.0 * motion;
    point = rotate2(point, container_angle);
    point += domain_warp(point, time, seed, motion);

    let spread = vec2<f32>(0.31 * max(aspect, 0.8), 0.31);
    let center0 = rotate2(vec2<f32>(-spread.x, -spread.y), block_angle + seed);
    let center1 = rotate2(vec2<f32>( spread.x, -spread.y), block_angle + 1.5707963 + seed * 0.7);
    let center2 = rotate2(vec2<f32>(-spread.x,  spread.y), block_angle + 3.1415926 + seed * 1.3);
    let center3 = rotate2(vec2<f32>( spread.x,  spread.y), block_angle + 4.7123890 + seed * 1.9);

    let weight0 = gaussian_field(point, center0, 3.25);
    let weight1 = gaussian_field(point, center1, 3.10);
    let weight2 = gaussian_field(point, center2, 3.35);
    let weight3 = gaussian_field(point, center3, 3.00);
    let fourth = mix(dominant, secondary, 0.52) * 0.92 + tertiary * 0.08;
    let sum = max(weight0 + weight1 + weight2 + weight3, 0.0001);

    var color = (
        dominant * weight0 +
        secondary * weight1 +
        tertiary * weight2 +
        fourth * weight3
    ) / sum;

    // Continuous low-frequency fields already have the visual footprint of a strongly blurred
    // source. Saturation/brightness roughly match the reference's saturate(1.5) brightness(0.8).
    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = mix(vec3<f32>(luminance), color, 1.28) * 0.88;

    let centered = (input.uv - vec2<f32>(0.5, 0.5)) * vec2<f32>(0.88, 1.08);
    let vignette = smoothstep(0.26, 0.78, length(centered));
    let dark_mix = clamp(dim * 0.58 + vignette * 0.20, 0.12, 0.58);
    color = mix(color, dark, dark_mix);

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
