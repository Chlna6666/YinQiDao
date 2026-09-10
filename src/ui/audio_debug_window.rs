use std::time::Duration;

use anyhow::Result;
use gpui::{
    App, AppContext, BorrowAppContext, Bounds, Context, Global, IntoElement, MouseButton, Render,
    Timer, Window, WindowBounds, WindowHandle, WindowOptions, canvas, div, prelude::*, px, rgb,
    size,
};
use yinqidao_audio_spatial::{
    ChannelLayout, SpeakerLayout, SpatialDebugReflectionWall, SpatialDebugSnapshot,
    SpatialDebugSourceKind, late_field_telemetry, pinna_cue_telemetry,
};

use crate::audio::{
    AudioDebugMonitorMode, AudioDebugSnapshot, HeadTrackingBridge, HeadTrackingEulerPose,
    ManualHeadTrackingProvider, Vec3, audio_debug_latest_snapshot, reset_runtime_listener_pose,
    set_audio_debug_enabled, set_audio_debug_monitor_mode, spatial_debug_latest_snapshot,
};

use super::{
    audio_debug_analysis::{analysis_sections, stage_card as analysis_stage_card},
    audio_spatial_debug_3d::{SpatialDebug3dCamera, SpatialDebug3dScene},
};

const DEBUG_UI_TICK: Duration = Duration::from_millis(33);
const SOURCE_ROWS: usize = 12;
const REFLECTION_ROWS: usize = 24;
const CAMERA_ORBIT_RADIANS_PER_PIXEL: f32 = 0.0075;
const HEAD_TRACK_RADIANS_PER_PIXEL: f32 = 0.0065;

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
    let bounds = Bounds::centered(None, size(px(1_440.0), px(960.0)), cx);
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
    reset_runtime_listener_pose();
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
                    if spatial_changed {
                        view.gpu_scene.update(spatial);
                        view.spatial_snapshot = spatial;
                    }
                    if audio_changed {
                        view.snapshot = audio;
                    }
                    if audio_changed || spatial_changed {
                        view_cx.notify();
                        window.refresh();
                    }
                });
                if result.is_err() {
                    if cx.has_global::<AudioDebugWindowState>() {
                        cx.update_global(|state: &mut AudioDebugWindowState, _cx| state.window = None);
                    }
                    reset_runtime_listener_pose();
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
    drag_anchor: Option<(f32, f32)>,
    head_tracking: HeadTrackingBridge<ManualHeadTrackingProvider>,
    head_tracking_demo: bool,
    head_yaw: f32,
    head_pitch: f32,
    frozen: bool,
}

impl Default for AudioDebugView {
    fn default() -> Self {
        Self {
            snapshot: AudioDebugSnapshot::default(),
            spatial_snapshot: None,
            gpu_scene: SpatialDebug3dScene::default(),
            camera: SpatialDebug3dCamera::default(),
            drag_anchor: None,
            head_tracking: HeadTrackingBridge::new(ManualHeadTrackingProvider::default()),
            head_tracking_demo: false,
            head_yaw: 0.0,
            head_pitch: 0.0,
            frozen: false,
        }
    }
}

impl AudioDebugView {
    fn publish_debug_head_pose(&mut self) {
        self.head_tracking.provider_mut().push_euler(HeadTrackingEulerPose {
            position_meters: Vec3::ZERO,
            yaw_radians: self.head_yaw,
            pitch_radians: self.head_pitch,
            roll_radians: 0.0,
        });
        let _ = self.head_tracking.poll_and_publish();
    }

    fn center_debug_head_pose(&mut self) {
        self.head_yaw = 0.0;
        self.head_pitch = 0.0;
        self.head_tracking.reset();
        if self.head_tracking_demo {
            self.publish_debug_head_pose();
        }
    }
}

