from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, got {count}: {old[:120]!r}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


Path("src/gpu").mkdir(parents=True, exist_ok=True)

Path("src/gpu/mod.rs").write_text(
    '''mod apple_fluid;
mod shader_effect;

pub(crate) use apple_fluid::{apple_fluid_params, apple_fluid_program};
pub(crate) use shader_effect::{ShaderEffectProgram, ShaderParams16, shader_effect_canvas};
''',
    encoding="utf-8",
)

Path("src/gpu/shader_effect.rs").write_text(
    r'''use std::sync::Arc;

use gpui::{
    AnyElement, GpuMesh3d, GpuMesh3dDrawParameters, GpuMesh3dDrawRanges, GpuMesh3dRange,
    GpuMesh3dShader, GpuMesh3dVertex, WgslShaderSource, canvas, prelude::*,
};

/// Standard 16-float payload for application-owned fullscreen WGSL effects.
///
/// GPUI's current custom-mesh ABI already exposes one `mat4x4<f32>` per draw. YinQiDao treats
/// that storage as four generic `vec4<f32>` columns rather than a transform matrix, which keeps
/// effect-specific state at 64 bytes without changing the renderer resource layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ShaderParams16 {
    columns: [[f32; 4]; 4],
}

impl ShaderParams16 {
    pub(crate) const fn from_columns(columns: [[f32; 4]; 4]) -> Self {
        Self { columns }
    }

    const fn as_draw_parameters(self) -> GpuMesh3dDrawParameters {
        GpuMesh3dDrawParameters {
            view_projection_model: self.columns,
        }
    }
}

/// Validated WGSL program plus the GPU-resident fullscreen quad used by 2D shader effects.
///
/// The WGSL source is validated once by GPUI/Naga. Nova then caches the backend render pipeline
/// by `GpuMesh3dShaderId`, while the quad's vertex/index buffers remain resident across frames.
#[derive(Clone)]
pub(crate) struct ShaderEffectProgram {
    mesh: Arc<GpuMesh3d>,
}

impl ShaderEffectProgram {
    pub(crate) fn from_source(
        label: impl Into<String>,
        source: impl Into<String>,
        vertex_entry_point: impl Into<String>,
        fragment_entry_point: impl Into<String>,
    ) -> Result<Arc<Self>, String> {
        let source = WgslShaderSource::from_source(label, source).map_err(|error| error.to_string())?;
        let shader = Arc::new(GpuMesh3dShader::new(
            Arc::new(source),
            vertex_entry_point,
            fragment_entry_point,
        ));

        let vertices = vec![
            GpuMesh3dVertex {
                position: [-1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GpuMesh3dVertex {
                position: [1.0, -1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GpuMesh3dVertex {
                position: [1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
            GpuMesh3dVertex {
                position: [-1.0, 1.0, 0.0],
                color: [1.0, 1.0, 1.0, 1.0],
            },
        ];
        let indices = vec![0, 1, 2, 0, 2, 3];
        let mesh = Arc::new(GpuMesh3d::new(
            vertices,
            indices,
            GpuMesh3dDrawRanges {
                opaque: GpuMesh3dRange { start: 0, count: 6 },
                ..Default::default()
            },
            [0.0, 0.0, 0.0],
            1.0,
            1.0,
            shader,
        ));

        Ok(Arc::new(Self { mesh }))
    }
}

/// Paint a validated fullscreen shader through GPUI's retained custom-mesh scene path.
///
/// The paint closure only emits one mesh draw and one 64-byte parameter block. It performs no
/// image decode, blur, heap construction, or shader compilation on animation frames.
pub(crate) fn shader_effect_canvas(
    program: Arc<ShaderEffectProgram>,
    params: ShaderParams16,
) -> AnyElement {
    let mesh = program.mesh.clone();
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_gpu_mesh_3d(bounds, mesh.clone(), params.as_draw_parameters());
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}
''',
    encoding="utf-8",
)

