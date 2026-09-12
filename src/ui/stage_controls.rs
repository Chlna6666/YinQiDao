use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    BorrowAppContext as _, Context, Entity, Global, IntoElement, Render, SharedString, WeakEntity,
    Window, div, hsla, prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    audio::{AudioEngine, PlayerCommand},
    model::PlaybackState,
};

use super::{
    components::{SliderStyle, slider::InteractiveSliderState},
    shell::{DragTarget, MusicApp},
    theme::{self, ACCENT_RED, format_remaining_time, format_time, themed_icon},
};

const TRANSPORT_MIN_SLEEP_MS: u64 = 8;
const TRANSPORT_MAX_SLEEP_MS: u64 = 1_000;
const STAGE_CHROME_FADE_DURATION: Duration = Duration::from_millis(220);
const STAGE_CHROME_IDLE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug)]
struct StageChromeFade {
    value: f32,
    from: f32,
    to: f32,
    started_at: Option<Instant>,
    duration: Duration,
}

impl StageChromeFade {
    fn new(visible: bool) -> Self {
        let value = if visible { 1.0 } else { 0.0 };
        Self {
            value,
            from: value,
            to: value,
            started_at: None,
            duration: STAGE_CHROME_FADE_DURATION,
        }
    }

    #[inline]
    fn ease(progress: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        if progress < 0.5 {
            4.0 * progress * progress * progress
        } else {
            1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
        }
    }

    fn sample(&self, now: Instant) -> f32 {
        let Some(started_at) = self.started_at else {
            return self.value;
        };
        if self.duration.is_zero() {
            return self.to;
        }
        let linear = now.saturating_duration_since(started_at).as_secs_f32()
            / self.duration.as_secs_f32();
        let eased = Self::ease(linear);
        self.from + (self.to - self.from) * eased
    }

    fn set_target(&mut self, visible: bool) -> bool {
        let target = if visible { 1.0 } else { 0.0 };
        if (self.to - target).abs() <= 0.001 {
            return false;
        }

        let now = Instant::now();
        let current = self.sample(now).clamp(0.0, 1.0);
        self.value = current;
        self.from = current;
        self.to = target;

        let distance = (target - current).abs();
        if distance <= 0.001 {
            self.value = target;
            self.from = target;
            self.started_at = None;
            return true;
        }

        self.duration = Duration::from_secs_f32(
            (STAGE_CHROME_FADE_DURATION.as_secs_f32() * distance).max(0.001),
        );
        self.started_at = Some(now);
        true
    }

    fn advance(&mut self, now: Instant) -> bool {
        let Some(started_at) = self.started_at else {
            return false;
        };

        self.value = self.sample(now).clamp(0.0, 1.0);
        if now.saturating_duration_since(started_at) >= self.duration {
            self.value = self.to;
            self.from = self.to;
            self.started_at = None;
            false
        } else {
            true
        }
    }

    #[inline]
    fn value(&self) -> f32 {
        self.value.clamp(0.0, 1.0)
    }

    #[inline]
    fn is_animating(&self) -> bool {
        self.started_at.is_some()
    }
}

#[derive(Default)]
struct StageControlsViewCache {
    view: Option<Entity<StageControlsView>>,
}

impl Global for StageControlsViewCache {}

#[derive(Default)]
struct StageTransportViewCache {
    view: Option<Entity<StageTransportView>>,
}

impl Global for StageTransportViewCache {}

#[derive(Default)]
struct StageTitlebarViewCache {
    view: Option<Entity<StageTitlebarView>>,
}

impl Global for StageTitlebarViewCache {}

#[inline]
fn stage_chrome_target_visible(app: &MusicApp) -> bool {
    if !app.stage_open || app.stage_suppress_wake_until.is_some() {
        return false;
    }

    let idle = app.stage_last_user_activity.elapsed() >= STAGE_CHROME_IDLE_TIMEOUT
        && !app.seeking
        && !app.volume_dragging
        && !app.stage_controls_hovered;
    !idle
}

