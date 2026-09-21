use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyView, BorrowAppContext as _, Context, Easing, Entity, Global, IntoElement, Render,
    SharedString, StyleRefinement, Subscription, Transition, TransitionProperty, WeakEntity, Window, div,
    hsla,
    prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    audio::{AudioEngine, PlayerCommand},
    model::PlaybackState,
};

use super::{
    app_ui_events::{self, AppUiEvent, AppUiEventBridge},
    components::{
        SliderStyle,
        slider::InteractiveSliderState,
    },
    shell::MusicApp,
    stage_chrome,
    theme::{self, ACCENT_RED, format_remaining_time, format_time, themed_icon},
};

const STAGE_PROGRESS_REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const VOLUME_COMMAND_INTERVAL: Duration = Duration::from_micros(16_667);
const TRANSPORT_MIN_SLEEP_MS: u64 = 8;
const TRANSPORT_MAX_SLEEP_MS: u64 = 1_000;
const STAGE_CHROME_FADE_DURATION: Duration = Duration::from_millis(220);
const STAGE_TRANSPORT_HEIGHT: f32 = 48.0;
const STAGE_PROGRESS_HEIGHT: f32 = 30.0;

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
        let linear =
            now.saturating_duration_since(started_at).as_secs_f32() / self.duration.as_secs_f32();
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
    fn target_value(&self) -> f32 {
        self.to.clamp(0.0, 1.0)
    }

    fn deadline(&self) -> Option<Instant> {
        self.started_at.map(|started_at| started_at + self.duration)
    }
}

