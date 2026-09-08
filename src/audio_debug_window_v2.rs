use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, AppContext, BorrowAppContext, Bounds, Context, Global, Hsla, IntoElement, PathBuilder,
    Render, Timer, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, fill, point,
    prelude::*, px, rgb, size,
};
use yinqidao_audio_spatial::{
    SpatialDebugReflectionWall, SpatialDebugSnapshot, SpatialDebugSourceKind,
};

use crate::audio::{
    AudioDebugMonitorMode, AudioDebugSnapshot, AudioDebugStage, audio_debug_latest_snapshot,
    set_audio_debug_enabled, set_audio_debug_monitor_mode, spatial_debug_latest_snapshot,
};
use crate::audio_spatial_debug_3d::{SpatialDebug3dCamera, SpatialDebug3dScene};

const DEBUG_UI_TICK: Duration = Duration::from_millis(33);
const SOURCE_ROWS: usize = 12;
const REFLECTION_ROWS: usize = 16;

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
    let bounds = Bounds::centered(None, size(px(1_420.0), px(940.0)), cx);
    let window = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(1_080.0), px(720.0))),
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
            let audio = audio_debug_latest_snapshot();
            let spatial = spatial_debug_latest_snapshot();
            let still_open = cx.update(|cx| {
                let result = window.update(cx, |view, window, view_cx| {
                    if view.frozen {
                        return;
                    }
                    let audio_changed = audio.sequence != view.snapshot.sequence;
                    let spatial_changed = spatial.as_ref().map(|snapshot| snapshot.sequence)
                        != view.spatial_snapshot.as_ref().map(|snapshot| snapshot.sequence);
                    if audio_changed || spatial_changed {
                        if spatial_changed {
                            view.gpu_scene.update(spatial);
                            view.spatial_snapshot = spatial;
                        }
                        if audio_changed {
                            view.snapshot = audio;
                        }
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

pub(crate) struct AudioDebugView {
    snapshot: AudioDebugSnapshot,
    spatial_snapshot: Option<SpatialDebugSnapshot>,
    gpu_scene: SpatialDebug3dScene,
    camera: SpatialDebug3dCamera,
    frozen: bool,
}

impl Default for AudioDebugView {
    fn default() -> Self {
        Self {
            snapshot: AudioDebugSnapshot::default(),
            spatial_snapshot: None,
            gpu_scene: SpatialDebug3dScene::default(),
            camera: SpatialDebug3dCamera::default(),
            frozen: false,
        }
    }
}

impl Render for AudioDebugView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot.clone();
        let spatial = self.spatial_snapshot;
        let mesh = self.gpu_scene.mesh();
        let mesh_error = self.gpu_scene.error();
        let camera = self.camera;
        let draw_parameters = self.gpu_scene.draw_parameters(1.56, camera);
        let frozen = self.frozen;

        div()
            .id("audio-debug-v2-root")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(0x090b0f))
            .text_color(rgb(0xecf1f6))
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
                                    .child("Audio Laboratory · Spatial Engine GPU Debug"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x85909d))
                                    .child("A/B/C 信号参考 + GPUI Custom Mesh 3D + image-source reflection geometry"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(monitor_button(
                                "A SOURCE",
                                snapshot.monitor_mode == AudioDebugMonitorMode::Source,
                            )
                            .on_click(cx.listener(|_, _, _, _| {
                                set_audio_debug_monitor_mode(AudioDebugMonitorMode::Source);
                            })))
                            .child(monitor_button(
                                "B POST-EQ",
                                snapshot.monitor_mode == AudioDebugMonitorMode::PostEq,
                            )
                            .on_click(cx.listener(|_, _, _, _| {
                                set_audio_debug_monitor_mode(AudioDebugMonitorMode::PostEq);
                            })))
                            .child(monitor_button(
                                "C SPATIAL",
                                snapshot.monitor_mode == AudioDebugMonitorMode::PostSpatial,
                            )
                            .on_click(cx.listener(|_, _, _, _| {
                                set_audio_debug_monitor_mode(AudioDebugMonitorMode::PostSpatial);
                            })))
                            .child(
                                action_button(if frozen { "继续" } else { "冻结" }).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.frozen = !this.frozen;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(stage_card("A · ORIGINAL", "decoder/source reference", &snapshot.source, rgb(0x8fa3ba)))
                    .child(stage_card("B · POST-EQ", "PEQ + preamp", &snapshot.eq, rgb(0xffa63d)))
                    .child(stage_card("C · POST-SPATIAL", "virtual source + room", &snapshot.spatial, rgb(0x56d38f))),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_stretch()
                    .child(
                        panel(
                            "Spatial Room · GPU 3D",
                            Some(spatial.map_or_else(
                                || "等待 SpatialEngine scene".to_string(),
                                |scene| format!(
                                    "{} source · {} reflection · {} Hz · sequence {}",
                                    scene.source_count,
                                    scene.reflection_count,
                                    scene.sample_rate,
                                    scene.sequence,
                                ),
                            )),
                            div()
                                .relative()
                                .w_full()
                                .h(px(510.0))
                                .overflow_hidden()
                                .rounded_lg()
                                .bg(rgb(0x070a0e))
                                .child(match mesh {
                                    Some(mesh) => canvas(
                                        move |bounds, _window, _cx| bounds,
                                        move |bounds, _prepaint, window, _cx| {
                                            window.paint_gpu_mesh_3d(bounds, mesh.clone(), draw_parameters);
                                        },
                                    )
                                    .absolute()
                                    .inset_0()
                                    .into_any_element(),
                                    None => div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_sm()
                                        .text_color(rgb(0x74808d))
                                        .child(mesh_error.map_or_else(
                                            || "播放空间音频后显示 3D 场景".to_string(),
                                            |error| format!("3D shader/mesh: {error}"),
                                        ))
                                        .into_any_element(),
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .top_2()
                                        .left_2()
                                        .flex()
                                        .gap_1()
                                        .child(action_button("↶").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.orbit(-0.12, 0.0);
                                            cx.notify();
                                        })))
                                        .child(action_button("↷").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.orbit(0.12, 0.0);
                                            cx.notify();
                                        })))
                                        .child(action_button("↑").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.orbit(0.0, -0.09);
                                            cx.notify();
                                        })))
                                        .child(action_button("↓").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.orbit(0.0, 0.09);
                                            cx.notify();
                                        })))
                                        .child(action_button("+").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.zoom_by(1.12);
                                            cx.notify();
                                        })))
                                        .child(action_button("−").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.zoom_by(0.89);
                                            cx.notify();
                                        })))
                                        .child(action_button("Reset").on_click(cx.listener(|this, _, _, cx| {
                                            this.camera.reset();
                                            cx.notify();
                                        }))),
                                ),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        panel(
                            "Spatial Telemetry",
                            Some("authored channel / ITD / ILD / distance / external cues".into()),
                            spatial_telemetry(spatial),
                        )
                        .w(px(450.0)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        panel(
                            "Original ↔ Spatial Waveform",
                            Some("灰：原版 · 绿：空间处理后".into()),
                            chart(190.0, waveform_compare_canvas(snapshot.clone())),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        panel(
                            "Image-source Reflection Matrix",
                            Some("source → wall bounce → listener · 实际 excess delay / binaural arrival".into()),
                            reflection_telemetry(spatial),
                        )
                        .w(px(560.0)),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x727d89))
                    .child("3D 场景使用 BMCBL GPUI GpuMesh3d/WGSL/depth 正式管线；空间定位仍为 YinQiDao 自研参数化双耳模型，不宣称 measured HRTF。"),
            )
    }
}

