use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, AppContext, BorrowAppContext, Bounds, Context, Global, Hsla, IntoElement, PathBuilder,
    Render, Timer, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, fill, hsla,
    point, prelude::*, px, rgb, size,
};

use crate::audio::{
    AudioDebugMonitorMode, AudioDebugSnapshot, AudioDebugStage, audio_debug_latest_snapshot,
    set_audio_debug_enabled, set_audio_debug_monitor_mode,
};

const DEBUG_UI_TICK: Duration = Duration::from_millis(33);
const DB_FLOOR: f32 = -120.0;

#[derive(Default)]
struct AudioDebugWindowState {
    window: Option<WindowHandle<AudioDebugView>>,
}

impl Global for AudioDebugWindowState {}

pub(crate) fn open(cx: &mut App) -> Result<()> {
    ensure_window_state(cx);

    if let Some(existing) = cx
        .try_global::<AudioDebugWindowState>()
        .and_then(|state| state.window.clone())
    {
        if existing
            .update(cx, |_view, window, _cx| window.show_window())
            .is_ok()
        {
            set_audio_debug_enabled(true);
            return Ok(());
        }
        cx.update_global(|state: &mut AudioDebugWindowState, _cx| state.window = None);
    }

    remove_untracked_windows(cx);
    set_audio_debug_enabled(true);

    let bounds = Bounds::centered(None, size(px(1_260.0), px(860.0)), cx);
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(960.0), px(660.0))),
            is_resizable: true,
            is_minimizable: true,
            is_movable: true,
            ..Default::default()
        },
        |_, cx| cx.new(|_| AudioDebugView::default()),
    )?;

    cx.update_global(|state: &mut AudioDebugWindowState, _cx| {
        state.window = Some(window.clone());
    });
    start_debug_ui_service(window, cx);
    Ok(())
}

pub(crate) fn shutdown(cx: &mut App) {
    ensure_window_state(cx);
    set_audio_debug_enabled(false);
    let tracked = cx.update_global(|state: &mut AudioDebugWindowState, _cx| state.window.take());
    if let Some(window) = tracked {
        let _ = window.update(cx, |_view, window, _cx| window.remove_window());
    }
    remove_untracked_windows(cx);
}

fn ensure_window_state(cx: &mut App) {
    if !cx.has_global::<AudioDebugWindowState>() {
        cx.set_global(AudioDebugWindowState::default());
    }
}

fn remove_untracked_windows(cx: &mut App) {
    let windows: Vec<_> = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<AudioDebugView>())
        .collect();
    for window in windows {
        let _ = window.update(cx, |_view, window, _cx| window.remove_window());
    }
}

