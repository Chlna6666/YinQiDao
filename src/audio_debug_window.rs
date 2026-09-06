use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, AppContext, Bounds, Context, Hsla, IntoElement, PathBuilder, Render, Timer, Window,
    WindowBounds, WindowHandle, WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
};

use crate::audio::{
    AudioDebugSnapshot, AudioDebugStage, audio_debug_latest_snapshot, set_audio_debug_enabled,
};

const DEBUG_UI_TICK: Duration = Duration::from_millis(33);
const DB_FLOOR: f32 = -96.0;

pub(crate) fn requested() -> bool {
    let mut force = false;
    let mut suppress = false;
    for argument in std::env::args_os() {
        match argument.to_string_lossy().as_ref() {
            "--audio-debug" => force = true,
            "--no-audio-debug" => suppress = true,
            _ => {}
        }
    }
    if suppress {
        return false;
    }
    if force {
        return true;
    }
    match std::env::var("YINQIDAO_AUDIO_DEBUG") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => cfg!(debug_assertions),
    }
}

pub(crate) fn open(cx: &mut App) -> Result<()> {
    set_audio_debug_enabled(true);
    let bounds = Bounds::centered(None, size(px(1_180.0), px(780.0)), cx);
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(900.0), px(620.0))),
            is_resizable: true,
            is_minimizable: true,
            is_movable: true,
            ..Default::default()
        },
        |_, cx| cx.new(|_| AudioDebugView::default()),
    )?;
    start_debug_ui_service(window, cx);
    Ok(())
}

fn start_debug_ui_service(window: WindowHandle<AudioDebugView>, cx: &mut App) {
    cx.spawn(async move |cx| -> anyhow::Result<()> {
        loop {
            Timer::after(DEBUG_UI_TICK).await;
            let snapshot = audio_debug_latest_snapshot();
            let still_open = cx.update(|cx| {
                window
                    .update(cx, |view, window, view_cx| {
                        if !view.frozen && snapshot.sequence != view.snapshot.sequence {
                            view.snapshot = snapshot;
                            view_cx.notify();
                            window.refresh();
                        }
                    })
                    .is_ok()
            })?;
            if !still_open {
                set_audio_debug_enabled(false);
                break;
            }
        }
        Ok(())
    })
    .detach();
}

#[derive(Default)]
pub(crate) struct AudioDebugView {
    snapshot: AudioDebugSnapshot,
    frozen: bool,
}

impl Render for AudioDebugView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot.clone();
        let frozen = self.frozen;
        let source_color = rgb(0x8f_a3_ba);
        let eq_color = rgb(0xff_a6_3d);
        let spatial_color = rgb(0x56_d3_8f);

        div()
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0x0b_0d_11))
            .text_color(rgb(0xee_f1_f5))
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Audio Laboratory · DSP A/B/C"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x9b_a4_b0))
                                    .child(format!(
                                        "实时链路：SOURCE → EQ → SPATIAL · {} Hz · frame #{}",
                                        snapshot.sample_rate, snapshot.sequence
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("audio-debug-freeze")
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(if frozen { rgb(0x37_2a_18) } else { rgb(0x1a_1e_25) })
                            .border_1()
                            .border_color(rgb(0x32_38_43))
                            .text_sm()
                            .child(if frozen { "继续采样" } else { "冻结分析" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.frozen = !this.frozen;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(stage_card("SOURCE", "解码 / 下混 / 重采样基线", &snapshot.source, source_color))
                    .child(stage_card("POST-EQ", "十段 PEQ + Preamp", &snapshot.eq, eq_color))
                    .child(stage_card(
                        "POST-SPATIAL",
                        "宽度 / 反射 / ITD·ILD / 运动空间化",
                        &snapshot.spatial,
                        spatial_color,
                    )),
            )
            .child(
                div()
                    .p_4()
                    .rounded_xl()
                    .bg(rgb(0x10_13_18))
                    .border_1()
                    .border_color(rgb(0x25_2b_34))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("频谱对比 · 20 Hz → Nyquist-safe"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0x8b_94_a0))
                                    .child(format!(
                                        "ΔRMS EQ {:+.2} dB · Spatial {:+.2} dB",
                                        snapshot.eq.rms_dbfs - snapshot.source.rms_dbfs,
                                        snapshot.spatial.rms_dbfs - snapshot.eq.rms_dbfs
                                    )),
                            ),
                    )
                    .child(spectrum_canvas(snapshot.clone()).h(px(220.0))),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .p_4()
                            .rounded_xl()
                            .bg(rgb(0x10_13_18))
                            .border_1()
                            .border_color(rgb(0x25_2b_34))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("波形叠加 · Left channel"),
                            )
                            .child(waveform_canvas(snapshot.clone()).h(px(180.0))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .p_4()
                            .rounded_xl()
                            .bg(rgb(0x10_13_18))
                            .border_1()
                            .border_color(rgb(0x25_2b_34))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("Stereo Vectorscope · SOURCE vs SPATIAL"),
                            )
                            .child(vectorscope_canvas(snapshot).h(px(180.0))),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x7f_88_94))
                    .child(
                        "测量口径：Peak/RMS/Crest 为 dBFS；Correlation 为 L/R 相关系数；Side/Mid 为能量比。当前不伪装成 LUFS：若后续需要响度合规，应单独实现 ITU-R BS.1770 / EBU R128 K-weighting 与门限积分。",
                    ),
            )
    }
}