fn monitor_button(label: &'static str, active: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_3()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .border_1()
        .border_color(if active { rgb(0x4b8f70) } else { rgb(0x2a3038) })
        .bg(if active { rgb(0x173729) } else { rgb(0x15191f) })
        .text_xs()
        .child(label)
}

fn action_button(label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_2()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .border_1()
        .border_color(rgb(0x303843))
        .bg(rgb(0x141920))
        .text_xs()
        .child(label)
}

fn panel(title: &'static str, subtitle: Option<String>, content: impl IntoElement) -> gpui::Div {
    let mut header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_sm().font_weight(gpui::FontWeight::BOLD).child(title));
    if let Some(subtitle) = subtitle {
        header = header.child(div().text_xs().text_color(rgb(0x77828f)).child(subtitle));
    }
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(rgb(0x252b33))
        .bg(rgb(0x101319))
        .child(header)
        .child(content)
}

fn chart(height: f32, content: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .w_full()
        .h(px(height))
        .overflow_hidden()
        .rounded_lg()
        .bg(rgb(0x090c10))
        .child(content)
}

fn stage_card(title: &'static str, subtitle: &'static str, stage: &AudioDebugStage, accent: Hsla) -> gpui::Div {
    div()
        .flex_1()
        .min_w(px(0.0))
        .p_3()
        .rounded_xl()
        .border_1()
        .border_color(rgb(0x252b33))
        .bg(rgb(0x101319))
        .flex()
        .flex_col()
        .gap_2()
        .child(div().text_sm().font_weight(gpui::FontWeight::BOLD).text_color(accent).child(title))
        .child(div().text_xs().text_color(rgb(0x75808c)).child(subtitle))
        .child(
            div()
                .flex()
                .gap_2()
                .flex_wrap()
                .child(metric("Peak", format!("{:.1} dBFS", stage.peak_dbfs)))
                .child(metric("RMS", format!("{:.1} dBFS", stage.rms_dbfs)))
                .child(metric("LUFS-I", format!("{:.1}", stage.lufs_integrated)))
                .child(metric("Corr", format!("{:+.3}", stage.stereo_correlation)))
                .child(metric("S/M", format!("{:+.1} dB", stage.side_mid_db)))
                .child(metric("DR", format!("{:.1} dB", stage.dynamic_range_db))),
        )
}

