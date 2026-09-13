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
    full_effect: bool,
) -> ShaderParams16 {
    let fallback = ArtworkPalette::default();
    let palette = palette.unwrap_or(&fallback);
    let dominant = rgb01(palette.dominant_rgb);
    let secondary = rgb01(palette.secondary_rgb);
    let tertiary = mix3(dominant, secondary, 0.46);
    let dark = rgb01(palette.dark_ambient_rgb);
    let seed = ((track_id.unsigned_abs() % 10_007) as f32 / 10_007.0).fract();
    let time = time_seconds.rem_euclid(21_600.0);
    // The same shader/pipeline is used for both paths. While the stage is prewarming or the drawer
    // is moving we set motion to zero, which selects the cheap static fragment path. This still
    // exercises the backend pipeline during prewarm without paying the full-screen FBM cost on
    // every drawer frame. Once the stage settles, motion=1 enables the full fluid effect.
    let motion = if full_effect { 1.0 } else { 0.0 };
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
    full_effect_ready: bool,
    full_effect_resume_armed: bool,
    playing: bool,
    animation_seconds: f32,
    last_frame_at: Instant,
    shader_available: bool,
}

impl AppleFluidView {
    pub(crate) fn new() -> Self {
        // Parse/validate WGSL and build the shared mesh as soon as the retained entity is created.
        // The existing offscreen stage prewarm can then spend its frame on backend pipeline creation
        // instead of also paying Naga/source construction during the first immersive render.
        let shader_available = apple_fluid_program().is_ok();
        Self {
            track_id: 0,
            palette: None,
            stage_visible: false,
            full_effect_ready: false,
            full_effect_resume_armed: false,
            playing: false,
            animation_seconds: 0.0,
            last_frame_at: Instant::now(),
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
        self.track_id = track_id;
        self.palette = palette;
        self.stage_visible = stage_visible;

        if visibility_changed {
            // Keep the first fully settled Stage frame on the cheap static shader path. The full
            // procedural effect is re-enabled only after that frame has actually been presented,
            // so the Stage terminal reconciliation and the expensive fullscreen fragment workload
            // cannot land on the same frame.
            self.full_effect_ready = false;
            self.full_effect_resume_armed = false;
        }
        if changed {
            self.last_frame_at = Instant::now();
            cx.notify();
        }
    }

    pub(crate) fn set_playing(&mut self, playing: bool, cx: &mut Context<Self>) {
        if self.playing == playing {
            return;
        }
        self.playing = playing;
        self.last_frame_at = Instant::now();
        cx.notify();
    }
}

impl Render for AppleFluidView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.stage_visible && !self.full_effect_ready && !self.full_effect_resume_armed {
            self.full_effect_resume_armed = true;
            let entity = cx.entity();
            window.on_next_frame(move |_window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    view.full_effect_resume_armed = false;
                    if view.stage_visible && !view.full_effect_ready {
                        view.full_effect_ready = true;
                        view.last_frame_at = Instant::now();
                        cx.notify();
                    }
                });
            });
        }

        match apple_fluid_program() {
            Ok(program) => {
                self.shader_available = true;
                let now = window.animation_time();
                let full_effect = self.stage_visible && self.full_effect_ready;
                if full_effect && self.playing {
                    let delta = now
                        .saturating_duration_since(self.last_frame_at)
                        .as_secs_f32()
                        .min(0.05);
                    self.animation_seconds =
                        (self.animation_seconds + delta).rem_euclid(21_600.0);
                    // The pinned GPUI fork targets request_animation_frame() at the currently
                    // rendering Entity. Fluid therefore follows the display's real vsync cadence
                    // without waking MusicApp or imposing a fixed 30 Hz software timer.
                    window.request_animation_frame();
                }
                self.last_frame_at = now;

                return shader_effect_canvas(
                    program,
                    apple_fluid_params(
                        self.track_id,
                        self.palette.as_ref(),
                        self.animation_seconds,
                        full_effect,
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
    fn stage_prewarm_uses_a_cheaper_static_shader_parameter_set() {
        let warmup = apple_fluid_params(7, None, 12.5, false);
        let active = apple_fluid_params(7, None, 12.5, true);
        assert_ne!(warmup, active);
    }
}
