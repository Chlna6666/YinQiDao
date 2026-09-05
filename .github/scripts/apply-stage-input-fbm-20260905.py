from pathlib import Path
import re


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise RuntimeError(f"{label}: expected exactly one match, got {count}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


def regex_once(path: str, pattern: str, replacement: str, label: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    next_text, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise RuntimeError(f"{label}: expected exactly one regex match, got {count}")
    p.write_text(next_text, encoding="utf-8")


# ---------------------------------------------------------------------------
# Unified slider: exact painted bounds + GPUI drag capture + mouse-up-out.
# ---------------------------------------------------------------------------
Path("src/ui/components/slider.rs").write_text(r'''use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Bounds, Div, ElementId, Empty, Global, Hsla, MouseButton, Pixels, Stateful, div, hsla,
    prelude::*, px, relative, rgb,
};

use crate::ui::theme;

#[derive(Clone, Copy, Debug)]
pub struct SliderStyle {
    pub track_height: Pixels,
    pub hover_track_height: Pixels,
    pub thumb_size: Pixels,
    pub hover_thumb_scale: f32,
    pub track_bg: Hsla,
    pub filled_color: Hsla,
    pub thumb_color: Hsla,
    pub thumb_border: Option<Hsla>,
}

impl Default for SliderStyle {
    fn default() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(7.0),
            thumb_size: px(12.0),
            hover_thumb_scale: 1.25,
            track_bg: hsla(0.0, 0.0, 1.0, 0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.15)),
        }
    }
}

impl SliderStyle {
    pub fn mini_progress() -> Self {
        Self {
            track_height: px(3.5),
            hover_track_height: px(5.5),
            thumb_size: px(11.0),
            hover_thumb_scale: 1.25,
            track_bg: rgb(0xe8_ea_ee).into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.15)),
        }
    }

    pub fn stage_progress() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(7.5),
            thumb_size: px(13.0),
            hover_thumb_scale: 1.25,
            track_bg: hsla(0.0, 0.0, 1.0, 0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: None,
        }
    }

    pub fn stage_volume() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(7.0),
            thumb_size: px(11.0),
            hover_thumb_scale: 1.25,
            track_bg: hsla(0.0, 0.0, 1.0, 0.20),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: None,
        }
    }

    pub fn mini_volume() -> Self {
        Self {
            track_height: px(5.0),
            hover_track_height: px(6.5),
            thumb_size: px(10.0),
            hover_thumb_scale: 1.20,
            track_bg: rgb(0xe0_e2_e8).into(),
            filled_color: theme::ACCENT_RED.into(),
            thumb_color: hsla(0.0, 0.0, 1.0, 1.0),
            thumb_border: Some(hsla(0.0, 0.0, 0.0, 0.12)),
        }
    }
}

#[derive(Clone)]
struct SliderDrag {
    id: String,
    thumb_size: Pixels,
    on_change: Rc<dyn Fn(f32, &mut App)>,
}

#[derive(Default)]
struct SliderInteractionState {
    active_drag_id: Option<String>,
}

impl Global for SliderInteractionState {}

fn ratio_from_position(position_x: Pixels, bounds: Bounds<Pixels>, thumb_size: Pixels) -> f32 {
    let thumb = f32::from(thumb_size);
    let usable_width = (f32::from(bounds.size.width) - thumb).max(1.0);
    let local = f32::from(position_x - bounds.left()) - thumb * 0.5;
    (local / usable_width).clamp(0.0, 1.0)
}

fn is_active_drag(id: &str, cx: &App) -> bool {
    cx.try_global::<SliderInteractionState>()
        .is_some_and(|state| state.active_drag_id.as_deref() == Some(id))
}

fn begin_active_drag(id: &str, cx: &mut App) {
    if !cx.has_global::<SliderInteractionState>() {
        cx.set_global(SliderInteractionState::default());
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.active_drag_id = Some(id.to_owned());
    });
}

fn end_active_drag(id: &str, cx: &mut App) -> bool {
    if !is_active_drag(id, cx) {
        return false;
    }
    cx.update_global(|state: &mut SliderInteractionState, _cx| {
        state.active_drag_id = None;
    });
    true
}

fn slider_visual(id: ElementId, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    let clamped_ratio = ratio.clamp(0.0, 1.0);
    let interaction_height = px(
        (f32::from(style.thumb_size) * style.hover_thumb_scale)
            .max(f32::from(style.hover_track_height)),
    );

    let mut thumb = div()
        .flex_none()
        .size(style.thumb_size)
        .rounded_full()
        .bg(style.thumb_color)
        .shadow_md()
        .hover(move |s| s.scale(style.hover_thumb_scale))
        .transition(theme::hover_transition());

    if let Some(border) = style.thumb_border {
        thumb = thumb.border_1().border_color(border);
    }

    let track = div()
        .w_full()
        .h(style.track_height)
        .rounded_full()
        .bg(style.track_bg)
        .hover(move |s| s.h(style.hover_track_height))
        .transition(theme::hover_transition())
        .child(
            div()
                .h_full()
                .w(relative(clamped_ratio))
                .rounded_full()
                .bg(style.filled_color),
        );

    div()
        .id(id)
        .relative()
        .cursor_pointer()
        .h(interaction_height)
        .flex()
        .items_center()
        .child(track)
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .right(style.thumb_size)
                .top(px(0.0))
                .bottom(px(0.0))
                .flex()
                .items_center()
                .child(div().flex_none().w(relative(clamped_ratio)).h(px(1.0)))
                .child(thumb),
        )
}

/// Pure visual slider kept for places that do not need pointer interaction.
pub fn smooth_slider(id: impl Into<ElementId>, ratio: f32, style: SliderStyle) -> Stateful<Div> {
    slider_visual(id.into(), ratio, style)
}

/// Slider with retained pointer drag semantics.
///
/// Mapping is derived from the element's actual painted bounds and exactly matches the thumb's
/// `(width - thumb_size)` travel range. GPUI's drag session continues dispatching `on_drag_move`
/// after the pointer leaves the slider, while `on_mouse_up_out` commits only when the button is
/// actually released outside the element.
pub fn interactive_slider(
    id: impl Into<ElementId>,
    ratio: f32,
    style: SliderStyle,
    on_change: impl Fn(f32, &mut App) + 'static,
    on_commit: impl Fn(f32, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let id_string = id.to_string();
    let on_change: Rc<dyn Fn(f32, &mut App)> = Rc::new(on_change);
    let on_commit: Rc<dyn Fn(f32, &mut App)> = Rc::new(on_commit);
    let bounds: Rc<RefCell<Option<Bounds<Pixels>>>> = Rc::default();

    let bounds_for_prepaint = bounds.clone();
    let bounds_for_down = bounds.clone();
    let bounds_for_up = bounds.clone();
    let bounds_for_up_out = bounds.clone();
    let id_for_down = id_string.clone();
    let id_for_down_out = id_string.clone();
    let id_for_drag = id_string.clone();
    let id_for_up = id_string.clone();
    let id_for_up_out = id_string.clone();
    let change_for_down = on_change.clone();
    let commit_for_up = on_commit.clone();
    let commit_for_up_out = on_commit.clone();
    let drag = SliderDrag {
        id: id_string,
        thumb_size: style.thumb_size,
        on_change,
    };

    slider_visual(id, ratio, style)
        .on_children_prepainted(move |children_bounds, _window, _cx| {
            // The track is the first child and spans the slider's full width. Preserve its X range
            // but use the unioned vertical interaction bounds from the root hitbox.
            if let Some(track_bounds) = children_bounds.first().copied() {
                *bounds_for_prepaint.borrow_mut() = Some(track_bounds);
            }
        })
        .on_drag(drag, |_: &SliderDrag, _, _, cx| cx.new(|_| Empty))
        .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            begin_active_drag(&id_for_down, cx);
            if let Some(bounds) = *bounds_for_down.borrow() {
                (change_for_down)(ratio_from_position(event.position.x, bounds, style.thumb_size), cx);
            }
        })
        .on_mouse_down_out(move |_event, _window, cx| {
            let _ = end_active_drag(&id_for_down_out, cx);
        })
        .on_drag_move::<SliderDrag>(move |event, _window, cx| {
            let drag = event.drag(cx);
            if drag.id != id_for_drag {
                return;
            }
            let ratio = ratio_from_position(event.event.position.x, event.bounds, drag.thumb_size);
            (drag.on_change)(ratio, cx);
        })
        .on_mouse_up(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            if !end_active_drag(&id_for_up, cx) {
                return;
            }
            if let Some(bounds) = *bounds_for_up.borrow() {
                (commit_for_up)(ratio_from_position(event.position.x, bounds, style.thumb_size), cx);
            }
        })
        .on_mouse_up_out(MouseButton::Left, move |event, _window, cx| {
            cx.stop_propagation();
            if !end_active_drag(&id_for_up_out, cx) {
                return;
            }
            if let Some(bounds) = *bounds_for_up_out.borrow() {
                (commit_for_up_out)(
                    ratio_from_position(event.position.x, bounds, style.thumb_size),
                    cx,
                );
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_mapping_matches_thumb_travel() {
        let bounds = Bounds::new(gpui::point(px(100.0), px(0.0)), gpui::size(px(210.0), px(12.0)));
        let thumb = px(10.0);
        assert_eq!(ratio_from_position(px(105.0), bounds, thumb), 0.0);
        assert!((ratio_from_position(px(205.0), bounds, thumb) - 0.5).abs() < 0.0001);
        assert_eq!(ratio_from_position(px(305.0), bounds, thumb), 1.0);
        assert_eq!(ratio_from_position(px(-500.0), bounds, thumb), 0.0);
        assert_eq!(ratio_from_position(px(900.0), bounds, thumb), 1.0);
    }
}
''', encoding="utf-8")

# ---------------------------------------------------------------------------
# GPU fluid: independent retained entity and FBM-driven procedural palette field.
# ---------------------------------------------------------------------------
Path("src/gpu/apple_fluid.rs").write_text(r'''use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::{Context, IntoElement, Render, Timer, Window, div, prelude::*, rgb};

use crate::artwork::ArtworkPalette;

use super::{ShaderEffectProgram, ShaderParams16, shader_effect_canvas};

const APPLE_FLUID_SHADER: &str = include_str!("apple_fluid.wgsl");
const ACTIVE_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const IDLE_FRAME_INTERVAL: Duration = Duration::from_millis(250);

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
    timer_started: bool,
}

impl AppleFluidView {
    pub(crate) fn new() -> Self {
        Self {
            track_id: 0,
            palette: None,
            dynamic: true,
            active: false,
            clock_started_at: Instant::now(),
            timer_started: false,
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

    fn start_clock(&mut self, cx: &mut Context<Self>) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        cx.spawn(async move |this, cx| -> Result<()> {
            let mut animate = false;
            loop {
                Timer::after(if animate {
                    ACTIVE_FRAME_INTERVAL
                } else {
                    IDLE_FRAME_INTERVAL
                })
                .await;
                match this.update(cx, |this, cx| {
                    let should_animate = this.active && this.dynamic;
                    if should_animate {
                        cx.notify();
                    }
                    should_animate
                }) {
                    Ok(next) => animate = next,
                    Err(_) => break,
                }
            }
            Ok(())
        })
        .detach();
    }
}

impl Render for AppleFluidView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.start_clock(cx);
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
''', encoding="utf-8")

Path("src/gpu/mod.rs").write_text(r'''mod apple_fluid;
mod shader_effect;

pub(crate) use apple_fluid::AppleFluidView;
pub(crate) use shader_effect::{ShaderEffectProgram, ShaderParams16, shader_effect_canvas};
''', encoding="utf-8")

Path("src/gpu/apple_fluid.wgsl").write_text(r'''struct ShaderEffectDrawParameters {
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
    let seed = input.params2.w;
    let dim = input.params3.w;

    let aspect = input.bounds_size.x / max(input.bounds_size.y, 1.0);
    var p = (input.uv - vec2<f32>(0.5)) * vec2<f32>(aspect, 1.0) * 2.35;

    // Independent monotonic time advects the palette-noise field. Seeking or pausing audio never
    // changes this clock. The rotation is deliberately visible but remains low frequency.
    let t = time * 0.20 * motion;
    p = rotate2(p, (sin(time * 0.11 + seed * 6.28318) * 0.16) * motion);
    let drift = vec2<f32>(t * 0.27, -t * 0.19);

    // Fractional Brownian Motion domain warp. q is a low-frequency 2D flow vector; the warped
    // coordinate is then used to sample a second FBM octave stack. This is the GPU equivalent of
    // deforming a palette-colored noise image rather than translating flat gradient blobs.
    let q = vec2<f32>(
        fbm(p * 1.03 + drift + vec2<f32>(0.0, 0.0), seed + 0.11),
        fbm(p * 1.03 - drift * 0.73 + vec2<f32>(5.2, 1.3), seed + 0.37),
    ) - vec2<f32>(0.5);
    let warped = p + q * (1.55 * motion + 0.42);
    let flow = fbm(warped * 1.16 + vec2<f32>(-t * 0.22, t * 0.18), seed + 0.71);

    // Construct a soft noise image exclusively from colors extracted from the current artwork.
    // Three decorrelated samples keep broad colored islands visible while FBM continuously bends
    // their boundaries, producing the Apple/Refined-style liquid color field without CPU blur.
    let n0 = value_noise(warped * 1.43 + vec2<f32>(t * 0.34, -t * 0.16), seed + 1.17);
    let n1 = value_noise(warped * 1.31 + vec2<f32>(7.1, 2.8) - vec2<f32>(t * 0.21, t * 0.29), seed + 2.03);
    let n2 = value_noise(warped * 1.57 + vec2<f32>(-3.4, 8.6) + vec2<f32>(t * 0.17, -t * 0.31), seed + 2.89);

    let w0 = 0.20 + smoothstep(0.18, 0.82, n0 + (flow - 0.5) * 0.36) * 0.94;
    let w1 = 0.20 + smoothstep(0.16, 0.84, n1 - (flow - 0.5) * 0.30) * 0.90;
    let w2 = 0.16 + smoothstep(0.20, 0.80, n2 + (q.x - q.y) * 0.42) * 0.78;
    let weight_sum = max(w0 + w1 + w2, 0.001);
    var color = (dominant * w0 + secondary * w1 + tertiary * w2) / weight_sum;

    // Preserve artwork identity while making the movement readable. A very low-frequency light
    // modulation reveals flow direction without producing visible grain or flashing.
    let luminance = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    color = mix(vec3<f32>(luminance), color, 1.34);
    color *= 0.88 + (flow - 0.5) * 0.18;

    let centered = (input.uv - vec2<f32>(0.5)) * vec2<f32>(0.86, 1.06);
    let vignette = smoothstep(0.27, 0.77, length(centered));
    let dark_mix = clamp(dim * 0.48 + vignette * 0.18, 0.08, 0.48);
    color = mix(color, dark, dark_mix);

    return vec4<f32>(clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
''', encoding="utf-8")

# ---------------------------------------------------------------------------
# Player stage: interactive sliders, wheel volume, full-bleed retained fluid entity.
# ---------------------------------------------------------------------------
replace_once(
    "src/ui/player_stage.rs",
    """use crate::{\n    artwork::ArtworkPalette,\n    audio::PlayerCommand,\n    gpu::{apple_fluid_params, apple_fluid_program, shader_effect_canvas},\n    lyrics::LyricLine,\n    model::{PlaybackState, PlayerSnapshot, Track},\n};""",
    """use crate::{\n    audio::PlayerCommand,\n    gpu::AppleFluidView,\n    lyrics::LyricLine,\n    model::{PlaybackState, PlayerSnapshot, Track},\n};""",
    "stage imports",
)
replace_once(
    "src/ui/player_stage.rs",
    "components::{SliderStyle, smooth_slider},",
    "components::{SliderStyle, interactive_slider},",
    "stage slider import",
)
replace_once(
    "src/ui/player_stage.rs",
    "pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {",
    "pub(super) fn render(\n    app: &MusicApp,\n    cx: &mut Context<MusicApp>,\n    fluid_background: gpui::Entity<AppleFluidView>,\n) -> gpui::AnyElement {",
    "stage render signature",
)
regex_once(
    "src/ui/player_stage.rs",
    r"\n        \.child\(ambient_background\(\n            id,\n            palette,\n            app\.config\.dynamic_blur,\n            snapshot\.position_ms,\n        \)\)",
    "\n        .child(ambient_background(fluid_background))",
    "stage background call",
)
replace_once(
    "src/ui/player_stage.rs",
    ".p_8()\n                .gap_6()",
    ".px_8()\n                .pt(px(54.0))\n                .pb_8()\n                .gap_6()",
    "stage foreground top inset",
)

# Remove dock-level hardcoded drag mapping. Slider owns the drag session now.
regex_once(
    "src/ui/player_stage.rs",
    r"\n        \.on_mouse_move\(cx\.listener\(\|this, event: &gpui::MouseMoveEvent, window, cx\| \{\n            this\.handle_stage_mouse_move\(event\.position, cx\);\n            if .*?\n        \}\)\)",
    "\n        .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _window, cx| {\n            this.handle_stage_mouse_move(event.position, cx);\n        }))",
    "stage dock mouse move",
)

old_stage_progress = r'''            smooth_slider(
                "stage-progress-track",
                app.displayed_progress_ratio(),
                SliderStyle::stage_progress(),
            )
            .flex_1()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.wake_stage_controls_immediately(cx);
                    let ratio = this.stage_progress_ratio(f32::from(event.position.x), window);
                    this.begin_drag(DragTarget::Progress, ratio, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                if (this.seeking || event.dragging())
                    && this.drag_target == Some(DragTarget::Progress)
                {
                    let ratio = this.stage_progress_ratio(f32::from(event.position.x), window);
                    this.update_drag_ratio(DragTarget::Progress, ratio, cx);
                }
            }))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.wake_stage_controls_immediately(cx);
                    this.commit_drag(cx);
                }),
            ),'''
new_stage_progress = r'''            interactive_slider(
                "stage-progress-track",
                app.displayed_progress_ratio(),
                SliderStyle::stage_progress(),
                {
                    let view = cx.entity().downgrade();
                    move |ratio, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.wake_stage_controls_immediately(cx);
                            if this.drag_target == Some(DragTarget::Progress) {
                                this.update_drag_ratio(DragTarget::Progress, ratio, cx);
                            } else {
                                this.begin_drag(DragTarget::Progress, ratio, cx);
                            }
                        });
                    }
                },
                {
                    let view = cx.entity().downgrade();
                    move |ratio, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.wake_stage_controls_immediately(cx);
                            if this.drag_target == Some(DragTarget::Progress) {
                                this.update_drag_ratio(DragTarget::Progress, ratio, cx);
                            } else {
                                this.begin_drag(DragTarget::Progress, ratio, cx);
                            }
                            this.commit_drag(cx);
                        });
                    }
                },
            )
            .flex_1(),'''
replace_once("src/ui/player_stage.rs", old_stage_progress, new_stage_progress, "stage progress slider")

old_stage_volume = r'''                    smooth_slider("stage-volume-track", volume, SliderStyle::stage_volume())
                        .w(px(72.0))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.wake_stage_controls_immediately(cx);
                                let ratio =
                                    this.stage_volume_ratio(f32::from(event.position.x), window);
                                this.begin_drag(DragTarget::Volume, ratio, cx);
                                this.send(PlayerCommand::SetVolume(ratio));
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            |this, event: &gpui::MouseMoveEvent, window, cx| {
                                if (this.volume_dragging || event.dragging())
                                    && this.drag_target == Some(DragTarget::Volume)
                                {
                                    let ratio = this
                                        .stage_volume_ratio(f32::from(event.position.x), window);
                                    if this.update_drag_ratio(DragTarget::Volume, ratio, cx) {
                                        this.send(PlayerCommand::SetVolume(ratio));
                                    }
                                }
                            },
                        ))
                        .on_mouse_up(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.wake_stage_controls_immediately(cx);
                                this.commit_drag(cx);
                            }),
                        ),'''
new_stage_volume = r'''                    interactive_slider(
                        "stage-volume-track",
                        volume,
                        SliderStyle::stage_volume(),
                        {
                            let view = cx.entity().downgrade();
                            move |ratio, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.wake_stage_controls_immediately(cx);
                                    if this.drag_target == Some(DragTarget::Volume) {
                                        this.update_drag_ratio(DragTarget::Volume, ratio, cx);
                                    } else {
                                        this.begin_drag(DragTarget::Volume, ratio, cx);
                                    }
                                    this.send(PlayerCommand::SetVolume(ratio));
                                });
                            }
                        },
                        {
                            let view = cx.entity().downgrade();
                            move |ratio, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.wake_stage_controls_immediately(cx);
                                    if this.drag_target == Some(DragTarget::Volume) {
                                        this.update_drag_ratio(DragTarget::Volume, ratio, cx);
                                    } else {
                                        this.begin_drag(DragTarget::Volume, ratio, cx);
                                    }
                                    this.commit_drag(cx);
                                });
                            }
                        },
                    )
                    .w(px(72.0))
                    .on_scroll_wheel(cx.listener(
                        |this, event: &gpui::ScrollWheelEvent, _window, cx| {
                            cx.stop_propagation();
                            let delta = event.delta.pixel_delta(px(48.0)).y;
                            if delta < px(0.0) {
                                this.adjust_volume(0.04, cx);
                            } else if delta > px(0.0) {
                                this.adjust_volume(-0.04, cx);
                            }
                            this.wake_stage_controls_immediately(cx);
                        },
                    )),'''
replace_once("src/ui/player_stage.rs", old_stage_volume, new_stage_volume, "stage volume slider")

# Replace former MusicApp-position-driven background with retained effect entity.
regex_once(
    "src/ui/player_stage.rs",
    r"fn ambient_background\(.*?\nfn rgb_to_hsla\(value: \[u8; 3\]\) -> gpui::Hsla \{.*?\n\}",
    r'''fn ambient_background(fluid_background: gpui::Entity<AppleFluidView>) -> gpui::AnyElement {
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .child(fluid_background)
        .into_any_element()
}''',
    "stage ambient helpers",
)

# ---------------------------------------------------------------------------
# Mini player: same bounds-aware drag implementation + volume wheel.
# ---------------------------------------------------------------------------
replace_once(
    "src/ui/player.rs",
    "components::{SliderStyle, smooth_slider},",
    "components::{SliderStyle, interactive_slider},",
    "mini slider import",
)

old_progress_render = r'''        smooth_slider("mini-progress-track", ratio, SliderStyle::mini_progress())
            .w_full()
            .on_mouse_down(gpui::MouseButton::Left, {
                let parent = parent.clone();
                let this = this.clone();
                move |event, window, cx| {
                    cx.stop_propagation();
                    let width = f32::from(window.bounds().size.width).max(1.0);
                    let ratio = (f32::from(event.position.x) / width).clamp(0.0, 1.0);
                    let _ = parent.update(cx, |app, app_cx| {
                        app.begin_drag(DragTarget::Progress, ratio, app_cx);
                    });
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
            .on_mouse_move({
                let parent = parent.clone();
                let this = this.clone();
                move |event, window, cx| {
                    let width = f32::from(window.bounds().size.width).max(1.0);
                    let ratio = (f32::from(event.position.x) / width).clamp(0.0, 1.0);
                    let changed = parent
                        .update(cx, |app, app_cx| {
                            if (app.seeking || event.dragging())
                                && app.drag_target == Some(DragTarget::Progress)
                            {
                                app.update_drag_ratio(DragTarget::Progress, ratio, app_cx)
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if changed {
                        let _ = this.update(cx, |_, cx| cx.notify());
                    }
                }
            })
            .on_mouse_up(gpui::MouseButton::Left, {
                let parent = parent.clone();
                let this = this.clone();
                move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = parent.update(cx, |app, app_cx| app.commit_drag(app_cx));
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
            .into_any_element()'''
new_progress_render = r'''        interactive_slider(
            "mini-progress-track",
            ratio,
            SliderStyle::mini_progress(),
            {
                let parent = parent.clone();
                let this = this.clone();
                move |ratio, cx| {
                    let _ = parent.update(cx, |app, app_cx| {
                        if app.drag_target == Some(DragTarget::Progress) {
                            app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                        } else {
                            app.begin_drag(DragTarget::Progress, ratio, app_cx);
                        }
                    });
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            },
            {
                let parent = parent.clone();
                let this = this.clone();
                move |ratio, cx| {
                    let _ = parent.update(cx, |app, app_cx| {
                        if app.drag_target == Some(DragTarget::Progress) {
                            app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                        } else {
                            app.begin_drag(DragTarget::Progress, ratio, app_cx);
                        }
                        app.commit_drag(app_cx);
                    });
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            },
        )
        .w_full()
        .into_any_element()'''
replace_once("src/ui/player.rs", old_progress_render, new_progress_render, "mini progress slider")

regex_once(
    "src/ui/player.rs",
    r"\n        \.on_mouse_move\(\n            cx\.listener\(\|this, event: &gpui::MouseMoveEvent, window, cx\| \{.*?\n            \}\),\n        \)\n        \.on_mouse_up\(\n            gpui::MouseButton::Left,\n            cx\.listener\(\|this, _, _, cx\| \{.*?\n            \}\),\n        \)",
    "",
    "mini root legacy drag handlers",
)

# Add a stable weak entity handle for mini-volume callbacks.
replace_once(
    "src/ui/player.rs",
    "    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());\n\n    div()",
    "    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());\n    let app_entity = cx.entity().downgrade();\n\n    div()",
    "mini weak entity",
)

old_mini_volume = r'''                                    smooth_slider(
                                        "mini-volume-bar",
                                        app.displayed_volume_ratio(),
                                        SliderStyle::mini_volume(),
                                    )
                                    .w(px(72.0))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(
                                            |this, event: &gpui::MouseDownEvent, window, cx| {
                                                cx.stop_propagation();
                                                let ratio = this.mini_volume_ratio(
                                                    f32::from(event.position.x),
                                                    window,
                                                );
                                                this.begin_drag(DragTarget::Volume, ratio, cx);
                                                this.send(PlayerCommand::SetVolume(ratio));
                                            },
                                        ),
                                    )
                                    .on_mouse_move(cx.listener(
                                        |this, event: &gpui::MouseMoveEvent, window, cx| {
                                            if (this.volume_dragging || event.dragging())
                                                && this.drag_target == Some(DragTarget::Volume)
                                            {
                                                let ratio = this.mini_volume_ratio(
                                                    f32::from(event.position.x),
                                                    window,
                                                );
                                                if this.update_drag_ratio(
                                                    DragTarget::Volume,
                                                    ratio,
                                                    cx,
                                                ) {
                                                    this.send(PlayerCommand::SetVolume(ratio));
                                                }
                                            }
                                        },
                                    ))
                                    .on_mouse_up(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.commit_drag(cx);
                                        }),
                                    ),'''
new_mini_volume = r'''                                    interactive_slider(
                                        "mini-volume-bar",
                                        app.displayed_volume_ratio(),
                                        SliderStyle::mini_volume(),
                                        {
                                            let view = app_entity.clone();
                                            move |ratio, cx| {
                                                let _ = view.update(cx, |this, cx| {
                                                    if this.drag_target == Some(DragTarget::Volume) {
                                                        this.update_drag_ratio(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    } else {
                                                        this.begin_drag(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    }
                                                    this.send(PlayerCommand::SetVolume(ratio));
                                                });
                                            }
                                        },
                                        {
                                            let view = app_entity.clone();
                                            move |ratio, cx| {
                                                let _ = view.update(cx, |this, cx| {
                                                    if this.drag_target == Some(DragTarget::Volume) {
                                                        this.update_drag_ratio(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    } else {
                                                        this.begin_drag(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    }
                                                    this.commit_drag(cx);
                                                });
                                            }
                                        },
                                    )
                                    .w(px(72.0))
                                    .on_scroll_wheel({
                                        let view = app_entity.clone();
                                        move |event: &gpui::ScrollWheelEvent, _window, cx| {
                                            cx.stop_propagation();
                                            let delta = event.delta.pixel_delta(px(48.0)).y;
                                            let _ = view.update(cx, |this, cx| {
                                                if delta < px(0.0) {
                                                    this.adjust_volume(0.04, cx);
                                                } else if delta > px(0.0) {
                                                    this.adjust_volume(-0.04, cx);
                                                }
                                            });
                                        }
                                    }),'''
replace_once("src/ui/player.rs", old_mini_volume, new_mini_volume, "mini volume slider")

# ---------------------------------------------------------------------------
# Shell: remove guessed window coordinate helpers, isolate fluid entity, full-bleed titlebar.
# ---------------------------------------------------------------------------
replace_once(
    "src/ui/shell.rs",
    "    pub(crate) lyrics_scroll_target_y: Option<f32>,\n    pub(crate) library_scroll_handle: gpui::UniformListScrollHandle,",
    "    pub(crate) lyrics_scroll_target_y: Option<f32>,\n    pub(crate) fluid_background: Option<Entity<crate::gpu::AppleFluidView>>,\n    pub(crate) artwork_online_fallback_requested: HashSet<TrackId>,\n    pub(crate) library_scroll_handle: gpui::UniformListScrollHandle,",
    "shell new state fields",
)
replace_once(
    "src/ui/shell.rs",
    "            lyrics_user_scrolling_until: None,\n            lyrics_scroll_target_y: None,\n            library_scroll_handle: gpui::UniformListScrollHandle::new(),",
    "            lyrics_user_scrolling_until: None,\n            lyrics_scroll_target_y: None,\n            fluid_background: None,\n            artwork_online_fallback_requested: HashSet::new(),\n            library_scroll_handle: gpui::UniformListScrollHandle::new(),",
    "shell new state init",
)

# Remove all four hardcoded window-space slider mapping helpers.
regex_once(
    "src/ui/shell.rs",
    r"\n    pub\(crate\) fn mini_progress_ratio\(&self, x: f32, window: &Window\) -> f32 \{.*?\n    \}\n\n    pub\(crate\) fn stage_volume_ratio\(&self, x: f32, window: &Window\) -> f32 \{.*?\n    \}\n",
    "\n",
    "hardcoded slider mappings",
)
replace_once(
    "src/ui/shell.rs",
    "                    .is_none_or(|c| (c - ratio).abs() >= 0.002);",
    "                    .is_none_or(|c| (c - ratio).abs() >= 0.0005);",
    "progress drag precision",
)
replace_once(
    "src/ui/shell.rs",
    "                    .is_none_or(|c| (c - ratio).abs() >= 0.004);",
    "                    .is_none_or(|c| (c - ratio).abs() >= 0.001);",
    "volume drag precision",
)

# Fluid view lifecycle helper before show_page.
replace_once(
    "src/ui/shell.rs",
    "    pub(crate) fn show_page(&mut self, page: AppPage, cx: &mut Context<Self>) {",
    r'''    fn ensure_fluid_background(&mut self, cx: &mut Context<Self>) -> Entity<crate::gpu::AppleFluidView> {
        if let Some(view) = &self.fluid_background {
            return view.clone();
        }
        let view = cx.new(|_| crate::gpu::AppleFluidView::new());
        self.fluid_background = Some(view.clone());
        view
    }

    pub(crate) fn show_page(&mut self, page: AppPage, cx: &mut Context<Self>) {''',
    "fluid entity helper",
)

# Local artwork failure requests exactly one online-artwork fallback attempt.
old_artwork_result = r'''                    Ok(None) => {
                        this.artwork_missing.insert(track_id);
                    }
                    Err(error) => {
                        this.artwork_missing.insert(track_id);
                        this.status = format!("封面读取失败：{error:#}");
                    }'''
new_artwork_result = r'''                    Ok(None) => {
                        this.artwork_missing.insert(track_id);
                        if this.config.online_metadata
                            && this.artwork_online_fallback_requested.insert(track_id)
                        {
                            this.enrichment_done.remove(&track_id);
                            this.status = "本地封面不可用，正在尝试联网封面…".into();
                            this.request_current_enrichment(cx);
                        }
                    }
                    Err(error) => {
                        this.artwork_missing.insert(track_id);
                        if this.config.online_metadata
                            && this.artwork_online_fallback_requested.insert(track_id)
                        {
                            this.enrichment_done.remove(&track_id);
                            this.status = format!("本地封面解析失败，正在尝试联网封面：{error:#}");
                            this.request_current_enrichment(cx);
                        } else {
                            this.status = format!("封面读取失败：{error:#}");
                        }
                    }'''
replace_once("src/ui/shell.rs", old_artwork_result, new_artwork_result, "current artwork fallback")

# Stage titlebar becomes a transparent overlay over the same fluid background.
replace_once(
    "src/ui/shell.rs",
    ".bg(hsla(0.0, 0.0, 0.0, 0.40))\n                    .border_b_1()\n                    .border_color(hsla(0.0, 0.0, 1.0, 0.08))",
    ".bg(hsla(0.0, 0.0, 0.0, 0.10))\n                    .border_b_1()\n                    .border_color(hsla(0.0, 0.0, 1.0, 0.05))",
    "stage titlebar glass",
)

# Synchronize isolated shader entity after stage/controls animation state is advanced.
replace_once(
    "src/ui/shell.rs",
    "        self.advance_lyrics_scroll_animation(dt);\n\n        if self.has_active_animations() {",
    r'''        self.advance_lyrics_scroll_animation(dt);

        let fluid_background = self.ensure_fluid_background(cx);
        let fluid_track_id = self.snapshot.current_track.as_ref().map_or(0, |track| track.id);
        let fluid_palette = self
            .artwork_palettes
            .get(&fluid_track_id)
            .cloned();
        let fluid_active = self.stage_progress > 0.001;
        let fluid_dynamic = self.config.dynamic_blur;
        let _ = fluid_background.update(cx, |view, cx| {
            view.sync(
                fluid_track_id,
                fluid_palette,
                fluid_dynamic,
                fluid_active,
                cx,
            );
        });

        if self.has_active_animations() {''',
    "fluid background sync",
)

# Render stage content full bleed first, then titlebar as overlay so fluid reaches window top.
old_stage_children = r'''                    .flex()
                    .flex_col()
                    .bg(rgb(0x10_11_1a))
                    .text_color(theme::TEXT_WHITE)
                    .on_mouse_move(cx.listener(
                        |this, event: &gpui::MouseMoveEvent, _window, cx| {
                            this.handle_stage_mouse_move(event.position, cx);
                        },
                    ))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.wake_stage_controls_immediately(cx);
                        }),
                    )
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.wake_stage_controls_immediately(cx);
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            if this.drag_target.is_some() {
                                this.commit_drag(cx);
                            }
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        this.wake_stage_controls(cx);
                        let key = event.keystroke.key.as_str();
                        if key == "escape" {
                            this.close_stage(cx);
                        } else if key == "space" {
                            this.toggle_play(cx);
                        } else if key == "left" {
                            this.seek_relative(-10_000, cx);
                        } else if key == "right" {
                            this.seek_relative(10_000, cx);
                        }
                    }))
                    .child(self.stage_titlebar(window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(player::render(self, cx)),
                    ),'''
new_stage_children = r'''                    .relative()
                    .overflow_hidden()
                    .bg(rgb(0x0e_0f_16))
                    .text_color(theme::TEXT_WHITE)
                    .on_mouse_move(cx.listener(
                        |this, event: &gpui::MouseMoveEvent, _window, cx| {
                            this.handle_stage_mouse_move(event.position, cx);
                        },
                    ))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            this.wake_stage_controls_immediately(cx);
                        }),
                    )
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.wake_stage_controls_immediately(cx);
                    }))
                    .on_mouse_up(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            if this.drag_target.is_some() {
                                this.commit_drag(cx);
                            }
                        }),
                    )
                    .on_mouse_up_out(
                        gpui::MouseButton::Left,
                        cx.listener(|this, _, _window, cx| {
                            if this.drag_target.is_some() {
                                this.commit_drag(cx);
                            }
                        }),
                    )
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                        this.wake_stage_controls(cx);
                        let key = event.keystroke.key.as_str();
                        if key == "escape" {
                            this.close_stage(cx);
                        } else if key == "space" {
                            this.toggle_play(cx);
                        } else if key == "left" {
                            this.seek_relative(-10_000, cx);
                        } else if key == "right" {
                            this.seek_relative(10_000, cx);
                        }
                    }))
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .overflow_hidden()
                            .child(player::render(self, cx, fluid_background.clone())),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(0.0))
                            .left(px(0.0))
                            .right(px(0.0))
                            .child(self.stage_titlebar(window, cx)),
                    ),'''
replace_once("src/ui/shell.rs", old_stage_children, new_stage_children, "full bleed stage titlebar")

# ---------------------------------------------------------------------------
# Local artwork decode: embedded -> sidecar, and corrupt embedded data must not block sidecar.
# ---------------------------------------------------------------------------
replace_once(
    "src/artwork.rs",
    r'''        let source = embedded_artwork(&track.path).or_else(|| sidecar_artwork(&track.path));
        let Some(source) = source else {
            return Ok(None);
        };
        let image = image::load_from_memory(&source).context("封面格式不受支持或文件已损坏")?;
        let image = image.thumbnail(image.width().min(768), image.height().min(768));''',
    r'''        let Some(image) = decoded_local_artwork(&track.path) else {
            return Ok(None);
        };
        let image = image.thumbnail(image.width().min(768), image.height().min(768));''',
    "local artwork decode chain",
)
replace_once(
    "src/artwork.rs",
    "fn embedded_artwork(path: &Path) -> Option<Vec<u8>> {",
    r'''fn decoded_local_artwork(path: &Path) -> Option<DynamicImage> {
    embedded_artwork(path)
        .and_then(|bytes| image::load_from_memory(&bytes).ok())
        .or_else(|| {
            sidecar_artwork(path).and_then(|bytes| image::load_from_memory(&bytes).ok())
        })
}

fn embedded_artwork(path: &Path) -> Option<Vec<u8>> {''',
    "decoded local artwork helper",
)

# ---------------------------------------------------------------------------
# Enrichment: local artwork wins; a local miss can request one online fallback even if an older
# checked_online record would normally suppress the provider request.
# ---------------------------------------------------------------------------
replace_once(
    "src/ui/enrichment.rs",
    r'''        if self.enrichment_done.contains(&track.id) || !self.enrichment_loading.insert(track.id) {
            return;
        }''',
    r'''        let pending_artwork_fallback = self.artwork_online_fallback_requested.contains(&track.id);
        if (self.enrichment_done.contains(&track.id) && !pending_artwork_fallback)
            || !self.enrichment_loading.insert(track.id)
        {
            return;
        }
        let needs_artwork_fallback = self.artwork_online_fallback_requested.remove(&track.id);''',
    "enrichment fallback guard",
)
replace_once(
    "src/ui/enrichment.rs",
    "            if stored.checked_online && !needs_translation_upgrade {",
    "            if stored.checked_online && !needs_translation_upgrade && !needs_artwork_fallback {",
    "checked online artwork fallback",
)
replace_once(
    "src/ui/enrichment.rs",
    r'''                        if let Some(artwork) = outcome.artwork {
                            this.set_artwork_parts(
                                track_id,
                                artwork.png,
                                artwork.blurred_png,
                                artwork.palette,
                            );
                            this.artwork_missing.remove(&track_id);
                        }''',
    r'''                        if let Some(artwork) = outcome.artwork {
                            // Local embedded/sidecar artwork has priority. Online artwork is only
                            // installed if local loading has not produced a usable cover.
                            if !this.artworks.contains_key(&track_id)
                                || this.artwork_missing.contains(&track_id)
                            {
                                this.set_artwork_parts(
                                    track_id,
                                    artwork.png,
                                    artwork.blurred_png,
                                    artwork.palette,
                                );
                                this.artwork_missing.remove(&track_id);
                            }
                        }''',
    "local artwork priority",
)
# If local failure arrived while the initial enrichment request was in flight, consume the queued
# fallback after that request completes. The fallback flag is removed when the retry starts, so a
# provider miss cannot create an infinite retry loop.
replace_once(
    "src/ui/enrichment.rs",
    "                cx.notify();\n            })?;",
    r'''                if this.artwork_online_fallback_requested.contains(&track_id)
                    && !this.artworks.contains_key(&track_id)
                    && !this.enrichment_loading.contains(&track_id)
                {
                    this.enrichment_done.remove(&track_id);
                    this.request_current_enrichment(cx);
                }
                cx.notify();
            })?;''',
    "queued artwork fallback retry",
)

print("stage input, artwork fallback and FBM patch applied")