pub(super) fn view(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> Entity<StageControlsView> {
    let transport = transport_view(app, cx);
    let parent = cx.entity().downgrade();
    let view = cx.update_default_global(|cache: &mut StageControlsViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| StageControlsView::new(parent, transport.clone()));
        cache.view = Some(view.clone());
        view
    });

    let stage_active = app.stage_open || app.stage_animating;
    view.update(cx, |view, cx| view.sync_from_app(app, stage_active, cx));
    view
}

pub(super) fn titlebar_view(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> Entity<StageTitlebarView> {
    let parent = cx.entity().downgrade();
    let view = cx.update_default_global(|cache: &mut StageTitlebarViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| StageTitlebarView::new(parent));
        cache.view = Some(view.clone());
        view
    });
    view.update(cx, |view, cx| view.sync_from_app(app, cx));
    view
}

fn transport_view(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> Entity<StageTransportView> {
    let parent = cx.entity().downgrade();
    let engine = app.engine.clone();
    let view = cx.update_default_global(|cache: &mut StageTransportViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| StageTransportView::new(parent, engine));
        cache.view = Some(view.clone());
        view
    });

    let stage_active = app.stage_open || app.stage_animating;
    let controls_visible = stage_chrome_target_visible(app) || app.drag_target.is_some();
    view.update(cx, |view, cx| {
        view.sync_from_app(app, stage_active, controls_visible, cx)
    });
    view
}

pub(super) struct StageControlsView {
    parent: WeakEntity<MusicApp>,
    transport: Entity<StageTransportView>,
    volume_slider: InteractiveSliderState,
    stage_active: bool,
    fade: StageChromeFade,
    playback_state: PlaybackState,
    volume: f32,
}

impl StageControlsView {
    fn new(parent: WeakEntity<MusicApp>, transport: Entity<StageTransportView>) -> Self {
        let click_parent = parent.clone();
        let drag_parent = parent.clone();
        let commit_parent = parent.clone();
        let volume_slider = InteractiveSliderState::new(
            "stage-volume-track",
            move |ratio, cx| {
                let _ = click_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    app.pending_volume_ratio = None;
                    app.set_app_volume(ratio, app_cx);
                });
            },
            move |ratio, cx| {
                let _ = drag_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    if app.drag_target == Some(DragTarget::Volume) {
                        app.update_drag_ratio(DragTarget::Volume, ratio, app_cx);
                    } else {
                        app.begin_drag(DragTarget::Volume, ratio, app_cx);
                    }
                    app.send(PlayerCommand::SetVolume(ratio));
                });
            },
            move |ratio, cx| {
                let _ = commit_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    if app.drag_target == Some(DragTarget::Volume) {
                        app.update_drag_ratio(DragTarget::Volume, ratio, app_cx);
                    } else {
                        app.begin_drag(DragTarget::Volume, ratio, app_cx);
                    }
                    app.commit_drag(app_cx);
                    app.pending_volume_ratio = None;
                });
            },
        );

        Self {
            parent,
            transport,
            volume_slider,
            stage_active: false,
            fade: StageChromeFade::new(true),
            playback_state: PlaybackState::Paused,
            volume: 1.0,
        }
    }

    fn sync_from_app(
        &mut self,
        app: &MusicApp,
        stage_active: bool,
        cx: &mut Context<Self>,
    ) {
        let target_visible = stage_chrome_target_visible(app);
        let playback_state = app.snapshot.state;
        let volume = app.displayed_volume_ratio();
        let changed = self.stage_active != stage_active
            || self.playback_state != playback_state
            || (self.volume - volume).abs() > 0.0005;
        let fade_changed = self.fade.set_target(target_visible);

        // shell.rs still owns the legacy float until its final wiring cleanup. Treat that float as
        // a discrete target and collapse the first legacy interpolation frame back to 0/1. This
        // prevents MusicApp from requesting RAF for the whole 220 ms chrome fade; subsequent frames
        // are requested only by this retained Entity.
        let target_value = if target_visible { 1.0 } else { 0.0 };
        if (app.stage_controls_visibility - target_value).abs() > 0.005 {
            let parent = self.parent.clone();
            cx.defer(move |cx| {
                let _ = parent.update(cx, |app, app_cx| {
                    let target_visible = stage_chrome_target_visible(app);
                    let target_value = if target_visible { 1.0 } else { 0.0 };
                    if (app.stage_controls_visibility - target_value).abs() > 0.005 {
                        app.stage_controls_visibility = target_value;
                        app_cx.notify();
                    }
                });
            });
        }

        self.stage_active = stage_active;
        self.playback_state = playback_state;
        self.volume = volume;

        if changed || fade_changed {
            cx.notify();
        }
    }
}

