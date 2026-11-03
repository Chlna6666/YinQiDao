use std::time::{Duration, Instant};

use gpui::{Context, IntoElement, Render, Window, div, prelude::*, rgb};

use crate::artwork::ArtworkPalette;

use super::{ShaderKind, ShaderParams16, next_shader_effect_id, shader_effect_canvas};

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
    effect_id: u64,
}

impl AppleFluidView {
    fn freeze_animation_clock(&mut self, now: Instant) {
        if let Some(started_at) = self.running_since.take() {
            self.animation_seconds = (self.animation_seconds
                + now.saturating_duration_since(started_at).as_secs_f32())
            .rem_euclid(21_600.0);
        }
    }

    fn current_animation_seconds(&self, now: Instant) -> f32 {
        let elapsed = self.running_since.map_or(0.0, |started_at| {
            now.saturating_duration_since(started_at).as_secs_f32()
        });
        (self.animation_seconds + elapsed).rem_euclid(21_600.0)
    }

    pub(crate) fn new() -> Self {
        Self {
            track_id: 0,
            palette: None,
            stage_visible: false,
            playing: false,
            animation_seconds: 0.0,
            running_since: None,
            effect_id: next_shader_effect_id(),
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

// The renderer extension is time-dependent, but that flag alone doesn't guarantee
// a new present when the rest of the application is idle. Keep a bounded local
// frame clock as long as audio actually plays and the Stage remains visible.
const FLUID_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

#[inline]
fn fluid_should_animate(stage_visible: bool, playing: bool, minimized: bool) -> bool {
    stage_visible && playing && !minimized
}

impl Render for AppleFluidView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The retained GPU extension advances with RendererExtensionContext::frame_time
        // (monotonic Instant), not GPUI's UI animation clock. That UI clock can
        // be paused or sampled from an older committed layout after focus loss.
        // Mixing the two made a live background appear frozen until another
        // window event repainted the fluid snapshot.
        let now = Instant::now();
        let running = fluid_should_animate(self.stage_visible, self.playing, window.is_minimized());

        if running {
            // An active audio stream must animate the fluid even with no lyrics,
            // pointer movement, or UI state transitions. Invalidate only this
            // retained surface; never poll or rebuild MusicApp at 60 Hz.
            window.request_invalidation_at(now + FLUID_FRAME_INTERVAL, cx);
            if self.running_since.is_none() {
                self.running_since = Some(now);
            }
        } else {
            self.freeze_animation_clock(now);
        }

        // The native GPUI renderer extension samples the presentation clock and
        // changes one 96-byte uniform block, without repainting the view tree.
        let effect = shader_effect_canvas(
            ShaderKind::Fluid,
            self.effect_id,
            apple_fluid_params(
                self.track_id,
                self.palette.as_ref(),
                self.current_animation_seconds(now),
                true,
                running,
            ),
            running,
        );

        let palette = self.palette.clone().unwrap_or_default();
        let dark = ((palette.dark_ambient_rgb[0] as u32) << 16)
            | ((palette.dark_ambient_rgb[1] as u32) << 8)
            | palette.dark_ambient_rgb[2] as u32;
        div()
            .relative()
            .size_full()
            .bg(rgb(dark))
            .child(effect)
            .into_any_element()
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
    fn fluid_clock_is_driven_by_audio_not_lyrics() {
        assert!(fluid_should_animate(true, true, false));
        assert!(!fluid_should_animate(true, false, false));
        assert!(!fluid_should_animate(false, true, false));
        assert!(!fluid_should_animate(true, true, true));
    }

    #[test]
    fn apple_fluid_shader_validates() {
        gpui::WgslShaderSource::from_source(
            "src/gpu/apple_fluid.wgsl",
            include_str!("apple_fluid.wgsl"),
        )
        .expect("Apple fluid WGSL should validate");
    }

    #[test]
    fn fluid_clock_continues_across_focus_and_preserves_pause_phase() {
        use std::time::Duration;

        let mut fluid = AppleFluidView::new();
        let started_at = Instant::now();
        fluid.animation_seconds = 12.0;
        fluid.running_since = Some(started_at);
        let running = fluid.current_animation_seconds(started_at + Duration::from_secs(3));
        assert!((running - 15.0).abs() < 0.001);

        fluid.freeze_animation_clock(started_at + Duration::from_secs(3));
        assert!(
            (fluid.current_animation_seconds(started_at + Duration::from_secs(10)) - 15.0).abs()
                < 0.001
        );

        fluid.running_since = Some(started_at + Duration::from_secs(10));
        let resumed = fluid.current_animation_seconds(started_at + Duration::from_secs(11));
        assert!((resumed - 16.0).abs() < 0.001);
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
