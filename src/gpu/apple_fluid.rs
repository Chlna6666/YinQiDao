use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

use gpui::{Context, IntoElement, Render, Window, div, prelude::*, rgb};

use crate::artwork::ArtworkPalette;

use super::{ShaderEffectProgram, ShaderParams16, shader_effect_canvas};

const APPLE_FLUID_SHADER: &str = include_str!("apple_fluid.wgsl");

pub(crate) fn apple_fluid_program() -> std::result::Result<Arc<ShaderEffectProgram>, String> {
    static PROGRAM: OnceLock<std::result::Result<Arc<ShaderEffectProgram>, String>> =
        OnceLock::new();
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
    time_seconds: f32,
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
        time_seconds.rem_euclid(21_600.0)
    } else {
        seed * 97.0
    };
    let motion = if dynamic { 1.0 } else { 0.0 };
    let dim = (palette.mask_alpha * 0.64).clamp(0.18, 0.46);

    ShaderParams16::from_columns([
        [dominant[0], dominant[1], dominant[2], time],
        [secondary[0], secondary[1], secondary[2], motion],
        [tertiary[0], tertiary[1], tertiary[2], seed],
        [dark[0], dark[1], dark[2], dim],
    ])
}

pub(crate) struct AppleFluidView {
    track_id: i64,
    palette: Option<ArtworkPalette>,
    dynamic: bool,
    active: bool,
    clock_started_at: Instant,
}

impl AppleFluidView {
    pub(crate) fn new() -> Self {
        Self {
            track_id: 0,
            palette: None,
            dynamic: true,
            active: false,
            clock_started_at: Instant::now(),
        }
    }

    pub(crate) fn sync(
        &mut self,
        track_id: i64,
        palette: Option<ArtworkPalette>,
        dynamic: bool,
        active: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self.track_id != track_id
            || self.palette != palette
            || self.dynamic != dynamic
            || self.active != active;
        self.track_id = track_id;
        self.palette = palette;
        self.dynamic = dynamic;
        self.active = active;
        if changed {
            cx.notify();
        }
    }
}

impl Render for AppleFluidView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // This is a view-driven GPU animation. Request the next frame from inside AppleFluidView
        // itself so GPUI invalidates only this retained entity and guarantees presentation on the
        // next vsync. A timer + cx.notify() can be coalesced/replayed by the retained scene and can
        // leave custom-mesh draw parameters stuck on the first frame.
        if self.active && self.dynamic {
            window.request_animation_frame();
        }

        let elapsed = self.clock_started_at.elapsed().as_secs_f32();
        if let Ok(program) = apple_fluid_program() {
            return shader_effect_canvas(
                program,
                apple_fluid_params(self.track_id, self.palette.as_ref(), elapsed, self.dynamic),
            );
        }

        let palette = self.palette.clone().unwrap_or_default();
        let dark = ((palette.dark_ambient_rgb[0] as u32) << 16)
            | ((palette.dark_ambient_rgb[1] as u32) << 8)
            | palette.dark_ambient_rgb[2] as u32;
        div().size_full().bg(rgb(dark)).into_any_element()
    }
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

    #[test]
    fn fluid_time_is_not_audio_position() {
        let first = apple_fluid_params(7, None, 12.5, true);
        let second = apple_fluid_params(7, None, 18.5, true);
        assert_ne!(first, second);
    }
}