fn start_debug_ui_service(window: WindowHandle<AudioDebugView>, cx: &mut App) {
    cx.spawn(async move |cx| -> anyhow::Result<()> {
        loop {
            Timer::after(DEBUG_UI_TICK).await;
            let snapshot = audio_debug_latest_snapshot();
            let still_open = cx.update(|cx| {
                let result = window.update(cx, |view, window, view_cx| {
                    if !view.frozen && snapshot.sequence != view.snapshot.sequence {
                        view.snapshot = snapshot;
                        view_cx.notify();
                        window.refresh();
                    }
                });
                if result.is_err() {
                    if cx.has_global::<AudioDebugWindowState>() {
                        cx.update_global(|state: &mut AudioDebugWindowState, _cx| {
                            state.window = None;
                        });
                    }
                    set_audio_debug_enabled(false);
                    return false;
                }
                true
            })?;
            if !still_open {
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
        let source_color = color(0x8f_a3_ba);
        let eq_color = color(0xff_a6_3d);
        let spatial_color = color(0x56_d3_8f);

        div()
            .id("audio-debug-root")
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
                                    .child("Audio Laboratory · SOURCE / EQ / SPATIAL"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x9b_a4_b0))
                                    .child(format!(
                                        "实时 DSP 探针 · {} Hz · frame #{} · 监听 {}",
                                        snapshot.sample_rate,
                                        snapshot.sequence,
                                        snapshot.monitor_mode.label()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(monitor_button(
                                AudioDebugMonitorMode::Source,
                                snapshot.monitor_mode,
                            ))
                            .child(monitor_button(
                                AudioDebugMonitorMode::PostEq,
                                snapshot.monitor_mode,
                            ))
                            .child(monitor_button(
                                AudioDebugMonitorMode::PostSpatial,
                                snapshot.monitor_mode,
                            ))
                            .child(
                                div()
                                    .id("audio-debug-freeze")
                                    .px_3()
                                    .py_2()
                                    .rounded_lg()
                                    .cursor_pointer()
                                    .bg(if frozen {
                                        rgb(0x37_2a_18)
                                    } else {
                                        rgb(0x1a_1e_25)
                                    })
                                    .border_1()
                                    .border_color(rgb(0x32_38_43))
                                    .text_sm()
                                    .child(if frozen { "继续采样" } else { "冻结分析" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.frozen = !this.frozen;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(stage_card(
                        "SOURCE",
                        "解码 / 多声道双耳化 / 重采样",
                        &snapshot.source,
                        source_color,
                    ))
                    .child(stage_card(
                        "POST-EQ",
                        "十段 PEQ + Preamp",
                        &snapshot.eq,
                        eq_color,
                    ))
                    .child(stage_card(
                        "POST-SPATIAL",
                        "宽度 / 反射 / ITD·ILD / 空间化",
                        &snapshot.spatial,
                        spatial_color,
                    )),
            )
            .child(panel(
                "频谱 A/B/C · 20 Hz → Nyquist-safe",
                Some(format!(
                    "ΔRMS EQ {:+.2} dB · Spatial {:+.2} dB",
                    snapshot.eq.rms_dbfs - snapshot.source.rms_dbfs,
                    snapshot.spatial.rms_dbfs - snapshot.eq.rms_dbfs
                )),
                chart(230.0, spectrum_canvas(snapshot.clone())),
            ))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        panel(
                            "Transfer ΔdB · EQ / Spatial",
                            Some("实际频谱能量差，不是预设 EQ 曲线".into()),
                            chart(190.0, transfer_canvas(snapshot.clone())),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        panel(
                            "M/S Spectrum · POST-SPATIAL",
                            Some("Mid / Side 声场能量".into()),
                            chart(190.0, ms_canvas(snapshot.clone())),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        panel(
                            "Phase Correlation History",
                            Some("-1 反相 · 0 宽场 · +1 单声道相关".into()),
                            chart(
                                155.0,
                                history_canvas(
                                    snapshot.phase_history.clone(),
                                    -1.0,
                                    1.0,
                                    color(0x70_d6_ff),
                                ),
                            ),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        panel(
                            "Crest / Dynamic History",
                            Some("瞬态裕量趋势".into()),
                            chart(
                                155.0,
                                history_canvas(
                                    snapshot.crest_history.clone(),
                                    0.0,
                                    30.0,
                                    color(0xff_c8_57),
                                ),
                            ),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    ),
            )
            .child(panel(
                "Spectrogram · POST-SPATIAL",
                Some("时间向下推进 · 48 个对数频带".into()),
                chart(220.0, spectrogram_canvas(snapshot.clone())),
            ))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        panel(
                            "Waveform Overlay · Left",
                            None,
                            chart(180.0, waveform_canvas(snapshot.clone())),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        panel(
                            "Stereo Vectorscope · SOURCE vs SPATIAL",
                            None,
                            chart(180.0, vectorscope_canvas(snapshot)),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x7f_88_94))
                    .child(
                        "分析口径：Peak/RMS/Crest/Correlation/S·M 为实时工程测量；LUFS 使用 K-weighting + 绝对/相对门限积分的工程实现，True Peak 为 4× 插值估计。它适合播放器内 A/B/C 与回归分析，但当前不宣称通过 EBU R128 / ITU-R BS.1770 合规测试。",
                    ),
            )
    }
}

fn monitor_button(mode: AudioDebugMonitorMode, active: AudioDebugMonitorMode) -> impl IntoElement {
    let selected = mode == active;
    div()
        .id(match mode {
            AudioDebugMonitorMode::Source => "audio-monitor-source",
            AudioDebugMonitorMode::PostEq => "audio-monitor-eq",
            AudioDebugMonitorMode::PostSpatial => "audio-monitor-spatial",
        })
        .px_3()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .bg(if selected {
            rgb(0x24_35_2d)
        } else {
            rgb(0x15_18_1e)
        })
        .border_1()
        .border_color(if selected {
            rgb(0x4b_8f_6d)
        } else {
            rgb(0x2b_31_3a)
        })
        .text_xs()
        .font_weight(if selected {
            gpui::FontWeight::BOLD
        } else {
            gpui::FontWeight::MEDIUM
        })
        .child(mode.label())
        .on_click(move |_, _, _cx| set_audio_debug_monitor_mode(mode))
}

fn panel(title: &'static str, subtitle: Option<String>, body: gpui::AnyElement) -> gpui::Div {
    let mut header = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        );
    if let Some(subtitle) = subtitle {
        header = header.child(
            div()
                .text_xs()
                .text_color(rgb(0x82_8c_99))
                .child(subtitle),
        );
    }

    div()
        .p_4()
        .rounded_xl()
        .bg(rgb(0x10_13_18))
        .border_1()
        .border_color(rgb(0x25_2b_34))
        .flex()
        .flex_col()
        .gap_3()
        .child(header)
        .child(body)
}

fn chart(height: f32, body: impl IntoElement) -> gpui::AnyElement {
    // Canvas has no intrinsic size. In a normal flow container it can therefore collapse to a
    // zero-height/zero-width layout node and all plots appear as a single flat line. A one-cell
    // grid gives the Canvas an explicit stretch constraint on both axes without relying on the
    // opaque `impl IntoElement` return type to expose `Styled` methods.
    div()
        .w_full()
        .h(px(height))
        .grid()
        .grid_cols(1)
        .grid_rows(1)
        .overflow_hidden()
        .child(body)
        .into_any_element()
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
        .child(metric_row([
            ("Peak", format_db(stage.peak_dbfs)),
            (
                "True Peak",
                format!("{} dBTP", format_number(stage.true_peak_dbtp)),
            ),
            ("RMS", format_db(stage.rms_dbfs)),
        ]))
        .child(metric_row([
            ("Crest", format!("{:.2} dB", stage.crest_db)),
            ("DR", format!("{:.2} dB", stage.dynamic_range_db)),
            ("Clip", stage.clipped_samples.to_string()),
        ]))
        .child(metric_row([
            ("LUFS-M", format_number(stage.lufs_momentary)),
            ("LUFS-S", format_number(stage.lufs_short_term)),
            ("LUFS-I", format_number(stage.lufs_integrated)),
        ]))
        .child(metric_row([
            ("Corr", format!("{:+.3}", stage.stereo_correlation)),
            ("S/M", format!("{:+.2} dB", stage.side_mid_db)),
            ("", String::new()),
        ]))
}

fn metric_row<const N: usize>(items: [(&'static str, String); N]) -> impl IntoElement {
    let mut row = div().flex().gap_2();
    for (label, value) in items {
        row = row.child(metric(label, value));
    }
    row
}

fn metric(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_xs()
                .text_color(rgb(0x78_82_8f))
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(value),
        )
}

fn spectrum_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_db_grid(window, bounds);
            paint_series(
                window,
                bounds,
                &snapshot.source.spectrum_dbfs,
                color(0x8f_a3_ba),
                px(1.2),
                DB_FLOOR,
                6.0,
            );
            paint_series(
                window,
                bounds,
                &snapshot.eq.spectrum_dbfs,
                color(0xff_a6_3d),
                px(1.5),
                DB_FLOOR,
                6.0,
            );
            paint_series(
                window,
                bounds,
                &snapshot.spatial.spectrum_dbfs,
                color(0x56_d3_8f),
                px(1.8),
                DB_FLOOR,
                6.0,
            );
        },
    )
}

fn transfer_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_zero_line(window, bounds, -18.0, 18.0);
            paint_series(
                window,
                bounds,
                &snapshot.eq_transfer_db,
                color(0xff_a6_3d),
                px(1.5),
                -18.0,
                18.0,
            );
            paint_series(
                window,
                bounds,
                &snapshot.spatial_transfer_db,
                color(0x56_d3_8f),
                px(1.5),
                -18.0,
                18.0,
            );
        },
    )
}

fn ms_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_db_grid(window, bounds);
            paint_series(
                window,
                bounds,
                &snapshot.spatial.mid_spectrum_dbfs,
                color(0x7d_b8_ff),
                px(1.5),
                DB_FLOOR,
                6.0,
            );
            paint_series(
                window,
                bounds,
                &snapshot.spatial.side_spectrum_dbfs,
                color(0xd0_82_ff),
                px(1.5),
                DB_FLOOR,
                6.0,
            );
        },
    )
}