fn stage_card(
    title: &'static str,
    subtitle: &'static str,
    stage: &AudioDebugStage,
    accent: Hsla,
) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(0.0))
        .p_4()
        .rounded_xl()
        .bg(rgb(0x10_13_18))
        .border_1()
        .border_color(rgb(0x25_2b_34))
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(accent)
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x7f_88_94))
                        .child(subtitle),
                ),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .gap_2()
                .child(metric("Peak", format_db(stage.peak_dbfs)))
                .child(metric("RMS", format_db(stage.rms_dbfs)))
                .child(metric("Crest", format!("{:.2} dB", stage.crest_db))),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .gap_2()
                .child(metric("Corr", format!("{:+.3}", stage.stereo_correlation)))
                .child(metric("S/M", format!("{:+.2} dB", stage.side_mid_db)))
                .child(metric("Clip", stage.clipped_samples.to_string())),
        )
}

fn metric(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_xs().text_color(rgb(0x78_82_8f)).child(label))
        .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(value))
}

fn spectrum_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_db_grid(window, bounds);
            paint_spectrum(window, bounds, &snapshot.source.spectrum_dbfs, rgb(0x8f_a3_ba), px(1.2));
            paint_spectrum(window, bounds, &snapshot.eq.spectrum_dbfs, rgb(0xff_a6_3d), px(1.5));
            paint_spectrum(window, bounds, &snapshot.spatial.spectrum_dbfs, rgb(0x56_d3_8f), px(1.8));
        },
    )
    .w_full()
}

fn waveform_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_horizontal_zero(window, bounds);
            paint_waveform(window, bounds, &snapshot.source.waveform_left, rgb(0x6f_7f_92), px(1.0), 0.5);
            paint_waveform(window, bounds, &snapshot.eq.waveform_left, rgb(0xff_a6_3d), px(1.2), 0.5);
            paint_waveform(window, bounds, &snapshot.spatial.waveform_left, rgb(0x56_d3_8f), px(1.4), 0.5);
        },
    )
    .w_full()
}

fn vectorscope_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_vectorscope_axes(window, bounds);
            paint_vectorscope(window, bounds, &snapshot.source.waveform_left, &snapshot.source.waveform_right, rgb(0x6f_7f_92), px(1.0));
            paint_vectorscope(window, bounds, &snapshot.spatial.waveform_left, &snapshot.spatial.waveform_right, rgb(0x56_d3_8f), px(1.5));
        },
    )
    .w_full()
}

fn paint_db_grid(window: &mut Window, bounds: Bounds<gpui::Pixels>) {
    for db in [-72.0_f32, -48.0, -24.0, 0.0] {
        let ratio = ((db - DB_FLOOR) / -DB_FLOOR).clamp(0.0, 1.0);
        let y = f32::from(bounds.bottom()) - ratio * f32::from(bounds.size.height);
        let mut builder = PathBuilder::stroke(px(1.0));
        builder.move_to(point(bounds.left(), px(y)));
        builder.line_to(point(bounds.right(), px(y)));
        if let Ok(path) = builder.build() {
            window.paint_path(path, rgb(0x24_2a_33));
        }
    }
}