Path("src/gpu/apple_fluid.rs").write_text(
    r'''use std::sync::{Arc, OnceLock};

use crate::artwork::ArtworkPalette;

use super::{ShaderEffectProgram, ShaderParams16};

const APPLE_FLUID_SHADER: &str = include_str!("apple_fluid.wgsl");

pub(crate) fn apple_fluid_program() -> Result<Arc<ShaderEffectProgram>, String> {
    static PROGRAM: OnceLock<Result<Arc<ShaderEffectProgram>, String>> = OnceLock::new();
    PROGRAM
        .get_or_init(|| {
            ShaderEffectProgram::from_source(
                "src/gpu/apple_fluid.wgsl",
                APPLE_FLUID_SHADER,
                "vs_shader_effect",
                "fs_apple_fluid_opaque",
            )
        })
        .clone()
}

pub(crate) fn apple_fluid_params(
    track_id: i64,
    palette: Option<&ArtworkPalette>,
    position_ms: u64,
    dynamic: bool,
) -> ShaderParams16 {
    let fallback = ArtworkPalette::default();
    let palette = palette.unwrap_or(&fallback);
    let dominant = rgb01(palette.dominant_rgb);
    let secondary = rgb01(palette.secondary_rgb);
    let tertiary = mix3(dominant, secondary, 0.46);
    let dark = rgb01(palette.dark_ambient_rgb);
    let seed = ((track_id.unsigned_abs() % 10_007) as f32 / 10_007.0).fract();
    let time = if dynamic {
        ((position_ms % 3_600_000) as f32) * 0.001
    } else {
        seed * 150.0
    };
    let motion = if dynamic { 1.0 } else { 0.0 };
    let dim = (palette.mask_alpha * 0.72).clamp(0.24, 0.52);

    ShaderParams16::from_columns([
        [dominant[0], dominant[1], dominant[2], time],
        [secondary[0], secondary[1], secondary[2], motion],
        [tertiary[0], tertiary[1], tertiary[2], seed],
        [dark[0], dark[1], dark[2], dim],
    ])
}

fn rgb01(rgb: [u8; 3]) -> [f32; 3] {
    [
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ]
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_fluid_shader_validates() {
        apple_fluid_program().expect("Apple fluid WGSL should validate");
    }
}
''',
    encoding="utf-8",
)

Path("src/gpu/apple_fluid.wgsl").write_text(
    r'''struct ShaderEffectDrawParameters {
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
''',
    encoding="utf-8",
)

replace_once(
    "src/main.rs",
    "mod artwork;\nmod audio;\n",
    "mod artwork;\nmod audio;\nmod gpu;\n",
)

replace_once(
    "src/ui/player_stage.rs",
    '''use gpui::{
    Animation, AnimationDirection, AnimationExt as _, AnimationProperty, AnimationSpec,
    CompositeLayerExt as _, Context, Easing,
    EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, RepeatMode, SharedString, Transition,
    TransitionProperty,
    StatefulInteractiveElement as _, div, hsla, img, linear_color_stop, linear_gradient,
    point, prelude::*, px, relative, rgb,
};''',
    '''use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, Context, Easing,
    EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, Transition,
    TransitionProperty, StatefulInteractiveElement as _, div, hsla, img, linear_color_stop,
    linear_gradient, point, prelude::*, px, rgb,
};''',
)

replace_once(
    "src/ui/player_stage.rs",
    '''use crate::{
    artwork::ArtworkPalette,
    audio::PlayerCommand,
    lyrics::LyricLine,
    model::{PlaybackState, PlayerSnapshot, Track},
};''',
    '''use crate::{
    artwork::ArtworkPalette,
    audio::PlayerCommand,
    gpu::{apple_fluid_params, apple_fluid_program, shader_effect_canvas},
    lyrics::LyricLine,
    model::{PlaybackState, PlayerSnapshot, Track},
};''',
)

replace_once(
    "src/ui/player_stage.rs",
    '''    let artwork = id.and_then(|id| app.artworks.get(&id).cloned());
    let blurred = id.and_then(|id| app.blurred_artworks.get(&id).cloned());
    let palette = id.and_then(|id| app.artwork_palettes.get(&id));''',
    '''    let artwork = id.and_then(|id| app.artworks.get(&id).cloned());
    let palette = id.and_then(|id| app.artwork_palettes.get(&id));''',
)

replace_once(
    "src/ui/player_stage.rs",
    '''        .child(ambient_background(
            id,
            artwork.clone(),
            blurred,
            palette,
            app.config.dynamic_blur,
            app.config.blur_radius,
        ))''',
    '''        .child(ambient_background(
            id,
            palette,
            app.config.dynamic_blur,
            snapshot.position_ms,
        ))''',
)

player = Path("src/ui/player_stage.rs")
text = player.read_text(encoding="utf-8")
start = text.index("fn ambient_background(")
end = text.index("fn ambient_palette(", start)
new_background = r'''fn ambient_background(
    id: Option<i64>,
    palette: Option<&ArtworkPalette>,
    dynamic: bool,
    position_ms: u64,
) -> gpui::AnyElement {
    let id = id.unwrap_or_default();
    let (c1, c2, c3, dark, mask) = ambient_palette(id, palette);
    let root = div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(dark);

    if let Ok(program) = apple_fluid_program() {
        return root
            .child(shader_effect_canvas(
                program,
                apple_fluid_params(id, palette, position_ms, dynamic),
            ))
            .into_any_element();
    }

    // Runtime WGSL validation/backend pipeline failures retain a cheap static fallback rather
    // than re-entering the former realtime blur path.
    root
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(linear_gradient(
                    128.0,
                    linear_color_stop(c1.opacity(0.62), 0.0),
                    linear_color_stop(c2.opacity(0.42), 1.0),
                )),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .bg(linear_gradient(
                    306.0,
                    linear_color_stop(c3.opacity(0.28), 0.0),
                    linear_color_stop(dark.opacity(mask.clamp(0.24, 0.52)), 1.0),
                )),
        )
        .into_any_element()
}

'''
player.write_text(text[:start] + new_background + text[end:], encoding="utf-8")