fn history_canvas(values: Vec<f32>, min: f32, max: f32, line_color: Hsla) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_zero_line(window, bounds, min, max);
            paint_series(window, bounds, &values, line_color, px(1.5), min, max);
        },
    )
}

fn spectrogram_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x08_0a_0e)));
            let rows = snapshot.spectrogram.len();
            let bins = snapshot.spectrogram.first().map_or(0, Vec::len);
            if rows == 0 || bins == 0 {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let cell_w = width / bins as f32;
            let cell_h = height / rows as f32;
            let left = f32::from(bounds.left());
            let top = f32::from(bounds.top());

            for (row_index, row) in snapshot.spectrogram.iter().enumerate() {
                for (bin_index, db) in row.iter().copied().enumerate() {
                    let level = ((db.clamp(DB_FLOOR, 0.0) - DB_FLOOR) / -DB_FLOOR)
                        .clamp(0.0, 1.0);
                    let cell_color = hsla(
                        0.66 - level * 0.58,
                        0.72,
                        0.10 + level * 0.52,
                        0.90,
                    );
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(
                                px(left + bin_index as f32 * cell_w),
                                px(top + row_index as f32 * cell_h),
                            ),
                            size: size(px(cell_w + 0.5), px(cell_h + 0.5)),
                        },
                        cell_color,
                    ));
                }
            }
        },
    )
}