impl Render for StageControlsView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.fade.advance(Instant::now()) {
            window.request_animation_frame();
        }
        let visibility = self.fade.value();
        let playing = self.playback_state == PlaybackState::Playing;
        let volume = self.volume;
        let parent = self.parent.clone();

        // Keep the dock in its stable flex slot. Only opacity changes, so the animation cannot
        // perturb layout or hitbox geometry while the retained Entity repaints itself at vsync.
        div()
            .id("stage-bottom-dock")
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
            .on_hover({
                let parent = parent.clone();
                move |hovered: &bool, _, cx| {
                    let _ = parent.update(cx, |app, app_cx| {
                        let changed = app.stage_controls_hovered != *hovered;
                        app.stage_controls_hovered = *hovered;
                        if changed && app.stage_suppress_wake_until.is_none() {
                            app.stage_last_user_activity = Instant::now();
                            app_cx.notify();
                        }
                    });
                }
            })
            .on_mouse_move({
                let parent = parent.clone();
                move |event: &gpui::MouseMoveEvent, _, cx| {
                    let _ = parent.update(cx, |app, cx| {
                        app.handle_stage_mouse_move(event.position, cx);
                    });
                }
            })
            .child(self.transport.clone())
            .child(control_button("stage-prev-btn", icon!(skip_back), {
                let parent = parent.clone();
                move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = parent.update(cx, |app, app_cx| {
                        app.wake_stage_controls_immediately(app_cx);
                        app.previous(app_cx);
                    });
                }
            }))
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
                    .child(themed_icon(
                        if playing { icon!(pause) } else { icon!(play) },
                        22.0,
                        hsla(0.0, 0.0, 1.0, 1.0),
                    ))
                    .on_mouse_down(gpui::MouseButton::Left, {
                        let parent = parent.clone();
                        move |_, _, cx| {
                            cx.stop_propagation();
                            let _ = parent.update(cx, |app, app_cx| {
                                app.wake_stage_controls_immediately(app_cx);
                                app.toggle_play(app_cx);
                            });
                        }
                    }),
            )
            .child(control_button("stage-next-btn", icon!(skip_forward), {
                let parent = parent.clone();
                move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = parent.update(cx, |app, app_cx| {
                        app.wake_stage_controls_immediately(app_cx);
                        app.next(app_cx);
                    });
                }
            }))
            .child(
                div()
                    .id("stage-volume-group")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 1.0, 0.08))
                    .child(
                        div()
                            .id("stage-volume-mute")
                            .cursor_pointer()
                            .child(themed_icon(
                                if volume <= 0.001 {
                                    icon!(volume_x)
                                } else if volume < 0.5 {
                                    icon!(volume_1)
                                } else {
                                    icon!(volume_2)
                                },
                                16.0,
                                hsla(0.0, 0.0, 1.0, 0.82),
                            ))
                            .on_mouse_down(gpui::MouseButton::Left, {
                                let parent = parent.clone();
                                move |_, _, cx| {
                                    cx.stop_propagation();
                                    let _ = parent.update(cx, |app, app_cx| {
                                        app.wake_stage_controls_immediately(app_cx);
                                        app.pending_volume_ratio = None;
                                        app.toggle_mute(app_cx);
                                    });
                                }
                            }),
                    )
                    .child(
                        self.volume_slider
                            .render(volume, SliderStyle::stage_volume())
                            .w(px(72.0))
                            .on_scroll_wheel({
                                let parent = parent.clone();
                                move |event: &gpui::ScrollWheelEvent, _, cx| {
                                    cx.stop_propagation();
                                    let delta = event.delta.pixel_delta(px(48.0)).y;
                                    let _ = parent.update(cx, |app, app_cx| {
                                        if delta < px(0.0) {
                                            app.adjust_volume(0.04, app_cx);
                                        } else if delta > px(0.0) {
                                            app.adjust_volume(-0.04, app_cx);
                                        }
                                        app.wake_stage_controls_immediately(app_cx);
                                    });
                                }
                            }),
                    ),
            )
            .into_any_element()
    }
}

