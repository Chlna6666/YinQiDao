use std::{sync::{Arc, OnceLock}, time::{Duration, Instant}};

use gpui::{
    Context, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString,
    StatefulInteractiveElement as _, div, hsla, img, linear_color_stop, linear_gradient,
    prelude::*, px, relative, rgb,
};
use lucide_gpui::icons as lucide_icons;

use crate::{
    artwork::ArtworkPalette,
    lyrics::LyricLine,
    model::{PlaybackState, PlayerSnapshot, Track},
};

use super::{
    components::{SliderStyle, smooth_slider},
    player_legacy,
    shell::{DragTarget, MusicApp},
    theme::{ACCENT_RED, TEXT_WHITE, elegant_gradient_for, format_remaining_time, format_time, themed_icon},
};

pub(super) use player_legacy::{NowPlaying, PlaybackProgress, PlaybackTime, mini_player};

const STATE_PITCH: f32 = 60.0;
const ROW_PITCH: f32 = 68.0;
const WINDOW_RADIUS: usize = 7;

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let snapshot = &app.snapshot;
    let track = snapshot.current_track.as_ref();
    let id = track.map(|track| track.id);
    let artwork = id.and_then(|id| app.artworks.get(&id).cloned());
    let blurred = id.and_then(|id| app.blurred_artworks.get(&id).cloned());
    let palette = id.and_then(|id| app.artwork_palettes.get(&id));
    let lyrics = id
        .and_then(|id| app.lyrics.get(&id))
        .map_or(&[][..], |document| document.timed_lines());

    div()
        .id("stage-player-root")
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(rgb(0x0e_0f_16))
        .text_color(TEXT_WHITE)
        .child(ambient_background(
            id,
            artwork.clone(),
            blurred,
            palette,
            app.config.dynamic_blur,
            app.config.blur_radius,
            snapshot.state == PlaybackState::Playing,
        ))
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .p_8()
                .gap_6()
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h(px(0.0))
                        .gap_12()
                        .items_center()
                        .child(stage_cover(track, artwork))
                        .child(stage_lyrics(app, lyrics, snapshot.position_ms, cx)),
                )
                .child(stage_controls(app, snapshot, cx)),
        )
        .into_any_element()
}

fn stage_cover(track: Option<&Track>, artwork: Option<Arc<[u8]>>) -> impl IntoElement {
    let title = track.map_or("未在播放音乐", |track| track.title.as_str());
    let artist = track.map_or("请选择音乐", |track| track.artist.as_str());
    let album = track.map_or("未知专辑", |track| track.album.as_str());
    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size_full()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track.map_or(0, |track| track.id));
        div()
            .size_full()
            .bg(linear_gradient(135.0, linear_color_stop(c1, 0.0), linear_color_stop(c2, 1.0)))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(lucide_icons::icon_disc_3(), 96.0, hsla(0.0, 0.0, 1.0, 0.7)))
            .into_any_element()
    };

    div()
        .w(px(380.0))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_6()
        .child(
            div()
                .size(px(280.0))
                .rounded_2xl()
                .overflow_hidden()
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.15))
                .shadow_lg()
                .child(cover),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(div().text_2xl().font_weight(gpui::FontWeight::BOLD).child(title.to_owned()))
                .child(div().text_base().text_color(hsla(0.0, 0.0, 1.0, 0.72)).child(artist.to_owned()))
                .child(div().text_sm().text_color(hsla(0.0, 0.0, 1.0, 0.42)).child(album.to_owned())),
        )
}