fn waveform_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_zero_line(window, bounds, -1.0, 1.0);
            paint_series(
                window,
                bounds,
                &snapshot.source.waveform_left,
                color(0x6f_7f_92),
                px(1.0),
                -1.0,
                1.0,
            );
            paint_series(
                window,
                bounds,
                &snapshot.eq.waveform_left,
                color(0xff_a6_3d),
                px(1.2),
                -1.0,
                1.0,
            );
            paint_series(
                window,
                bounds,
                &snapshot.spatial.waveform_left,
                color(0x56_d3_8f),
                px(1.4),
                -1.0,
                1.0,
            );
        },
    )
}

fn vectorscope_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x0c_0f_14)));
            paint_vectorscope_axes(window, bounds);
            paint_vectorscope(
                window,
                bounds,
                &snapshot.source.waveform_left,
                &snapshot.source.waveform_right,
                color(0x6f_7f_92),
                px(1.0),
            );
            paint_vectorscope(
                window,
                bounds,
                &snapshot.spatial.waveform_left,
                &snapshot.spatial.waveform_right,
                color(0x56_d3_8f),
                px(1.5),
            );
        },
    )
}

fn paint_db_grid(window: &mut Window, bounds: Bounds<gpui::Pixels>) {
    for db in [-96.0_f32, -72.0, -48.0, -24.0, 0.0] {
        paint_horizontal_value(window, bounds, db, DB_FLOOR, 6.0, color(0x24_2a_33));
    }
}

fn paint_zero_line(window: &mut Window, bounds: Bounds<gpui::Pixels>, min: f32, max: f32) {
    if min <= 0.0 && max >= 0.0 {
        paint_horizontal_value(window, bounds, 0.0, min, max, color(0x2a_30_39));
    }
}