fn stage_chrome_fade_transition() -> Transition {
    Transition::new(STAGE_CHROME_FADE_DURATION)
        .ease(Easing::InOutCubic)
        .properties([TransitionProperty::Opacity])
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

pub(super) fn view(app: &MusicApp, cx: &mut Context<MusicApp>) -> Entity<StageControlsView> {
    let ui_events = app_ui_events::bridge(cx);
    let transport = transport_view(app, cx, ui_events.clone());
    let parent = cx.entity().downgrade();
    let engine = app.engine.clone();
    let view = cx.update_default_global(|cache: &mut StageControlsViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view_events = ui_events.clone();
        let view = cx.new(move |cx| {
            StageControlsView::new(parent, transport.clone(), engine, view_events, cx)
        });
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
    ui_events: Entity<AppUiEventBridge>,
) -> Entity<StageTransportView> {
    let parent = cx.entity().downgrade();
    let engine = app.engine.clone();
    let view = cx.update_default_global(|cache: &mut StageTransportViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view_events = ui_events.clone();
        let view =
            cx.new(move |cx| StageTransportView::new(parent, engine, view_events, cx));
        cache.view = Some(view.clone());
        view
    });

    let stage_active = app.stage_open || app.stage_animating;
    let controls_visible = stage_chrome::target_visible(app);
    view.update(cx, |view, cx| {
        view.sync_from_app(app, stage_active, controls_visible, cx)
    });
    view
}

pub(super) struct StageControlsView {
    parent: WeakEntity<MusicApp>,
    transport: Entity<StageTransportView>,
    engine: Option<Arc<AudioEngine>>,
    volume_slider: Option<InteractiveSliderState>,
    volume_drag_ratio: Option<f32>,
    stage_active: bool,
    fade: StageChromeFade,
    playback_state: PlaybackState,
    volume: f32,
    last_volume_command_at: Instant,
    last_volume_command: f32,
    _ui_subscription: Subscription,
}

impl StageControlsView {
    fn new(
        parent: WeakEntity<MusicApp>,
        transport: Entity<StageTransportView>,
        engine: Option<Arc<AudioEngine>>,
        ui_events: Entity<AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let ui_subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            if let AppUiEvent::PlaybackStateChanged(state) = *event
                && this.playback_state != state
            {
                this.playback_state = state;
                cx.notify();
            }
        });
        Self {
            parent,
            transport,
            engine,
            volume_slider: None,
            volume_drag_ratio: None,
            stage_active: false,
            fade: StageChromeFade::new(true),
            playback_state: PlaybackState::Paused,
            volume: 1.0,
            last_volume_command_at: Instant::now() - VOLUME_COMMAND_INTERVAL,
            last_volume_command: 1.0,
            _ui_subscription: ui_subscription,
        }
    }

    fn ensure_volume_slider(&mut self, cx: &mut Context<Self>) {
        if self.volume_slider.is_some() {
            return;
        }

        let parent = self.parent.clone();
        let click_parent = parent.clone();
        let commit_parent = parent;
        let this_click = cx.entity().downgrade();
        let this_drag = this_click.clone();
        let this_commit = this_click.clone();

        self.volume_slider = Some(InteractiveSliderState::new(
            "stage-volume-track",
            move |ratio, cx| {
                let _ = this_click.update(cx, |this, cx| {
                    this.volume_drag_ratio = None;
                    this.volume = ratio;
                    cx.notify();
                });
                let _ = click_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    app.pending_volume_ratio = None;
                    app.set_app_volume(ratio, app_cx);
                });
            },
            move |ratio, cx| {
                let ratio = ratio.clamp(0.0, 1.0);
                let _ = this_drag.update(cx, |this, cx| {
                    if this
                        .volume_drag_ratio
                        .is_some_and(|current| (current - ratio).abs() < 0.002)
                    {
                        return;
                    }
                    this.volume_drag_ratio = Some(ratio);
                    let now = Instant::now();
                    let due = now.saturating_duration_since(this.last_volume_command_at)
                        >= VOLUME_COMMAND_INTERVAL;
                    let stepped = (this.last_volume_command - ratio).abs() >= 0.02;
                    if due || stepped {
                        this.last_volume_command_at = now;
                        this.last_volume_command = ratio;
                        if let Some(engine) = &this.engine {
                            let _ = engine.try_send(PlayerCommand::SetVolume(ratio));
                        }
                    }
                    cx.notify();
                });
            },
            move |ratio, cx| {
                let _ = this_commit.update(cx, |this, cx| {
                    this.volume_drag_ratio = None;
                    this.volume = ratio;
                    cx.notify();
                });
                let _ = commit_parent.update(cx, |app, app_cx| {
                    app.wake_stage_controls_immediately(app_cx);
                    app.pending_volume_ratio = None;
                    app.set_app_volume(ratio, app_cx);
                });
            },
        ));
    }

    fn sync_from_app(&mut self, app: &MusicApp, stage_active: bool, cx: &mut Context<Self>) {
        let target_visible = stage_chrome::target_visible(app);
        let playback_state = app.snapshot.state;
        let engine_changed = match (&self.engine, &app.engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        let app_volume = app.displayed_volume_ratio();
        let volume_changed =
            self.volume_drag_ratio.is_none() && (self.volume - app_volume).abs() > 0.0005;
        let changed = engine_changed
            || self.stage_active != stage_active
            || self.playback_state != playback_state
            || volume_changed;
        let fade_changed = self.fade.set_target(target_visible);

        if engine_changed {
            self.engine = app.engine.clone();
        }
        self.stage_active = stage_active;
        self.playback_state = playback_state;
        if self.volume_drag_ratio.is_none() {
            self.volume = app_volume;
        }

        if changed || fade_changed {
            cx.notify();
        }
    }
}