pub(super) struct StageTitlebarView {
    parent: WeakEntity<MusicApp>,
    fade: StageChromeFade,
    title_key: u64,
    title: SharedString,
}

impl StageTitlebarView {
    fn new(parent: WeakEntity<MusicApp>) -> Self {
        Self {
            parent,
            fade: StageChromeFade::new(true),
            title_key: 0,
            title: SharedString::new_static("沉浸音乐大舞台"),
        }
    }

    fn sync_from_app(&mut self, app: &MusicApp, cx: &mut Context<Self>) {
        let fade_changed = self.fade.set_target(stage_chrome_target_visible(app));
        let title_key = stage_title_fingerprint(app);
        let title_changed = title_key != self.title_key;
        if title_changed {
            self.title_key = title_key;
            self.title = app.snapshot.current_track.as_ref().map_or_else(
                || SharedString::new_static("沉浸音乐大舞台"),
                |track| SharedString::from(format!("{} · {}", track.title, track.artist)),
            );
        }
        if fade_changed || title_changed {
            cx.notify();
        }
    }
}

impl Render for StageTitlebarView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if self.fade.advance(Instant::now()) {
            window.request_animation_frame();
        }
        let visibility = self.fade.value();
        if visibility <= 0.001 && !self.fade.is_animating() {
            return div().w_full().h(px(38.0)).into_any_element();
        }

        let parent = self.parent.clone();
        let hide_parent = parent.clone();
        let collapse_parent = parent.clone();
        let title = self.title.clone();

        div()
            .w_full()
            .h(px(38.0))
            .opacity(visibility)
            .child(
                div()
                    .id("stage-titlebar")
                    .w_full()
                    .h(px(38.0))
                    .flex_none()
                    .bg(hsla(0.0, 0.0, 0.0, 0.10))
                    .border_b_1()
                    .border_color(hsla(0.0, 0.0, 1.0, 0.05))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .child(
                        div()
                            .occlude()
                            .window_control_area(gpui::WindowControlArea::Client)
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(stage_traffic_light_button(
                                "stage-window-close",
                                rgb(0xff_5f_56),
                                |_, _, cx| cx.quit(),
                            ))
                            .child(stage_traffic_light_button(
                                "stage-window-minimize",
                                rgb(0xff_bd_2e),
                                |_, window, _| window.minimize_window(),
                            ))
                            .child(stage_traffic_light_button(
                                "stage-window-maximize",
                                rgb(0x27_c9_3f),
                                |_, window, _| {
                                    if window.is_maximized() {
                                        window.restore_window();
                                    } else {
                                        window.maximize_window();
                                    }
                                },
                            )),
                    )
                    .child(
                        div()
                            .id("stage-drag-region")
                            .window_control_area(gpui::WindowControlArea::Drag)
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(hsla(0.0, 0.0, 1.0, 0.70))
                                    .truncate()
                                    .child(title),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                |event: &gpui::MouseDownEvent, window, _| {
                                    if event.click_count >= 2 {
                                        window.titlebar_double_click();
                                    }
                                },
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("stage-quick-hide-btn")
                                    .occlude()
                                    .window_control_area(gpui::WindowControlArea::Client)
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(hsla(0.0, 0.0, 1.0, 0.12))
                                    .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.22)))
                                    .transition(theme::press_transition())
                                    .active(|style| style.scale(0.95))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        move |event: &gpui::MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            let _ = hide_parent.update(cx, |app, app_cx| {
                                                app.hide_stage_controls_immediately(
                                                    event.position,
                                                    app_cx,
                                                );
                                            });
                                        },
                                    )
                                    .child(themed_icon(
                                        icon!(eye_off),
                                        14.0,
                                        hsla(0.0, 0.0, 1.0, 0.90),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(hsla(0.0, 0.0, 1.0, 0.90))
                                            .child("纯享沉浸"),
                                    ),
                            )
                            .child(
                                div()
                                    .id("stage-collapse-btn")
                                    .occlude()
                                    .window_control_area(gpui::WindowControlArea::Client)
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(hsla(0.0, 0.0, 1.0, 0.12))
                                    .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.22)))
                                    .transition(theme::press_transition())
                                    .active(|style| style.scale(0.95))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            let _ = collapse_parent.update(cx, |app, app_cx| {
                                                app.close_stage(app_cx);
                                            });
                                        },
                                    )
                                    .child(themed_icon(
                                        icon!(chevron_down),
                                        14.0,
                                        hsla(0.0, 0.0, 1.0, 0.90),
                                    ))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(hsla(0.0, 0.0, 1.0, 0.90))
                                            .child("收起舞台 (Esc)"),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