impl Render for AudioDebugView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.snapshot.clone();
        let spatial = self.spatial_snapshot;
        let mesh = self.gpu_scene.mesh();
        let mesh_error = self.gpu_scene.error();
        let draw_parameters = self.gpu_scene.draw_parameters(1.58, self.camera);
        let frozen = self.frozen;
        let head_tracking_demo = self.head_tracking_demo;
        let interaction_hint = if head_tracking_demo {
            "HEAD TRACK 开启 · 拖拽=实时听者转头 · 滚轮=Zoom · 双击=听者归中"
        } else {
            "球面网格=直达 Source Field · 彩色 bounce=次级 Room Early · 蓝=左耳路径 · 红=右耳路径 · 拖拽 Orbit · 滚轮 Zoom · 双击 Reset"
        };

        div()
            .id("audio-debug-root")
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
                                    .child("Audio Laboratory · SOURCE / EQ / SPATIAL / GPU 3D"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(rgb(0x85909d))
                                    .child(format!(
                                        "实时 DSP 探针 · {} Hz · frame #{} · 监听 {} · scene {} · bed {}",
                                        snapshot.sample_rate,
                                        snapshot.sequence,
                                        snapshot.monitor_mode.label(),
                                        spatial.map_or(0, |scene| scene.sequence),
                                        spatial.map_or("--", |scene| layout_name(scene.layout)),
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(monitor_button("A SOURCE", snapshot.monitor_mode == AudioDebugMonitorMode::Source)
                                .on_click(cx.listener(|_, _, _, _| set_audio_debug_monitor_mode(AudioDebugMonitorMode::Source))))
                            .child(monitor_button("B POST-EQ", snapshot.monitor_mode == AudioDebugMonitorMode::PostEq)
                                .on_click(cx.listener(|_, _, _, _| set_audio_debug_monitor_mode(AudioDebugMonitorMode::PostEq))))
                            .child(monitor_button("C SPATIAL", snapshot.monitor_mode == AudioDebugMonitorMode::PostSpatial)
                                .on_click(cx.listener(|_, _, _, _| set_audio_debug_monitor_mode(AudioDebugMonitorMode::PostSpatial))))
                            .child(monitor_button("HEAD TRACK", head_tracking_demo).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.head_tracking_demo = !this.head_tracking_demo;
                                    this.drag_anchor = None;
                                    if this.head_tracking_demo {
                                        this.publish_debug_head_pose();
                                    } else {
                                        this.center_debug_head_pose();
                                    }
                                    cx.notify();
                                }),
                            ))
                            .child(action_button("重置视角").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.camera.reset();
                                    this.drag_anchor = None;
                                    cx.notify();
                                }),
                            ))
                            .child(action_button("听者归中").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.center_debug_head_pose();
                                    this.drag_anchor = None;
                                    cx.notify();
                                }),
                            ))
                            .child(action_button(if frozen { "继续采样" } else { "冻结分析" }).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.frozen = !this.frozen;
                                    cx.notify();
                                }),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(analysis_stage_card(
                        "SOURCE",
                        "解码 / 多声道双耳化 / 重采样",
                        &snapshot.source,
                        rgb(0x8fa3ba).into(),
                    ))
                    .child(analysis_stage_card(
                        "POST-EQ",
                        "十段 PEQ + Preamp",
                        &snapshot.eq,
                        rgb(0xffa63d).into(),
                    ))
                    .child(analysis_stage_card(
                        "POST-SPATIAL",
                        "球形 Source Bed / Pinna / 次级 Early / FDN Late",
                        &snapshot.spatial,
                        rgb(0x56d38f).into(),
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_stretch()
                    .child(
                        panel(
                            "Spherical Spatial Field · GPU 3D",
                            Some(spatial.map_or_else(
                                || "等待 SpatialEngine scene".to_string(),
                                |scene| format!(
                                    "{} · {} source · {} early reflection · {} Hz · seq {} · speaker 亮度/大小=实时 RMS+Peak · 独立 L/R ear path",
                                    layout_name(scene.layout),
                                    scene.source_count,
                                    scene.reflection_count,
                                    scene.sample_rate,
                                    scene.sequence,
                                ),
                            )),
                            div()
                                .id("spatial-sphere-3d-interaction")
                                .relative()
                                .w_full()
                                .h(px(540.0))
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
                                            || "播放空间音频后显示球形 3D 声场".to_string(),
                                            |error| format!("3D shader/mesh: {error}"),
                                        ))
                                        .into_any_element(),
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .bottom_2()
                                        .left_2()
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .bg(rgb(0x10151c))
                                        .text_xs()
                                        .text_color(rgb(0x8d98a5))
                                        .child(interaction_hint),
                                )
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, event: &gpui::MouseDownEvent, _window, cx| {
                                        cx.stop_propagation();
                                        if event.click_count >= 2 {
                                            if this.head_tracking_demo {
                                                this.center_debug_head_pose();
                                            } else {
                                                this.camera.reset();
                                            }
                                            this.drag_anchor = None;
                                            cx.notify();
                                            return;
                                        }
                                        this.drag_anchor = Some((
                                            f32::from(event.position.x),
                                            f32::from(event.position.y),
                                        ));
                                    }),
                                )
                                .on_mouse_move(cx.listener(
                                    |this, event: &gpui::MouseMoveEvent, _window, cx| {
                                        if !event.dragging() {
                                            return;
                                        }
                                        let current = (
                                            f32::from(event.position.x),
                                            f32::from(event.position.y),
                                        );
                                        let Some(previous) = this.drag_anchor else {
                                            this.drag_anchor = Some(current);
                                            return;
                                        };
                                        let dx = current.0 - previous.0;
                                        let dy = current.1 - previous.1;
                                        if dx.abs() > f32::EPSILON || dy.abs() > f32::EPSILON {
                                            if this.head_tracking_demo {
                                                this.head_yaw = (this.head_yaw
                                                    - dx * HEAD_TRACK_RADIANS_PER_PIXEL)
                                                    .rem_euclid(std::f32::consts::PI * 2.0);
                                                this.head_pitch = (this.head_pitch
                                                    - dy * HEAD_TRACK_RADIANS_PER_PIXEL)
                                                    .clamp(-1.35, 1.35);
                                                this.publish_debug_head_pose();
                                            } else {
                                                this.camera.orbit(
                                                    -dx * CAMERA_ORBIT_RADIANS_PER_PIXEL,
                                                    dy * CAMERA_ORBIT_RADIANS_PER_PIXEL,
                                                );
                                            }
                                            this.drag_anchor = Some(current);
                                            cx.notify();
                                        }
                                    },
                                ))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, _| this.drag_anchor = None),
                                )
                                .on_mouse_up_out(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, _| this.drag_anchor = None),
                                )
                                .on_scroll_wheel(cx.listener(
                                    |this, event: &gpui::ScrollWheelEvent, _window, cx| {
                                        cx.stop_propagation();
                                        let delta = event.delta.pixel_delta(px(48.0)).y;
                                        if delta < px(0.0) {
                                            this.camera.zoom_by(1.10);
                                        } else if delta > px(0.0) {
                                            this.camera.zoom_by(0.91);
                                        } else {
                                            return;
                                        }
                                        cx.notify();
                                    },
                                )),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        panel(
                            "Spatial Telemetry",
                            Some("exact source-bed layout / Peak+RMS / ITD / ILD / pinna / late field".into()),
                            spatial_telemetry(spatial),
                        )
                        .w(px(455.0)),
                    ),
            )
            .child(panel(
                "Image-source Early Reflection Matrix",
                Some("secondary room acoustics · source → floor/ceiling/wall bounce → left/right ear · excess delay / binaural arrival".into()),
                reflection_telemetry(spatial),
            ))
            .child(analysis_sections(snapshot.clone()))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x727d89))
                    .child("完整实验室分析已恢复：A/B/C 频谱、Transfer ΔdB、M/S、相位相关历史、Crest/动态历史、Spectrogram、Waveform、Vectorscope、True Peak/LUFS；GPU 3D 只替换空间主视图，不再删除原有工程分析能力。"),
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
        return body
            .child(div().text_sm().text_color(rgb(0x77828f)).child("等待 scene"))
            .into_any_element();
    };
    let late = late_field_telemetry(snapshot.sample_rate, snapshot.environment);
    body = body
        .child(
            div()
                .flex()
                .gap_2()
                .flex_wrap()
                .child(metric("Bed", layout_name(snapshot.layout).to_string()))
                .child(metric("Sources", snapshot.source_count.to_string()))
                .child(metric("Room", format!("{:.0}%", snapshot.environment.room_size * 100.0)))
                .child(metric("Early Wet", format!("{:.0}%", snapshot.environment.mix * 100.0)))
                .child(metric("Damp", format!("{:.0}%", snapshot.environment.damping * 100.0))),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .flex_wrap()
                .child(metric("FDN Wet", format!("{:.1}%", late.wet_gain * 100.0)))
                .child(metric("Feedback", format!("{:.3}", late.feedback_gain)))
                .child(metric("Cutoff", format!("{:.0} Hz", late.damping_cutoff_hz)))
                .child(metric("Late Delay", format!("{:.1}–{:.1} ms", late.minimum_delay_ms, late.maximum_delay_ms))),
        );
    let visible = snapshot.source_count.min(SOURCE_ROWS);
    for (index, source) in snapshot.sources[..visible].iter().copied().enumerate() {
        if !source.active {
            continue;
        }
        let channel = channel_name(snapshot.layout, index);
        let kind = if matches!(source.kind, SpatialDebugSourceKind::Lfe) {
            "LFE"
        } else {
            "FULL"
        };
        let mut row = div()
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
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(format!("#{:02} {channel} · {kind}", source.source_index)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0x9aa4af))
                            .child(format!(
                                "az {:+.1}° / el {:+.1}° / {:.2}m",
                                source.azimuth_degrees,
                                source.elevation_degrees,
                                source.distance_meters
                            )),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x76a8c8))
                    .child(format!(
                        "input Peak {:+.1} dBFS · RMS {:+.1} dBFS",
                        linear_dbfs(source.input_peak),
                        linear_dbfs(source.input_rms),
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x77828f))
                    .child(format!(
                        "ITD {:.2}smp · ILD {:+.2}dB · L/R {:.3}/{:.3}",
                        source.itd_samples, source.ild_db, source.left_gain, source.right_gain
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x68737f))
                    .child(format!(
                        "near {:.0}% · shadow {:.0}% · air {:.0}% · direct {:.3}",
                        source.near_field_amount * 100.0,
                        source.head_shadow_amount * 100.0,
                        source.air_absorption_amount * 100.0,
                        source.direct_contribution,
                    )),
            );
        if matches!(source.kind, SpatialDebugSourceKind::FullRange) {
            let pinna = pinna_cue_telemetry(
                source.azimuth_degrees,
                source.elevation_degrees,
                source.spread,
            );
            row = row.child(
                div()
                    .text_xs()
                    .text_color(rgb(0x8f86c9))
                    .child(format!(
                        "pinna {:.0}Hz · Q {:.2} · depth {:.2}dB · cue {:.0}% · L/R notch {:.0}/{:.0}Hz",
                        pinna.center_hz,
                        pinna.q,
                        pinna.depth_db,
                        pinna.cue_strength * 100.0,
                        pinna.left_center_hz,
                        pinna.right_center_hz,
                    )),
            );
        }
        body = body.child(row);
    }
    body.into_any_element()
}