impl Render for StageControlsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let animating = self.fade.advance(now);
        if animating && let Some(deadline) = self.fade.deadline() {
            window.request_invalidation_at(deadline, cx);
        }
        let visibility = if animating {
            self.fade.target_value()
        } else {
            self.fade.value()
        };
        let playing = self.playback_state == PlaybackState::Playing;
        self.ensure_volume_slider(cx);
        let volume = self.volume_drag_ratio.unwrap_or(self.volume);
        let parent = self.parent.clone();

        let transport_buttons = div()
            .id("stage-transport-buttons")
            .w_full()
            .h(px(48.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .gap_4()
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
                    .size(px(48.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(ACCENT_RED)
                    .hover(|style| style.opacity(0.92))
                    .active(|style| style.scale(0.94))
                    .transition(theme::press_transition())
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
            }));

        let volume_row = div()
            .id("stage-volume-row")
            .w_full()
            .h(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id("stage-volume-mute")
                    .size(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .cursor_pointer()
                    .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.10)))
                    .child(themed_icon(
                        if volume <= 0.001 {
                            icon!(volume_x)
                        } else if volume < 0.5 {
                            icon!(volume_1)
                        } else {
                            icon!(volume_2)
                        },
                        14.0,
                        hsla(0.0, 0.0, 1.0, 0.72),
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
                    .as_ref()
                    .expect("stage volume slider must be initialized")
                    .render(volume, SliderStyle::stage_volume())
                    .flex_1()
                    .min_w(px(80.0))
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
            )
            .child(themed_icon(
                icon!(volume_2),
                14.0,
                hsla(0.0, 0.0, 1.0, 0.46),
            ));

        // Apple Music-style distribution: progress/time, playback controls, and volume are separate
        // rows. Slider hit-testing never shares a flex row with clock labels or transport buttons.
        div()
            .id("stage-bottom-dock")
            .w_full()
            .flex_none()
            .opacity(visibility)
            .transition(stage_chrome_fade_transition())
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .on_hover({
                let parent = parent.clone();
                move |hovered: &bool, _, cx| {
                    let _ = parent.update(cx, |app, _app_cx| {
                        let changed = app.stage_controls_hovered != *hovered;
                        app.stage_controls_hovered = *hovered;
                        if changed && app.stage_suppress_wake_until.is_none() {
                            app.stage_last_user_activity = Instant::now();
                        }
                    });
                }
            })
            .child(cached_stage_transport(self.transport.clone()))
            .child(transport_buttons)
            .child(volume_row)
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
        let fade_changed = self.fade.set_target(stage_chrome::target_visible(app));
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let animating = self.fade.advance(now);
        if animating && let Some(deadline) = self.fade.deadline() {
            window.request_invalidation_at(deadline, cx);
        }
        let visibility = if animating {
            self.fade.target_value()
        } else {
            self.fade.value()
        };
        if visibility <= 0.001 {
            return div().w_full().h(px(38.0)).into_any_element();
        }

        let parent = self.parent.clone();
        let hide_parent = parent.clone();
        let collapse_parent = parent.clone();
        let title = self.title.clone();

        div()
            .id("stage-titlebar-shell")
            .w_full()
            .h(px(38.0))
            .occlude()
            .opacity(visibility)
            .transition(stage_chrome_fade_transition())
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
                    .child({
                        let drag_region = div()
                            .id("stage-drag-region")
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
                            );
                        if visibility >= 0.1 {
                            drag_region.window_control_area(gpui::WindowControlArea::Drag)
                        } else {
                            drag_region
                        }
                    })
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
                                    .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                                        cx.stop_propagation();
                                        let _ = collapse_parent.update(cx, |app, app_cx| {
                                            app.close_stage(app_cx);
                                        });
                                    })
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
    ui_events: Entity<AppUiEventBridge>,
    _ui_subscription: Subscription,
}