fn stage_lyrics(
    app: &MusicApp,
    lyrics: &[LyricLine],
    position_ms: u64,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    if lyrics.is_empty() {
        return div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(themed_icon(lucide_icons::icon_music(), 36.0, hsla(0.0, 0.0, 1.0, 0.25)))
            .child(div().text_lg().text_color(hsla(0.0, 0.0, 1.0, 0.50)).child("暂无同步滚动歌词"))
            .child(div().text_sm().text_color(hsla(0.0, 0.0, 1.0, 0.30)).child("支持内嵌 LRC 或联网自动检索"))
            .into_any_element();
    }

    let active = lyrics.iter().rposition(|line| line.timestamp_ms <= position_ms).unwrap_or(0);
    let raw = app.lyrics_current_offset / STATE_PITCH;
    let target = app.lyrics_target_offset / STATE_PITCH;
    let center = elastic_center(raw, target);
    let anchor = center.round().clamp(0.0, lyrics.len().saturating_sub(1) as f32) as usize;
    let start = anchor.saturating_sub(WINDOW_RADIUS);
    let end = (anchor + WINDOW_RADIUS + 1).min(lyrics.len());

    let mut layer = div()
        .id("stage-lyrics-window")
        .absolute()
        .left(relative(0.0))
        .right(relative(0.0))
        .top(relative(0.5));

    for (index, line) in lyrics.iter().enumerate().take(end).skip(start) {
        let distance = (index as f32 - center).abs();
        let focus = (-1.05 * distance).exp().clamp(0.0, 1.0);
        let alpha = 0.12 + focus * 0.88;
        let size = 17.0 + focus * 13.0;
        let scale = 0.96 + focus * 0.06 + (std::f32::consts::PI * focus).sin() * 0.018;
        let y = (index as f32 - center) * ROW_PITCH - 24.0;
        let timestamp = line.timestamp_ms;
        let weight = if focus > 0.72 {
            gpui::FontWeight::BOLD
        } else if focus > 0.32 {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::MEDIUM
        };

        let mut text = div().flex().flex_col().gap_1().font_weight(weight);
        if let Some((primary, translation)) = line.text.split_once('\n') {
            text = text
                .child(div().text_size(px(size)).text_color(hsla(0.0, 0.0, 1.0, alpha)).child(primary.to_owned()))
                .child(div().text_size(px(size * 0.72)).text_color(hsla(0.0, 0.0, 1.0, alpha * 0.66)).child(translation.to_owned()));
        } else {
            text = text.child(div().text_size(px(size)).text_color(hsla(0.0, 0.0, 1.0, alpha)).child(line.text.clone()));
        }

        layer = layer.child(
            div()
                .id(SharedString::from(format!("lyric-line-{index}")))
                .absolute()
                .left(relative(0.0))
                .right(relative(0.0))
                .top(px(y))
                .pl_4()
                .py_1()
                .scale(scale)
                .opacity(if index == active { 1.0 } else { 0.98 })
                .cursor_pointer()
                .hover(|style| style.opacity(1.0).scale(1.02))
                .child(text)
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.seek_to_ms(timestamp, cx);
                    this.lyrics_target_offset = index as f32 * STATE_PITCH;
                    this.lyrics_user_scrolling_until = None;
                    this.wake_stage_controls_immediately(cx);
                })),
        );
    }

    let max_offset = lyrics.len().saturating_sub(1) as f32 * STATE_PITCH;
    div()
        .id("stage-lyrics-viewport")
        .relative()
        .flex_1()
        .h_full()
        .overflow_hidden()
        .on_scroll_wheel(cx.listener(move |this, event: &gpui::ScrollWheelEvent, _, cx| {
            let delta = match event.delta {
                gpui::ScrollDelta::Pixels(pixels) => f32::from(pixels.y),
                gpui::ScrollDelta::Lines(lines) => lines.y * 32.0,
            };
            this.lyrics_current_offset = (this.lyrics_current_offset - delta).clamp(0.0, max_offset);
            this.lyrics_target_offset = this.lyrics_current_offset;
            this.lyrics_user_scrolling_until = Some(Instant::now() + Duration::from_secs(3));
            cx.notify();
        }))
        .child(layer)
        .into_any_element()
}

fn elastic_center(raw: f32, target: f32) -> f32 {
    let delta = target - raw;
    let distance = delta.abs();
    if distance <= 0.001 || distance >= 1.25 {
        return raw;
    }
    let phase = (1.25 - distance) / 1.25;
    raw + ((phase * std::f32::consts::TAU).sin() * 0.085 * phase).copysign(delta)
}

fn stage_controls(app: &MusicApp, snapshot: &PlayerSnapshot, cx: &mut Context<MusicApp>) -> impl IntoElement {
    let position = app.displayed_position_ms();
    let playing = snapshot.state == PlaybackState::Playing;
    let visibility = app.stage_controls_visibility;

    div()
        .id("stage-bottom-dock")
        .top(px((1.0 - visibility) * 56.0))
        .opacity(visibility)
        .flex()
        .items_center()
        .gap_5()
        .px_6()
        .py_3()
        .rounded_2xl()
        .bg(hsla(0.0, 0.0, 0.0, 0.40))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .child(div().text_xs().text_color(hsla(0.0, 0.0, 1.0, 0.68)).child(format_time(position)))
        .child(
            smooth_slider("stage-progress-track", app.displayed_progress_ratio(), SliderStyle::stage_progress())
                .flex_1()
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let ratio = this.stage_progress_ratio(f32::from(event.position.x), window);
                    this.begin_drag(DragTarget::Progress, ratio, cx);
                }))
                .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if (this.seeking || event.dragging()) && this.drag_target == Some(DragTarget::Progress) {
                        let ratio = this.stage_progress_ratio(f32::from(event.position.x), window);
                        this.update_drag_ratio(DragTarget::Progress, ratio, cx);
                    }
                }))
                .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.commit_drag(cx);
                })),
        )
        .child(div().text_xs().text_color(hsla(0.0, 0.0, 1.0, 0.68)).child(format_remaining_time(position, snapshot.duration_ms)))
        .child(control_button("stage-prev-btn", lucide_icons::icon_skip_back(), cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.previous(cx);
        })))
        .child(
            div()
                .id("stage-play-btn")
                .size(px(46.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .bg(ACCENT_RED)
                .active(|style| style.scale(0.95))
                .child(themed_icon(if playing { lucide_icons::icon_pause() } else { lucide_icons::icon_play() }, 22.0, hsla(0.0, 0.0, 1.0, 1.0)))
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_play(cx);
                })),
        )
        .child(control_button("stage-next-btn", lucide_icons::icon_skip_forward(), cx.listener(|this, _, _, cx| {
            cx.stop_propagation();
            this.next(cx);
        })))
}

