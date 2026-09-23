use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationDriver, AnimationExt as _, AnimationProperty, AnimationSpec, Context,
    IntoElement, Render, RepeatMode, Window, div, prelude::*, rgb,
};

use crate::artwork::ArtworkPalette;

use super::{ShaderEffectProgram, ShaderParams16, shader_effect_canvas};

const APPLE_FLUID_SHADER: &str = include_str!("apple_fluid.wgsl");
const FLUID_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

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
    time_base_seconds: f32,
    full_effect: bool,
    renderer_clock_running: bool,
) -> ShaderParams16 {
    let fallback = ArtworkPalette::default();
    let palette = palette.unwrap_or(&fallback);
    let dominant = rgb01(palette.dominant_rgb);
    let secondary = rgb01(palette.secondary_rgb);
    let tertiary = mix3(dominant, secondary, 0.46);
    let dark = rgb01(palette.dark_ambient_rgb);
    let seed = ((track_id.unsigned_abs() % 10_007) as f32 / 10_007.0).fract();
    // A non-negative seed means the shader should add Nova's renderer presentation clock. Paused
    // frames encode the exact same seed in [-2, -1] so the shader can freeze time without changing
    // any color/noise identity.
    let packed_seed = if renderer_clock_running {
        seed
    } else {
        -(seed + 1.0)
    };
    // Keep the cheap parameter branch available as a fallback/diagnostic path, but the retained
    // immersive Stage always paints the full field. Prewarm and drawer motion now reduce work by
    // freezing time and withholding RAF rather than switching to a visually different gradient.
    let motion = if full_effect { 1.0 } else { 0.0 };
    let dim = (palette.mask_alpha * 0.64).clamp(0.18, 0.46);

    ShaderParams16::from_columns([
        [dominant[0], dominant[1], dominant[2], time_base_seconds],
        [secondary[0], secondary[1], secondary[2], motion],
        [tertiary[0], tertiary[1], tertiary[2], packed_seed],
        [dark[0], dark[1], dark[2], dim],
    ])
}

pub(crate) struct AppleFluidView {
    track_id: i64,
    palette: Option<ArtworkPalette>,
    stage_visible: bool,
    playing: bool,
    animation_seconds: f32,
    running_since: Option<Instant>,
    shader_available: bool,
}

impl AppleFluidView {
    fn freeze_animation_clock(&mut self, now: Instant) {
        if let Some(started_at) = self.running_since.take() {
            self.animation_seconds = (
                self.animation_seconds
                    + now.saturating_duration_since(started_at).as_secs_f32()
            )
                .rem_euclid(21_600.0);
        }
    }

    fn current_animation_seconds(&self, now: Instant) -> f32 {
        let elapsed = self
            .running_since
            .map_or(0.0, |started_at| {
                now.saturating_duration_since(started_at).as_secs_f32()
            });
        (self.animation_seconds + elapsed).rem_euclid(21_600.0)
    }

    pub(crate) fn new() -> Self {
        // Parse/validate WGSL and build the shared mesh as soon as the retained entity is created.
        // The offscreen Stage prewarm then paints the exact full-fluid visual that will later be
        // replayed by the drawer instead of warming a cheaper but visibly different approximation.
        let shader_available = apple_fluid_program().is_ok();
        Self {
            track_id: 0,
            palette: None,
            stage_visible: false,
            playing: false,
            animation_seconds: 0.0,
            running_since: None,
            shader_available,
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
        let visibility_changed = self.stage_visible != stage_visible;
        let changed = self.track_id != track_id || self.palette != palette || visibility_changed;
        if self.stage_visible && !stage_visible {
            self.freeze_animation_clock(Instant::now());
        }
        self.track_id = track_id;
        self.palette = palette;
        self.stage_visible = stage_visible;

        if changed {
            cx.notify();
        }
    }

    pub(crate) fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        if self.playing == playing {
            return;
        }
        if self.playing && !playing {
            self.freeze_animation_clock(Instant::now());
        }
        self.playing = playing;
        cx.notify();
    }
}

impl Render for AppleFluidView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        match apple_fluid_program() {
            Ok(program) => {
                self.shader_available = true;
                let now = window.animation_time();
                let animate = self.stage_visible && self.playing && !window.is_minimized();

                if animate {
                    if self.running_since.is_none() {
                        self.running_since = Some(now);
                    }
                } else {
                    self.freeze_animation_clock(now);
                }

                let current_time = self.current_animation_seconds(now);
                // Nova publishes a 60 Hz-quantized presentation tick in GlobalParams on every
                // retained present. Store the inverse offset here so the first renderer-owned frame
                // is phase-continuous, then let the shader advance without touching this View.
                let renderer_time_60hz =
                    (window.presentation_time_seconds() * 60.0).floor() / 60.0;
                let time_base = if animate {
                    current_time - renderer_time_60hz
                } else {
                    current_time
                };

                let effect = shader_effect_canvas(
                    program,
                    apple_fluid_params(
                        self.track_id,
                        self.palette.as_ref(),
                        time_base,
                        true,
                        animate,
                    ),
                );

                if animate {
                    // Opacity 1 -> 1 deliberately has no visual effect. Its only job is to own one
                    // renderer timeline so Nova gets presentation-only frames at 60 Hz. No View
                    // notify, Taffy pass, or root traversal is required for ambient motion.
                    return effect
                        .with_animation(
                            "apple-fluid-presentation-clock",
                            Animation::from_spec(
                                AnimationSpec::new(Duration::from_secs(1))
                                    .repeat(RepeatMode::Forever)
                                    .presentation_interval(FLUID_FRAME_INTERVAL)
                                    .driver(AnimationDriver::Gpu),
                            )
                            .with_property(AnimationProperty::opacity(1.0, 1.0)),
                            |element, _| element,
                        )
                        .into_any_element();
                }

                return effect;
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
        let first = apple_fluid_params(7, None, 12.5, true, false);
        let second = apple_fluid_params(7, None, 18.5, true, false);
        assert_ne!(first, second);
    }

    #[test]
    fn renderer_clock_state_changes_only_time_control_encoding() {
        let frozen = apple_fluid_params(7, None, 12.5, true, false);
        let running = apple_fluid_params(7, None, 12.5, true, true);
        assert_ne!(frozen, running);
    }

    #[test]
    fn cheap_fallback_differs_from_full_fluid_parameter_set() {
        let fallback = apple_fluid_params(7, None, 12.5, false, false);
        let full = apple_fluid_params(7, None, 12.5, true, false);
        assert_ne!(fallback, full);
    }
}