fn metric(label: &'static str, value: String) -> gpui::Div {
    div()
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(0x171c23))
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_xs().text_color(rgb(0x697481)).child(label))
        .child(div().text_xs().child(value))
}

fn spatial_telemetry(snapshot: Option<SpatialDebugSnapshot>) -> gpui::AnyElement {
    let mut body = div().flex().flex_col().gap_2();
    let Some(snapshot) = snapshot else {
        return body.child(div().text_sm().text_color(rgb(0x77828f)).child("等待 scene"))
            .into_any_element();
    };
    body = body.child(
        div()
            .flex()
            .gap_2()
            .child(metric("Room", format!("{:.0}%", snapshot.environment.room_size * 100.0)))
            .child(metric("Wet", format!("{:.0}%", snapshot.environment.mix * 100.0)))
            .child(metric("Damp", format!("{:.0}%", snapshot.environment.damping * 100.0))),
    );
    let visible = snapshot.source_count.min(SOURCE_ROWS);
    for (index, source) in snapshot.sources[..visible].iter().copied().enumerate() {
        if !source.active {
            continue;
        }
        let channel = channel_name(snapshot.source_count, index);
        let kind = if matches!(source.kind, SpatialDebugSourceKind::Lfe) { "LFE" } else { "FULL" };
        body = body.child(
            div()
                .py_1()
                .border_b_1()
                .border_color(rgb(0x20252c))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .child(div().text_xs().font_weight(gpui::FontWeight::BOLD).child(format!("#{:02} {channel} · {kind}", source.source_index)))
                        .child(div().text_xs().text_color(rgb(0x9aa4af)).child(format!("az {:+.1}° / el {:+.1}° / {:.2}m", source.azimuth_degrees, source.elevation_degrees, source.distance_meters))),
                )
                .child(div().text_xs().text_color(rgb(0x77828f)).child(format!("ITD {:.2}smp · ILD {:+.2}dB · L/R {:.3}/{:.3}", source.itd_samples, source.ild_db, source.left_gain, source.right_gain)))
                .child(div().text_xs().text_color(rgb(0x68737f)).child(format!("near {:.0}% · shadow {:.0}% · air {:.0}%", source.near_field_amount * 100.0, source.head_shadow_amount * 100.0, source.air_absorption_amount * 100.0))),
        );
    }
    body.into_any_element()
}

