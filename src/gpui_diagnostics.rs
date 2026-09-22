use gpui::{App, PerformanceMetricsSnapshot, Timer, performance_metrics_snapshot};
use std::time::Duration;

const LOG_INTERVAL: Duration = Duration::from_secs(5);
const ENV_NAME: &str = "YINQIDAO_GPUI_DIAGNOSTICS";

#[derive(Clone, Copy, Debug, Default)]
struct CounterDeltas {
    frame_requests: usize,
    draws: usize,
    presents: usize,
    skips: usize,
    direct_presents: usize,
    retained_presents: usize,
    blur_frames: usize,
    partial_redraws: usize,
    full_redraw_fallbacks: usize,
}

impl CounterDeltas {
    fn between(previous: &PerformanceMetricsSnapshot, current: &PerformanceMetricsSnapshot) -> Self {
        Self {
            frame_requests: delta(previous.frame_request_count, current.frame_request_count),
            draws: delta(previous.draw_count, current.draw_count),
            presents: delta(previous.present_count, current.present_count),
            skips: delta(previous.skip_count, current.skip_count),
            direct_presents: delta(previous.direct_present_count, current.direct_present_count),
            retained_presents: delta(
                previous.retained_present_count,
                current.retained_present_count,
            ),
            blur_frames: delta(
                previous.backdrop_blur_frame_count,
                current.backdrop_blur_frame_count,
            ),
            partial_redraws: delta(previous.partial_redraw_count, current.partial_redraw_count),
            full_redraw_fallbacks: delta(
                previous.full_redraw_fallback_count,
                current.full_redraw_fallback_count,
            ),
        }
    }
}

#[inline]
fn delta(previous: usize, current: usize) -> usize {
    current.saturating_sub(previous)
}

#[inline]
fn duration_micros(duration: Option<Duration>) -> u128 {
    duration.map_or(0, |duration| duration.as_micros())
}

fn enabled() -> bool {
    std::env::var(ENV_NAME).ok().is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

pub(crate) fn start(cx: &mut App) {
    if !enabled() {
        return;
    }

    let mut previous = performance_metrics_snapshot();
    tracing::info!(
        target: "yinqidao::gpui_perf",
        backend = %previous.renderer_backend.as_str(),
        adapter = %previous.gpu_adapter_name,
        "GPUI 性能诊断已启用；每 5 秒输出一次采样"
    );

    cx.spawn(async move |_cx| {
        loop {
            Timer::after(LOG_INTERVAL).await;
            let current = performance_metrics_snapshot();
            emit_snapshot(&previous, &current);
            previous = current;
        }
    })
    .detach();
}

fn emit_snapshot(previous: &PerformanceMetricsSnapshot, current: &PerformanceMetricsSnapshot) {
    let counters = CounterDeltas::between(previous, current);

    tracing::info!(
        target: "yinqidao::gpui_perf",
        backend = %current.renderer_backend.as_str(),
        adapter = %current.gpu_adapter_name,
        adapter_type = %current.gpu_adapter_type,
        present_fps = current.present_fps,
        frame_requests = counters.frame_requests,
        draws = counters.draws,
        presents = counters.presents,
        skips = counters.skips,
        direct_presents = counters.direct_presents,
        retained_presents = counters.retained_presents,
        blur_frames = counters.blur_frames,
        partial_redraws = counters.partial_redraws,
        full_redraw_fallbacks = counters.full_redraw_fallbacks,
        frame_build_us = duration_micros(current.frame_build_time),
        layout_us = duration_micros(current.frame_layout_time),
        prepaint_us = duration_micros(current.frame_prepaint_time),
        paint_us = duration_micros(current.frame_paint_time),
        backend_draw_us = duration_micros(current.frame_backend_draw_time),
        windows = ?current.window_metrics,
        "GPUI frame diagnostics (5s delta)"
    );

    tracing::info!(
        target: "yinqidao::gpui_perf",
        layout_nodes = current.layout_nodes,
        measured_layout_nodes = current.measured_layout_nodes,
        dirty_rects = current.dirty_rect_count,
        dirty_area = current.dirty_rect_area,
        scene_primitives = current.scene_primitives,
        scene_batches = current.scene_batches,
        replayed_primitives = current.scene_replayed_primitives,
        rebuilt_segments = current.scene_segment_rebuild_count,
        reused_segments = current.scene_segment_reuse_count,
        encoded_primitives = current.encoded_scene_primitives,
        encoded_batches = current.encoded_scene_batches,
        upload_bytes = current.upload_bytes,
        atlas_upload_bytes = current.atlas_upload_bytes,
        animation_bytes = current.animation_upload_bytes,
        custom_mesh_parameter_bytes = current.custom_mesh_parameter_upload_bytes,
        "GPUI scene/upload diagnostics"
    );

    tracing::info!(
        target: "yinqidao::gpui_perf",
        surface_format = %current.gpu_surface_format,
        alpha_mode = %current.gpu_surface_alpha_mode,
        present_mode = %current.gpu_surface_present_mode,
        mask_passes = current.mask_pass_count,
        main_passes = current.main_pass_count,
        composite_passes = current.composite_pass_count,
        retained_copy_pixels = current.retained_copy_pixels,
        retained_copy_estimated_bytes = current.retained_copy_estimated_bytes,
        has_retained_target = current.has_retained_frame_target,
        blur_primitives = current.backdrop_blur_primitives,
        blur_source_pixels = current.backdrop_blur_source_pixels,
        blur_target_pixels = current.backdrop_blur_target_pixels,
        blur_level_pixels = ?current.backdrop_blur_level_pixels,
        gpu_wait_us = duration_micros(current.gpu_submission_wait_time),
        gpu_slow_waits = current.gpu_submission_slow_wait_count,
        surface_reconfigures = current.gpu_surface_reconfigure_count,
        surface_errors = current.gpu_surface_error_count,
        "GPUI present/compositor diagnostics"
    );
}