fn reflection_telemetry(snapshot: Option<SpatialDebugSnapshot>) -> gpui::AnyElement {
    let mut body = div().flex().flex_col().gap_1();
    let Some(snapshot) = snapshot else {
        return body
            .child(div().text_sm().text_color(rgb(0x77828f)).child("等待 reflection matrix"))
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
        let channel = channel_name(snapshot.layout, source);
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
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(rgb(0xffc857))
                                .child(format!(
                                    "{channel} · {} · tap {}",
                                    wall_label(reflection.wall),
                                    reflection.tap_index
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x9aa4af))
                                .child(format!("{:.2} ms", reflection.delay_milliseconds)),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x77828f))
                        .child(format!(
                            "path {:.2}m (+{:.2}m) · wet {:.3} · reflect {:.3}",
                            reflection.path_length_meters,
                            reflection.excess_path_meters,
                            reflection.wet_contribution,
                            reflection.wall_reflectance
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x68737f))
                        .child(format!(
                            "arrival az {:+.1}° / el {:+.1}° · L/R delay {:.2}/{:.2} · gain {:.3}/{:.3}",
                            reflection.arrival_azimuth_degrees,
                            reflection.arrival_elevation_degrees,
                            reflection.left_delay_samples,
                            reflection.right_delay_samples,
                            reflection.left_gain,
                            reflection.right_gain
                        )),
                ),
        );
    }
    if shown == 0 {
        body = body.child(div().text_sm().text_color(rgb(0x77828f)).child("当前没有 active reflection"));
    }
    body.into_any_element()
}