fn control_button(
    id: &'static str,
    icon: &'static str,
    listener: impl Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(36.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
        .active(|style| style.scale(0.92))
        .child(themed_icon(icon, 20.0, hsla(0.0, 0.0, 1.0, 0.85)))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn clock_secs() -> f32 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f32()
}

fn ambient_background(
    id: Option<i64>,
    artwork: Option<Arc<[u8]>>,
    blurred: Option<Arc<[u8]>>,
    palette: Option<&ArtworkPalette>,
    dynamic: bool,
    blur_radius: f32,
    playing: bool,
) -> impl IntoElement {
    let id = id.unwrap_or_default();
    let motion = if dynamic && playing { 1.0 } else { 0.0 };
    let t = if motion > 0.0 { clock_secs() } else { id.unsigned_abs() as f32 * 0.013 };
    let (c1, c2, c3, dark, mask) = ambient_palette(id, palette);
    let diffusion = (blur_radius / 32.0).clamp(0.4, 1.8);
    let mut root = div().absolute().inset_0().overflow_hidden().bg(dark);

    if let Some(bytes) = blurred.or(artwork) {
        root = root.child(
            div()
                .absolute()
                .inset_0()
                .scale(1.10 + diffusion * 0.025 + (t * 0.083).sin() * 0.018 * motion)
                .opacity(0.36)
                .child(img(EncodedImageBytes::new(ImageFormat::Png, bytes)).size_full().object_fit(ObjectFit::Cover)),
        );
    }

    root
        .child(blob(-0.14 + (t * 0.071).sin() * 0.11 * motion, -0.20 + (t * 0.097).cos() * 0.09 * motion, 0.86, 0.98, 34.0 + t * 7.1 * motion, c1, 0.88))
        .child(blob(0.58 + (t * 0.109).cos() * 0.10 * motion, 0.02 + (t * 0.137).sin() * 0.12 * motion, 0.82, 0.90, 211.0 - t * 5.3 * motion, c2, 0.82))
        .child(blob(0.20 + (t * 0.163).sin() * 0.08 * motion, 0.48 + (t * 0.191).cos() * 0.10 * motion, 0.78, 0.88, 302.0 + t * 3.7 * motion, c3, 0.76))
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.0,
            linear_color_stop(hsla(0.0, 0.0, 0.01, (mask * 0.48).clamp(0.20, 0.46)), 0.0),
            linear_color_stop(hsla(0.0, 0.0, 0.005, (mask * 0.82).clamp(0.38, 0.66)), 1.0),
        )))
}

fn blob(left: f32, top: f32, width: f32, height: f32, angle: f32, color: gpui::Hsla, alpha: f32) -> gpui::Div {
    div()
        .absolute()
        .left(relative(left))
        .top(relative(top))
        .w(relative(width))
        .h(relative(height))
        .rounded_full()
        .bg(linear_gradient(angle, linear_color_stop(color.opacity(alpha), 0.0), linear_color_stop(color.opacity(0.0), 1.0)))
}

fn ambient_palette(id: i64, palette: Option<&ArtworkPalette>) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla, gpui::Hsla, f32) {
    if let Some(palette) = palette {
        let mixed = [
            ((palette.dominant_rgb[0] as u16 + palette.secondary_rgb[2] as u16) / 2) as u8,
            ((palette.dominant_rgb[1] as u16 + palette.secondary_rgb[0] as u16) / 2) as u8,
            ((palette.dominant_rgb[2] as u16 + palette.secondary_rgb[1] as u16) / 2) as u8,
        ];
        let dark = ((palette.dark_ambient_rgb[0] as u32) << 16) | ((palette.dark_ambient_rgb[1] as u32) << 8) | palette.dark_ambient_rgb[2] as u32;
        return (rgb_to_hsla(palette.dominant_rgb), rgb_to_hsla(palette.secondary_rgb), rgb_to_hsla(mixed), rgb(dark).into(), palette.mask_alpha);
    }
    let hue = ((id.unsigned_abs() * 47) % 360) as f32 / 360.0;
    (hsla(hue, 0.82, 0.56, 1.0), hsla((hue + 0.22) % 1.0, 0.80, 0.50, 1.0), hsla((hue + 0.47) % 1.0, 0.76, 0.54, 1.0), rgb(0x10_11_1a).into(), 0.60)
}

fn rgb_to_hsla(value: [u8; 3]) -> gpui::Hsla {
    let r = value[0] as f32 / 255.0;
    let g = value[1] as f32 / 255.0;
    let b = value[2] as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-4 { return hsla(0.0, 0.0, l, 1.0); }
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if (max - r).abs() < 1e-4 {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if (max - g).abs() < 1e-4 {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    hsla(h, (s * 1.28).clamp(0.30, 0.96), l.clamp(0.24, 0.68), 1.0)
}
