use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::{Context, IntoElement, Render, Timer, Window, div, prelude::*, rgb};

use crate::artwork::ArtworkPalette;

use super::{ShaderEffectProgram, ShaderParams16, shader_effect_canvas};

const APPLE_FLUID_SHADER: &str = include_str!("apple_fluid.wgsl");
const FLUID_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const FLUID_MOUNT_LIVENESS: Duration = Duration::from_millis(160);

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
    _dynamic: bool,
) -> ShaderParams16 {
    let fallback = ArtworkPalette::default();
    let palette = palette.unwrap_or(&fallback);
    let dominant = rgb01(palette.dominant_rgb);
    let secondary = rgb01(palette.secondary_rgb);
    let tertiary = mix3(dominant, secondary, 0.46);
    let dark = rgb01(palette.dark_ambient_rgb);
    let seed = ((track_id.unsigned_abs() % 10_007) as f32 / 10_007.0).fract();
    let time = time_seconds.rem_euclid(21_600.0);
    let motion = 1.0;
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
    stage_visible: bool,
    animation_seconds: f32,
    last_tick_at: Instant,
    last_render_at: Instant,
    timer_started: bool,
    shader_available: bool,
}

impl AppleFluidView {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            track_id: 0,
            palette: None,
            stage_visible: false,
            animation_seconds: 0.0,
            last_tick_at: now,
            last_render_at: now,
            timer_started: false,
            shader_available: true,
        }
    }

    pub(crate) fn sync(
        &mut self,
        track_id: i64,
        palette: Option<ArtworkPalette>,
        _dynamic: bool,
        stage_visible: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self.track_id != track_id
            || self.palette != palette
            || self.stage_visible != stage_visible;
        self.track_id = track_id;
        self.palette = palette;
        self.stage_visible = stage_visible;
        if changed {
            self.last_tick_at = Instant::now();
            cx.notify();
        }
    }

    fn start_timer(&mut self, cx: &mut Context<Self>) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;

        cx.spawn(async move |this, cx| -> Result<()> {
            loop {
                Timer::after(FLUID_FRAME_INTERVAL).await;
                if this
                    .update(cx, |this, cx| {
                        let now = Instant::now();
                        let mounted = now.saturating_duration_since(this.last_render_at)
                            <= FLUID_MOUNT_LIVENESS;
                        let should_animate =
                            this.stage_visible && mounted && this.shader_available;

                        if should_animate {
                            let delta = now
                                .saturating_duration_since(this.last_tick_at)
                                .as_secs_f32()
                                .min(0.05);
                            this.animation_seconds =
                                (this.animation_seconds + delta).rem_euclid(21_600.0);
                            cx.notify();
                        }
                        this.last_tick_at = now;
                    })
                    .is_err()
                {
                    break;
                }
            }
            Ok(())
        })
        .detach();
    }
}

impl Render for AppleFluidView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_timer(cx);
        self.last_render_at = Instant::now();

        match apple_fluid_program() {
            Ok(program) => {
                self.shader_available = true;
                return shader_effect_canvas(
                    program,
                    apple_fluid_params(
                        self.track_id,
                        self.palette.as_ref(),
                        self.animation_seconds,
                        true,
                    ),
                );
            }
            Err(error) => {
                self.shader_available = false;
                static LOGGED_SHADER_ERROR: OnceLock<()> = OnceLock::new();
                if LOGGED_SHADER_ERROR.set(()).is_ok() {
                    tracing::error!(error = %error, "Apple fluid shader initialization failed");
                }
            }
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

    #[test]
    fn legacy_dynamic_blur_flag_no_longer_freezes_immersive_fluid() {
        let enabled = apple_fluid_params(7, None, 12.5, true);
        let disabled = apple_fluid_params(7, None, 12.5, false);
        assert_eq!(enabled, disabled);
    }
}
