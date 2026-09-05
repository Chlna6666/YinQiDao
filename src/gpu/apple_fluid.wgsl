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

fn rotate2(value: vec2<f32>, angle: f32) -> vec2<f32> {
    let s = sin(angle);
    let c = cos(angle);
    return vec2<f32>(c * value.x - s * value.y, s * value.x + c * value.y);
}

fn hash21(point: vec2<f32>, seed: f32) -> f32 {
    let h = dot(point, vec2<f32>(127.1, 311.7)) + seed * 74.37;
    return fract(sin(h) * 43758.5453123);
}

fn value_noise(point: vec2<f32>, seed: f32) -> f32 {
    let cell = floor(point);
    let local = fract(point);
    let smooth = local * local * (vec2<f32>(3.0) - 2.0 * local);
    let a = hash21(cell, seed);
    let b = hash21(cell + vec2<f32>(1.0, 0.0), seed);
    let c = hash21(cell + vec2<f32>(0.0, 1.0), seed);
    let d = hash21(cell + vec2<f32>(1.0, 1.0), seed);
    return mix(mix(a, b, smooth.x), mix(c, d, smooth.x), smooth.y);
}

fn octave_transform(point: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        point.x * 1.58 + point.y * 1.16,
        -point.x * 1.16 + point.y * 1.58,
    );
}

fn fbm(point: vec2<f32>, seed: f32) -> f32 {
    var p = point;
    var value = value_noise(p, seed) * 0.5333333;
    p = octave_transform(p) + vec2<f32>(3.7, 1.9);
    value += value_noise(p, seed + 0.17) * 0.2666667;
    p = octave_transform(p) + vec2<f32>(-2.1, 5.4);
    value += value_noise(p, seed + 0.41) * 0.1333333;
    p = octave_transform(p) + vec2<f32>(4.8, -3.2);
    value += value_noise(p, seed + 0.73) * 0.0666667;
    return value;
}

@vertex
fn vs_shader_effect(
    @builtin(vertex_index) vertex_index: u32,
    @builtin(instance_index) instance_index: u32,
) -> ShaderEffectVarying {
    let vertex = effect_vertices[vertex_index];
    let draw = effect_draw_parameters[instance_index];
    let local = vec2<f32>(vertex.position_x, vertex.position_y) * 0.5 + vec2<f32>(0.5);
    let pixel_position = draw.bounds_origin + local * draw.bounds_size;
    let viewport = max(globals.viewport_size, vec2<f32>(1.0));
    let device_position = pixel_position / viewport * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);

    var out: ShaderEffectVarying;
    // Fullscreen 2D effects should use the front of the custom-mesh depth range. Using 0.999 made
    // the stage background vulnerable to being hidden behind normal UI primitives depending on
    // backend depth state, which looked exactly like the static dark fallback.
    out.position = vec4<f32>(device_position, 0.0, 1.0);
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
    let seed = input.params2.w;
    let dim = input.params3.w;

    let aspect = input.bounds_size.x / max(input.bounds_size.y, 1.0);
    var p = (input.uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0) * 2.35;

    // Independent monotonic time advects the palette-noise field. Audio seek/pause never touches
    // this clock. The rate is intentionally visible: broad structures should travel several
    // percent of the viewport per second instead of appearing frozen at normal viewing distance.
    let t = time * 0.42 * motion;
    p = rotate2(p, (sin(time * 0.16 + seed * 6.28318) * 0.24) * motion);
    let drift = vec2<f32>(t * 0.34, -t * 0.26);

    // 4-octave Fractional Brownian Motion domain warp. q bends the palette-noise image while the
    // second FBM sample produces a continuously evolving low-frequency flow field.
    let q = vec2<f32>(
        fbm(p * 1.03 + drift + vec2<f32>(0.0, 0.0), seed + 0.11),
        fbm(p * 1.03 - drift * 0.81 + vec2<f32>(5.2, 1.3), seed + 0.37),
    ) - vec2<f32>(0.5);
    let warped = p + q * (1.72 * motion + 0.42);
    let flow = fbm(warped * 1.16 + vec2<f32>(-t * 0.31, t * 0.27), seed + 0.71);

    // Build the noise image exclusively from artwork colors. The three decorrelated samples move
    // in different directions so boundaries visibly fold and slide rather than simply translating.
    let n0 = value_noise(warped * 1.43 + vec2<f32>(t * 0.49, -t * 0.23), seed + 1.17);
    let n1 = value_noise(
        warped * 1.31 + vec2<f32>(7.1, 2.8) - vec2<f32>(t * 0.31, t * 0.41),
        seed + 2.03,
    );
    let n2 = value_noise(
        warped * 1.57 + vec2<f32>(-3.4, 8.6) + vec2<f32>(t * 0.27, -t * 0.44),
        seed + 2.89,
    );

    let w0 = 0.18 + smoothstep(0.17, 0.83, n0 + (flow - 0.5) * 0.44) * 0.98;
    let w1 = 0.18 + smoothstep(0.15, 0.85, n1 - (flow - 0.5) * 0.38) * 0.94;
    let w2 = 0.14 + smoothstep(0.19, 0.81, n2 + (q.x - q.y) * 0.50) * 0.84;
    let weight_sum = max(w0 + w1 + w2, 0.001);
    var color = (dominant * w0 + secondary * w1 + tertiary * w2) / weight_sum;

    // Keep the field soft, but allow enough luminance movement that similarly colored palettes
    // still reveal motion instead of looking like one static flat fill.
    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = mix(vec3<f32>(luminance), color, 1.36);
    color *= 0.86 + (flow - 0.5) * 0.28;

    let centered = (input.uv - vec2<f32>(0.5)) * vec2<f32>(0.86, 1.06);
    let vignette = smoothstep(0.27, 0.77, length(centered));
    let dark_mix = clamp(dim * 0.46 + vignette * 0.18, 0.08, 0.46);
    color = mix(color, dark, dark_mix);

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