fn stage_title_fingerprint(app: &MusicApp) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    #[inline]
    fn mix(hash: &mut u64, bytes: &[u8]) {
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(PRIME);
        }
    }

    let mut hash = OFFSET;
    if let Some(track) = app.snapshot.current_track.as_ref() {
        mix(&mut hash, &track.id.to_le_bytes());
        mix(&mut hash, track.title.as_bytes());
        mix(&mut hash, &[0xff]);
        mix(&mut hash, track.artist.as_bytes());
    }
    hash.wrapping_mul(PRIME)
}

struct StageTransportView {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    stage_active: bool,
    controls_visible: bool,
    playback_state: PlaybackState,
    drag_progress_ratio: Option<f32>,
    progress: Option<Entity<StageProgressView>>,
}

impl StageTransportView {
    fn new(parent: WeakEntity<MusicApp>, engine: Option<Arc<AudioEngine>>) -> Self {
        Self {
            parent,
            engine,
            stage_active: false,
            controls_visible: true,
            playback_state: PlaybackState::Paused,
            drag_progress_ratio: None,
            progress: None,
        }
    }

    fn sync_from_app(
        &mut self,
        app: &MusicApp,
        stage_active: bool,
        controls_visible: bool,
        cx: &mut Context<Self>,
    ) {
        let engine_changed = match (&self.engine, &app.engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        let playback_state = app.snapshot.state;
        let drag_progress_ratio = app.drag_progress_ratio;
        let changed = engine_changed
            || self.stage_active != stage_active
            || self.controls_visible != controls_visible
            || self.playback_state != playback_state
            || option_ratio_changed(self.drag_progress_ratio, drag_progress_ratio, 0.0005);

        if engine_changed {
            self.engine = app.engine.clone();
        }
        self.stage_active = stage_active;
        self.controls_visible = controls_visible;
        self.playback_state = playback_state;
        self.drag_progress_ratio = drag_progress_ratio;

        if let Some(progress) = &self.progress {
            let engine = self.engine.clone();
            let playback_state = self.playback_state;
            let stage_active = self.stage_active;
            let controls_visible = self.controls_visible;
            let drag_progress_ratio = self.drag_progress_ratio;
            progress.update(cx, |progress, cx| {
                progress.sync(
                    engine,
                    playback_state,
                    stage_active,
                    controls_visible,
                    drag_progress_ratio,
                    cx,
                )
            });
        }

        if changed {
            cx.notify();
        }
    }

    fn ensure_progress(&mut self, cx: &mut Context<Self>) -> Entity<StageProgressView> {
        if let Some(progress) = &self.progress {
            return progress.clone();
        }
        let parent = self.parent.clone();
        let owner = cx.entity().downgrade();
        let engine = self.engine.clone();
        let playback_state = self.playback_state;
        let stage_active = self.stage_active;
        let controls_visible = self.controls_visible;
        let drag_progress_ratio = self.drag_progress_ratio;
        let progress = cx.new(move |_| {
            StageProgressView::new(
                parent,
                owner,
                engine,
                playback_state,
                stage_active,
                controls_visible,
                drag_progress_ratio,
            )
        });
        self.progress = Some(progress.clone());
        progress
    }

    #[inline]
    fn clock_should_run(&self) -> bool {
        self.stage_active
            && self.controls_visible
            && self.playback_state == PlaybackState::Playing
            && self.drag_progress_ratio.is_none()
            && self.engine.is_some()
    }
}

impl Render for StageTransportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let progress = self.ensure_progress(cx);
        let (_, live_position_ms, duration_ms) = self.engine.as_ref().map_or(
            (PlaybackState::Stopped, 0, 0),
            |engine| engine.progress(),
        );
        let position = self.drag_progress_ratio.map_or(live_position_ms, |ratio| {
            (duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64
        });

        if self.clock_should_run() {
            let remainder = live_position_ms % 1_000;
            let delay_ms = (1_000 - remainder)
                .clamp(TRANSPORT_MIN_SLEEP_MS, TRANSPORT_MAX_SLEEP_MS);
            window.request_invalidation_at(Instant::now() + Duration::from_millis(delay_ms), cx);
        }

        div()
            .flex()
            .flex_1()
            .min_w(px(0.0))
            .items_center()
            .gap_5()
            .child(
                div()
                    .text_xs()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                    .child(format_time(position)),
            )
            .child(progress)
            .child(
                div()
                    .text_xs()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                    .child(format_remaining_time(position, duration_ms)),
            )
    }
}