fn linear_dbfs(value: f32) -> f32 {
    let value = if value.is_finite() { value.abs() } else { 0.0 };
    if value <= 1.0e-9 {
        -180.0
    } else {
        20.0 * value.log10()
    }
}

fn layout_name(layout: Option<ChannelLayout>) -> &'static str {
    match layout {
        None => "FREE SOURCE",
        Some(ChannelLayout::Stereo) => "STEREO",
        Some(ChannelLayout::Surround5_1) => "5.1",
        Some(ChannelLayout::Surround7_1) => "7.1",
        Some(ChannelLayout::Surround5_1_2) => "5.1.2",
        Some(ChannelLayout::Surround5_1_4) => "5.1.4",
        Some(ChannelLayout::Surround7_1_2) => "7.1.2",
        Some(ChannelLayout::Surround7_1_4) => "7.1.4",
    }
}

fn channel_name(layout: Option<ChannelLayout>, index: usize) -> &'static str {
    let Some(layout) = layout else {
        return "SRC";
    };
    SpeakerLayout::for_layout(layout)
        .role(index)
        .map_or("SRC", |role| role.short_name())
}

fn wall_label(wall: SpatialDebugReflectionWall) -> &'static str {
    match wall {
        SpatialDebugReflectionWall::Left => "LEFT",
        SpatialDebugReflectionWall::Right => "RIGHT",
        SpatialDebugReflectionWall::Front => "FRONT",
        SpatialDebugReflectionWall::Rear => "REAR",
        SpatialDebugReflectionWall::Floor => "FLOOR",
        SpatialDebugReflectionWall::Ceiling => "CEILING",
    }
}