fn paint_horizontal_value(
    window: &mut Window,
    bounds: Bounds<gpui::Pixels>,
    value: f32,
    min: f32,
    max: f32,
    line_color: Hsla,
) {
    let span = (max - min).max(f32::EPSILON);
    let normalized = ((value - min) / span).clamp(0.0, 1.0);
    let y = f32::from(bounds.bottom()) - normalized * f32::from(bounds.size.height);
    let mut builder = PathBuilder::stroke(px(1.0));
    builder.move_to(point(bounds.left(), px(y)));
    builder.line_to(point(bounds.right(), px(y)));
    if let Ok(path) = builder.build() {
        window.paint_path(path, line_color);
    }
}

fn paint_series(
    window: &mut Window,
    bounds: Bounds<gpui::Pixels>,
    values: &[f32],
    line_color: Hsla,
    line_width: gpui::Pixels,
    min: f32,
    max: f32,
) {
    if values.len() < 2 {
        return;
    }
    let left = f32::from(bounds.left());
    let top = f32::from(bounds.top());
    let chart_width = f32::from(bounds.size.width);
    let chart_height = f32::from(bounds.size.height);
    let span = (max - min).max(f32::EPSILON);
    let mut builder = PathBuilder::stroke(line_width);
    for (index, value) in values.iter().copied().enumerate() {
        let x = left + index as f32 / (values.len() - 1) as f32 * chart_width;
        let normalized = ((value.clamp(min, max) - min) / span).clamp(0.0, 1.0);
        let y = top + (1.0 - normalized) * chart_height;
        if index == 0 {
            builder.move_to(point(px(x), px(y)));
        } else {
            builder.line_to(point(px(x), px(y)));
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, line_color);
    }
}

fn paint_vectorscope_axes(window: &mut Window, bounds: Bounds<gpui::Pixels>) {
    let center_x = f32::from(bounds.left()) + f32::from(bounds.size.width) * 0.5;
    let center_y = f32::from(bounds.top()) + f32::from(bounds.size.height) * 0.5;
    for (from, to) in [
        (
            point(bounds.left(), px(center_y)),
            point(bounds.right(), px(center_y)),
        ),
        (
            point(px(center_x), bounds.top()),
            point(px(center_x), bounds.bottom()),
        ),
    ] {
        let mut builder = PathBuilder::stroke(px(1.0));
        builder.move_to(from);
        builder.line_to(to);
        if let Ok(path) = builder.build() {
            window.paint_path(path, color(0x29_2f_38));
        }
    }
}

fn paint_vectorscope(
    window: &mut Window,
    bounds: Bounds<gpui::Pixels>,
    left_samples: &[f32],
    right_samples: &[f32],
    line_color: Hsla,
    line_width: gpui::Pixels,
) {
    let count = left_samples.len().min(right_samples.len());
    if count < 2 {
        return;
    }
    let center_x = f32::from(bounds.left()) + f32::from(bounds.size.width) * 0.5;
    let center_y = f32::from(bounds.top()) + f32::from(bounds.size.height) * 0.5;
    let scale = f32::from(bounds.size.width).min(f32::from(bounds.size.height)) * 0.42;
    let mut builder = PathBuilder::stroke(line_width);
    for index in 0..count {
        let left = left_samples[index].clamp(-1.0, 1.0);
        let right = right_samples[index].clamp(-1.0, 1.0);
        let side = (left - right) * 0.5;
        let mid = (left + right) * 0.5;
        let p = point(px(center_x + side * scale), px(center_y - mid * scale));
        if index == 0 {
            builder.move_to(p);
        } else {
            builder.line_to(p);
        }
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, line_color);
    }
}

fn format_db(value: f32) -> String {
    format!("{} dBFS", format_number(value))
}

fn format_number(value: f32) -> String {
    if value.is_finite() && value > DB_FLOOR + 0.5 {
        format!("{value:.2}")
    } else {
        "−∞".into()
    }
}

#[inline]
fn color(value: u32) -> Hsla {
    rgb(value).into()
}
