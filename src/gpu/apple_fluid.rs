use std::sync::{Arc, OnceLock};

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