fn reflection_telemetry(snapshot: Option<SpatialDebugSnapshot>) -> gpui::AnyElement {
    let mut body = div().flex().flex_col().gap_1();
    let Some(snapshot) = snapshot else {
        return body.child(div().text_sm().text_color(rgb(0x77828f)).child("等待 reflection matrix"))
            .into_any_element();
    };
    let mut shown = 0usize;
    for reflection in snapshot.reflections[..snapshot.reflection_count.min(snapshot.reflections.len())]
        .iter()
        .copied()
    {
        if !reflection.active {
            continue;
        }
        if shown == REFLECTION_ROWS {
            break;
        }
        shown += 1;
        let source = usize::from(reflection.source_index);
        let channel = channel_name(snapshot.source_count, source);
        body = body.child(
            div()
                .py_1()
                .border_b_1()
                .border_color(rgb(0x20252c))
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(div().text_xs().font_weight(gpui::FontWeight::BOLD).text_color(rgb(0xffc857)).child(format!("{channel} · {} · tap {}", wall_label(reflection.wall), reflection.tap_index)))
                        .child(div().text_xs().text_color(rgb(0x9aa4af)).child(format!("{:.2} ms", reflection.delay_milliseconds))),
                )
                .child(div().text_xs().text_color(rgb(0x77828f)).child(format!("path {:.2}m (+{:.2}m) · wet {:.3} · reflect {:.3}", reflection.path_length_meters, reflection.excess_path_meters, reflection.wet_contribution, reflection.wall_reflectance)))
                .child(div().text_xs().text_color(rgb(0x68737f)).child(format!("arrival az {:+.1}° / el {:+.1}° · L/R delay {:.2}/{:.2} · gain {:.3}/{:.3}", reflection.arrival_azimuth_degrees, reflection.arrival_elevation_degrees, reflection.left_delay_samples, reflection.right_delay_samples, reflection.left_gain, reflection.right_gain))),
        );
    }
    if shown == 0 {
        body = body.child(div().text_sm().text_color(rgb(0x77828f)).child("当前没有 active reflection"));
    }
    body.into_any_element()
}

fn waveform_compare_canvas(snapshot: AudioDebugSnapshot) -> impl IntoElement {
    canvas(
        move |bounds, _window, _cx| bounds,
        move |bounds, _prepaint, window, _cx| {
            window.paint_quad(fill(bounds, rgb(0x090c10)));
            let left = bounds.left();
            let top = bounds.top();
            let width = bounds.size.width;
            let height = bounds.size.height;
            let center = top + height * 0.5;
            let source = &snapshot.source.waveform_left;
            let spatial = &snapshot.spatial.waveform_left;
            draw_waveform(window, source, left, center, width, height, rgb(0x718092));
            draw_waveform(window, spatial, left, center, width, height, rgb(0x56d38f));
        },
    )
    .absolute()
    .inset_0()
}

fn draw_waveform(
    window: &mut Window,
    samples: &[f32],
    left: gpui::Pixels,
    center: gpui::Pixels,
    width: gpui::Pixels,
    height: gpui::Pixels,
    color: Hsla,
) {
    if samples.len() < 2 {
        return;
    }
    let mut path = PathBuilder::stroke(px(1.1));
    let denominator = (samples.len() - 1) as f32;
    for (index, sample) in samples.iter().copied().enumerate() {
        let x = left + width * (index as f32 / denominator);
        let y = center - height * 0.43 * sample.clamp(-1.0, 1.0);
        if index == 0 {
            path.move_to(point(x, y));
        } else {
            path.line_to(point(x, y));
        }
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

fn channel_name(source_count: usize, index: usize) -> &'static str {
    const STEREO: [&str; 2] = ["L", "R"];
    const SURROUND_5_1_4: [&str; 10] = ["FL", "FR", "C", "LFE", "SL", "SR", "TFL", "TFR", "TRL", "TRR"];
    const SURROUND_7_1_4: [&str; 12] = ["FL", "FR", "C", "LFE", "RL", "RR", "SL", "SR", "TFL", "TFR", "TRL", "TRR"];
    match source_count {
        2 => STEREO.get(index).copied().unwrap_or("SRC"),
        10 => SURROUND_5_1_4.get(index).copied().unwrap_or("SRC"),
        12 => SURROUND_7_1_4.get(index).copied().unwrap_or("SRC"),
        _ => "SRC",
    }
}

fn wall_label(wall: SpatialDebugReflectionWall) -> &'static str {
    match wall {
        SpatialDebugReflectionWall::Left => "LEFT",
        SpatialDebugReflectionWall::Right => "RIGHT",
        SpatialDebugReflectionWall::Front => "FRONT",
        SpatialDebugReflectionWall::Rear => "REAR",
    }
}