fn paint_spectrum(window: &mut Window, bounds: Bounds<gpui::Pixels>, spectrum: &[f32], color: Hsla, width: gpui::Pixels) {
    if spectrum.len() < 2 {
        return;
    }
    let left = f32::from(bounds.left());
    let top = f32::from(bounds.top());
    let chart_width = f32::from(bounds.size.width);
    let chart_height = f32::from(bounds.size.height);
    let mut builder = PathBuilder::stroke(width);
    for (index, db) in spectrum.iter().copied().enumerate() {
        let x = left + index as f32 / (spectrum.len() - 1) as f32 * chart_width;
        let normalized = ((db.clamp(DB_FLOOR, 6.0) - DB_FLOOR) / (6.0 - DB_FLOOR)).clamp(0.0, 1.0);
        let y = top + (1.0 - normalized) * chart_height;
        if index == 0 {
            builder.move_to(point(px(x), px(y)));
        } else {
            builder.line_to(point(px(x), px(y)));
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_horizontal_zero(window: &mut Window, bounds: Bounds<gpui::Pixels>) {
    let y = f32::from(bounds.top()) + f32::from(bounds.size.height) * 0.5;
    let mut builder = PathBuilder::stroke(px(1.0));
    builder.move_to(point(bounds.left(), px(y)));
    builder.line_to(point(bounds.right(), px(y)));
    if let Ok(path) = builder.build() {
        window.paint_path(path, rgb(0x2a_30_39));
    }
}

fn paint_waveform(window: &mut Window, bounds: Bounds<gpui::Pixels>, samples: &[f32], color: Hsla, width: gpui::Pixels, vertical_center: f32) {
    if samples.len() < 2 {
        return;
    }
    let left = f32::from(bounds.left());
    let top = f32::from(bounds.top());
    let chart_width = f32::from(bounds.size.width);
    let chart_height = f32::from(bounds.size.height);
    let mut builder = PathBuilder::stroke(width);
    for (index, sample) in samples.iter().copied().enumerate() {
        let x = left + index as f32 / (samples.len() - 1) as f32 * chart_width;
        let y = top + (vertical_center - sample.clamp(-1.0, 1.0) * 0.46) * chart_height;
        if index == 0 {
            builder.move_to(point(px(x), px(y)));
        } else {
            builder.line_to(point(px(x), px(y)));
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_vectorscope_axes(window: &mut Window, bounds: Bounds<gpui::Pixels>) {
    let center_x = f32::from(bounds.left()) + f32::from(bounds.size.width) * 0.5;
    let center_y = f32::from(bounds.top()) + f32::from(bounds.size.height) * 0.5;
    for diagonal in [false, true] {
        let mut builder = PathBuilder::stroke(px(1.0));
        if diagonal {
            builder.move_to(point(bounds.left(), bounds.top()));
            builder.line_to(point(bounds.right(), bounds.bottom()));
        } else {
            builder.move_to(point(bounds.left(), bounds.bottom()));
            builder.line_to(point(bounds.right(), bounds.top()));
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, rgb(0x23_29_31));
        }
    }
    let mut vertical = PathBuilder::stroke(px(1.0));
    vertical.move_to(point(px(center_x), bounds.top()));
    vertical.line_to(point(px(center_x), bounds.bottom()));
    if let Ok(path) = vertical.build() {
        window.paint_path(path, rgb(0x1d_22_2a));
    }
    let mut horizontal = PathBuilder::stroke(px(1.0));
    horizontal.move_to(point(bounds.left(), px(center_y)));
    horizontal.line_to(point(bounds.right(), px(center_y)));
    if let Ok(path) = horizontal.build() {
        window.paint_path(path, rgb(0x1d_22_2a));
    }
}

fn paint_vectorscope(window: &mut Window, bounds: Bounds<gpui::Pixels>, left_samples: &[f32], right_samples: &[f32], color: Hsla, width: gpui::Pixels) {
    let count = left_samples.len().min(right_samples.len());
    if count < 2 {
        return;
    }
    let center_x = f32::from(bounds.left()) + f32::from(bounds.size.width) * 0.5;
    let center_y = f32::from(bounds.top()) + f32::from(bounds.size.height) * 0.5;
    let radius = f32::from(bounds.size.width).min(f32::from(bounds.size.height)) * 0.44;
    let mut builder = PathBuilder::stroke(width);
    for index in 0..count {
        let left = left_samples[index].clamp(-1.0, 1.0);
        let right = right_samples[index].clamp(-1.0, 1.0);
        // 45° vectorscope transform: mono energy is vertical, side energy is horizontal.
        let x = center_x + (left - right) * 0.5 * radius;
        let y = center_y - (left + right) * 0.5 * radius;
        if index == 0 {
            builder.move_to(point(px(x), px(y)));
        } else {
            builder.line_to(point(px(x), px(y)));
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn format_db(value: f32) -> String {
    if value <= -119.0 {
        "-∞ dBFS".into()
    } else {
        format!("{value:.2} dBFS")
    }
}