struct StageProgressView {
    parent: WeakEntity<MusicApp>,
    owner: WeakEntity<StageTransportView>,
    engine: Option<Arc<AudioEngine>>,
    playback_state: PlaybackState,
    stage_active: bool,
    controls_visible: bool,
    drag_progress_ratio: Option<f32>,
    slider: Option<InteractiveSliderState>,
}

impl StageProgressView {
    fn new(
        parent: WeakEntity<MusicApp>,
        owner: WeakEntity<StageTransportView>,
        engine: Option<Arc<AudioEngine>>,
        playback_state: PlaybackState,
        stage_active: bool,
        controls_visible: bool,
        drag_progress_ratio: Option<f32>,
    ) -> Self {
        Self {
            parent,
            owner,
            engine,
            playback_state,
            stage_active,
            controls_visible,
            drag_progress_ratio,
            slider: None,
        }
    }

    fn sync(
        &mut self,
        engine: Option<Arc<AudioEngine>>,
        playback_state: PlaybackState,
        stage_active: bool,
        controls_visible: bool,
        drag_progress_ratio: Option<f32>,
        cx: &mut Context<Self>,
    ) {
        let engine_changed = match (&self.engine, &engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        let changed = engine_changed
            || self.playback_state != playback_state
            || self.stage_active != stage_active
            || self.controls_visible != controls_visible
            || option_ratio_changed(self.drag_progress_ratio, drag_progress_ratio, 0.0005);
        self.engine = engine;
        self.playback_state = playback_state;
        self.stage_active = stage_active;
        self.controls_visible = controls_visible;
        self.drag_progress_ratio = drag_progress_ratio;
        if changed {
            cx.notify();
        }
    }

    fn ensure_slider(&mut self, cx: &mut Context<Self>) {
        if self.slider.is_some() {
            return;
        }

        let parent = self.parent.clone();
        let click_parent = parent.clone();
        let drag_parent = parent.clone();
        let this_click = cx.entity().downgrade();
        let this_drag = this_click.clone();
        let this_commit = this_click.clone();

        self.slider = Some(InteractiveSliderState::new(
            "stage-progress-track",
            move |ratio, cx| {
                let _ = click_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    app.seek_to_ratio(ratio, app_cx);
                });
                let _ = this_click.update(cx, |this, cx| {
                    this.drag_progress_ratio = None;
                    let _ = this.owner.update(cx, |owner, cx| {
                        owner.drag_progress_ratio = None;
                        cx.notify();
                    });
                    cx.notify();
                });
            },
            move |ratio, cx| {
                let _ = drag_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    if app.drag_target == Some(DragTarget::Progress) {
                        app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                    } else {
                        app.begin_drag(DragTarget::Progress, ratio, app_cx);
                    }
                });
                let _ = this_drag.update(cx, |this, cx| {
                    this.drag_progress_ratio = Some(ratio);
                    let _ = this.owner.update(cx, |owner, cx| {
                        owner.drag_progress_ratio = Some(ratio);
                        cx.notify();
                    });
                    cx.notify();
                });
            },
            move |ratio, cx| {
                let _ = parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    if app.drag_target == Some(DragTarget::Progress) {
                        app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                    } else {
                        app.begin_drag(DragTarget::Progress, ratio, app_cx);
                    }
                    app.commit_drag(app_cx);
                });
                let _ = this_commit.update(cx, |this, cx| {
                    this.drag_progress_ratio = None;
                    let _ = this.owner.update(cx, |owner, cx| {
                        owner.drag_progress_ratio = None;
                        cx.notify();
                    });
                    cx.notify();
                });
            },
        ));
    }
}