impl StageTransportView {
    fn new(
        parent: WeakEntity<MusicApp>,
        engine: Option<Arc<AudioEngine>>,
        ui_events: Entity<AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let ui_subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            match *event {
                AppUiEvent::PlaybackStateChanged(state) => {
                    if this.playback_state != state {
                        this.playback_state = state;
                        cx.notify();
                    }
                }
                AppUiEvent::ProgressChanged { ratio, .. } => {
                    if option_ratio_changed(this.drag_progress_ratio, ratio, 0.0015) {
                        this.drag_progress_ratio = ratio;
                        cx.notify();
                    }
                }
            }
        });
        Self {
            parent,
            engine,
            stage_active: false,
            controls_visible: true,
            playback_state: PlaybackState::Paused,
            drag_progress_ratio: None,
            progress: None,
            ui_events,
            _ui_subscription: ui_subscription,
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
        let changed = engine_changed
            || self.stage_active != stage_active
            || self.controls_visible != controls_visible
            || self.playback_state != playback_state;

        if engine_changed {
            self.engine = app.engine.clone();
        }
        self.stage_active = stage_active;
        self.controls_visible = controls_visible;
        self.playback_state = playback_state;

        if let Some(progress) = &self.progress {
            let engine = self.engine.clone();
            let playback_state = self.playback_state;
            let stage_active = self.stage_active;
            let controls_visible = self.controls_visible;
            progress.update(cx, |progress, cx| {
                progress.sync(
                    engine,
                    playback_state,
                    stage_active,
                    controls_visible,
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
        let engine = self.engine.clone();
        let playback_state = self.playback_state;
        let stage_active = self.stage_active;
        let controls_visible = self.controls_visible;
        let drag_progress_ratio = self.drag_progress_ratio;
        let ui_events = self.ui_events.clone();
        let progress = cx.new(move |cx| {
            StageProgressView::new(
                parent,
                engine,
                playback_state,
                stage_active,
                controls_visible,
                drag_progress_ratio,
                ui_events,
                cx,
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
        let (_, live_position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((PlaybackState::Stopped, 0, 0), |engine| engine.progress());
        let position = self.drag_progress_ratio.map_or(live_position_ms, |ratio| {
            (duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64
        });

        if self.clock_should_run() {
            let remainder = live_position_ms % 1_000;
            let delay_ms =
                (1_000 - remainder).clamp(TRANSPORT_MIN_SLEEP_MS, TRANSPORT_MAX_SLEEP_MS);
            window.request_invalidation_at(Instant::now() + Duration::from_millis(delay_ms), cx);
        }

        div()
            .w_full()
            .h_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(cached_stage_progress(progress))
            .child(
                div()
                    .w_full()
                    .h(px(14.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_xs()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.62))
                    .child(format_time(position))
                    .child(format_remaining_time(position, duration_ms)),
            )
    }
}


struct StageProgressView {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    playback_state: PlaybackState,
    stage_active: bool,
    controls_visible: bool,
    drag_progress_ratio: Option<f32>,
    local_dragging: bool,
    transport_generation: u64,
    slider: Option<InteractiveSliderState>,
    ui_events: Entity<AppUiEventBridge>,
    last_preview_emit_at: Instant,
    _ui_subscription: Subscription,
}

impl StageProgressView {
    fn new(
        parent: WeakEntity<MusicApp>,
        engine: Option<Arc<AudioEngine>>,
        playback_state: PlaybackState,
        stage_active: bool,
        controls_visible: bool,
        drag_progress_ratio: Option<f32>,
        ui_events: Entity<AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let transport_generation = engine
            .as_ref()
            .map_or(0, |engine| engine.transport_generation());
        let ui_subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            match *event {
                AppUiEvent::PlaybackStateChanged(state) => {
                    if this.playback_state != state {
                        this.playback_state = state;
                        cx.notify();
                    }
                }
                AppUiEvent::ProgressChanged { ratio, .. } => {
                    if this.local_dragging && ratio.is_some() {
                        return;
                    }
                    if option_ratio_changed(this.drag_progress_ratio, ratio, 0.0015) {
                        this.drag_progress_ratio = ratio;
                        cx.notify();
                    }
                }
            }
        });
        Self {
            parent,
            engine,
            playback_state,
            stage_active,
            controls_visible,
            drag_progress_ratio,
            local_dragging: false,
            transport_generation,
            slider: None,
            ui_events,
            last_preview_emit_at: Instant::now() - app_ui_events::PROGRESS_PREVIEW_INTERVAL,
            _ui_subscription: ui_subscription,
        }
    }

    fn sync(
        &mut self,
        engine: Option<Arc<AudioEngine>>,
        playback_state: PlaybackState,
        stage_active: bool,
        controls_visible: bool,
        cx: &mut Context<Self>,
    ) {
        let engine_changed = match (&self.engine, &engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        if engine_changed {
            self.engine = engine;
        }
        let transport_generation = self
            .engine
            .as_ref()
            .map_or(0, |engine| engine.transport_generation());
        let changed = engine_changed
            || self.transport_generation != transport_generation
            || self.playback_state != playback_state
            || self.stage_active != stage_active
            || self.controls_visible != controls_visible;
        self.playback_state = playback_state;
        self.stage_active = stage_active;
        self.controls_visible = controls_visible;
        if changed {
            self.transport_generation = transport_generation;
            cx.notify();
        }
    }

    fn ensure_slider(&mut self, cx: &mut Context<Self>) {
        if self.slider.is_some() {
            return;
        }

        let parent = self.parent.clone();
        let press_parent = parent.clone();
        let commit_parent = parent;
        let this_press = cx.entity().downgrade();
        let this_drag = this_press.clone();
        let this_commit = this_press.clone();
        let press_events = self.ui_events.clone();
        let drag_events = self.ui_events.clone();

        self.slider = Some(InteractiveSliderState::new(
            "stage-progress-track",
            move |ratio, cx| {
                let ratio = ratio.clamp(0.0, 1.0);
                let result = this_press.update(cx, |this, cx| {
                    this.local_dragging = true;
                    this.drag_progress_ratio = Some(ratio);
                    this.last_preview_emit_at = Instant::now();
                    let duration_ms = this
                        .engine
                        .as_ref()
                        .map_or(0, |engine| engine.progress().2);
                    let position_ms = (duration_ms as f32 * ratio).round() as u64;
                    cx.notify();
                    AppUiEvent::ProgressChanged {
                        position_ms,
                        ratio: Some(ratio),
                    }
                });
                if let Ok(event) = result {
                    app_ui_events::emit_from_app(&press_events, event, cx);
                }
                let _ = press_parent.update(cx, |app, _app_cx| {
                    app.seeking = true;
                    app.stage_last_user_activity = Instant::now();
                });
            },
            move |ratio, cx| {
                let ratio = ratio.clamp(0.0, 1.0);
                let result = this_drag.update(cx, |this, cx| {
                    if this
                        .drag_progress_ratio
                        .is_some_and(|current| (current - ratio).abs() < 0.0015)
                    {
                        return None;
                    }

                    this.local_dragging = true;
                    this.drag_progress_ratio = Some(ratio);
                    let now = Instant::now();
                    let should_emit = now
                        .saturating_duration_since(this.last_preview_emit_at)
                        >= app_ui_events::PROGRESS_PREVIEW_INTERVAL;
                    cx.notify();

                    if !should_emit {
                        return None;
                    }
                    this.last_preview_emit_at = now;
                    let duration_ms = this
                        .engine
                        .as_ref()
                        .map_or(0, |engine| engine.progress().2);
                    let position_ms = (duration_ms as f32 * ratio).round() as u64;
                    Some(AppUiEvent::ProgressChanged {
                        position_ms,
                        ratio: Some(ratio),
                    })
                });

                if let Ok(Some(event)) = result {
                    app_ui_events::emit_from_app(&drag_events, event, cx);
                }
            },
            move |ratio, cx| {
                let ratio = ratio.clamp(0.0, 1.0);
                let _ = this_commit.update(cx, |this, cx| {
                    this.local_dragging = false;
                    this.drag_progress_ratio = None;
                    cx.notify();
                });
                let _ = commit_parent.update(cx, |app, app_cx| {
                    // One final seek at release. The ProgressChanged(ratio=None) emitted by
                    // seek_to_ratio closes the scrub lifecycle for lyrics and clocks.
                    app.seeking = false;
                    app.stage_last_user_activity = Instant::now();
                    app.seek_to_ratio(ratio, app_cx);
                    app.wake_stage_controls_immediately(app_cx);
                });
            },
        ));
    }

}

impl Render for StageProgressView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (engine_state, live_position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((PlaybackState::Stopped, 0, 0), |engine| engine.progress());
        let drag_progress_ratio = self.drag_progress_ratio;
        let should_tick = self.stage_active
            && self.controls_visible
            && self.playback_state == PlaybackState::Playing
            && engine_state == PlaybackState::Playing
            && drag_progress_ratio.is_none()
            && duration_ms > 0
            && !window.is_minimized();

        if should_tick {
            // Deadline invalidation is coalesced by GPUI per current View entity. No detached timer
            // survives pause/close, and no root MusicApp notification is involved.
            window.request_invalidation_at(
                Instant::now() + STAGE_PROGRESS_REFRESH_INTERVAL,
                cx,
            );
        }

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
            .w_full()
            .h_full()
            .into_any_element()
    }
}

fn cached_stage_transport(view: Entity<StageTransportView>) -> AnyView {
    AnyView::from(view)
        .cached(
            StyleRefinement::default()
                .w_full()
                .flex_none()
                .h(px(STAGE_TRANSPORT_HEIGHT)),
        )
        .reuse_on_window_refresh()
}

fn cached_stage_progress(view: Entity<StageProgressView>) -> AnyView {
    AnyView::from(view)
        .cached(
            StyleRefinement::default()
                .w_full()
                .flex_none()
                .h(px(STAGE_PROGRESS_HEIGHT)),
        )
        .reuse_on_window_refresh()
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
        .child(themed_icon(icon, 20.0, hsla(0.0, 0.0, 1.0, 0.85)))
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
        assert_eq!(stage_chrome::IDLE_TIMEOUT, Duration::from_secs(20));
    }
}