impl Render for StageProgressView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.stage_active
            && self.controls_visible
            && self.playback_state == PlaybackState::Playing
            && self.drag_progress_ratio.is_none()
            && self.engine.is_some()
        {
            // The pinned GPUI fork targets request_animation_frame() at this Entity. Only the
            // progress rail follows display vsync; StageTransport/StageControls/MusicApp stay clean.
            window.request_animation_frame();
        }

        let (_, live_position_ms, duration_ms) = self.engine.as_ref().map_or(
            (PlaybackState::Stopped, 0, 0),
            |engine| engine.progress(),
        );
        let drag_progress_ratio = self.drag_progress_ratio;
        let progress_ratio = drag_progress_ratio.unwrap_or_else(|| {
            if duration_ms == 0 {
                0.0
            } else {
                (live_position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
            }
        });

        self.ensure_slider(cx);
        self.slider
            .as_ref()
            .expect("stage progress slider must be initialized")
            .render(progress_ratio, SliderStyle::stage_progress())
            .flex_1()
            .min_w(px(80.0))
            .into_any_element()
    }
}

fn option_ratio_changed(current: Option<f32>, next: Option<f32>, epsilon: f32) -> bool {
    match (current, next) {
        (Some(current), Some(next)) => (current - next).abs() > epsilon,
        (None, None) => false,
        _ => true,
    }
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
        .child(themed_icon(
            icon,
            20.0,
            hsla(0.0, 0.0, 1.0, 0.85),
        ))
        .on_mouse_down(gpui::MouseButton::Left, listener)
}

fn stage_traffic_light_button(
    id: &'static str,
    color: gpui::Rgba,
    listener: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(12.0))
        .rounded_full()
        .bg(color)
        .border_1()
        .border_color(hsla(0.0, 0.0, 0.0, 0.15))
        .cursor_pointer()
        .occlude()
        .window_control_area(gpui::WindowControlArea::Client)
        .hover(|style| style.opacity(0.80))
        .transition(theme::press_transition())
        .active(|style| style.scale(0.90))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            listener(event, window, cx);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_ratio_change_uses_epsilon() {
        assert!(!option_ratio_changed(Some(0.5), Some(0.5001), 0.001));
        assert!(option_ratio_changed(Some(0.5), Some(0.51), 0.001));
        assert!(option_ratio_changed(None, Some(0.5), 0.001));
        assert!(!option_ratio_changed(None, None, 0.001));
    }

    #[test]
    fn stage_chrome_easing_is_bounded() {
        assert_eq!(StageChromeFade::ease(0.0), 0.0);
        assert!((StageChromeFade::ease(0.5) - 0.5).abs() < f32::EPSILON);
        assert_eq!(StageChromeFade::ease(1.0), 1.0);
    }

    #[test]
    fn stage_chrome_timeout_matches_stage_policy() {
        assert_eq!(STAGE_CHROME_IDLE_TIMEOUT, Duration::from_secs(20));
    }
}
