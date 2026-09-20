use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Context, EncodedImageBytes, Entity, ImageFormat, IntoElement, ObjectFit, Render, Subscription,
    StatefulInteractiveElement as _, WeakEntity, Window, div, hsla, img, linear_color_stop,
    linear_gradient, prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    audio::{AudioEngine, PlayerCommand},
    model::{AppPage, LibraryTab, PlaybackState, RepeatMode},
};

use super::{
    app_ui_events::{self, AppUiEvent, AppUiEventBridge},
    components::{SliderStyle, slider::InteractiveSliderState},
    shell::MusicApp,
    theme::{
        self, ACCENT_RED, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, TEXT_WHITE,
        elegant_gradient_for, press_transition, themed_icon,
    },
};

const MINI_PROGRESS_REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const MINI_TIME_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const NOW_PLAYING_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

fn mini_clock_visible(parent: &WeakEntity<MusicApp>, cx: &gpui::App) -> bool {
    parent
        .read_with(cx, |app, _| !(app.stage_open || app.stage_animating))
        .unwrap_or(false)
}

fn mini_clock_should_run(
    parent: &WeakEntity<MusicApp>,
    engine: &Option<Arc<AudioEngine>>,
    cx: &gpui::App,
) -> bool {
    mini_clock_visible(parent, cx)
        && engine
            .as_ref()
            .is_some_and(|engine| engine.progress().0 == PlaybackState::Playing)
}

pub(super) struct PlaybackProgress {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    transport_generation: u64,
    playback_state: PlaybackState,
    visible: bool,
    drag_ratio_bits: Option<u32>,
    local_drag_ratio: Option<f32>,
    slider: Option<InteractiveSliderState>,
    ui_events: Entity<AppUiEventBridge>,
    last_preview_emit_at: Instant,
    _ui_subscription: Subscription,
}

impl PlaybackProgress {
    pub(super) fn new(
        parent: WeakEntity<MusicApp>,
        engine: Option<Arc<AudioEngine>>,
        ui_events: Entity<AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let transport_generation = engine
            .as_ref()
            .map_or(0, |engine| engine.transport_generation());
        let playback_state = engine
            .as_ref()
            .map_or(PlaybackState::Stopped, |engine| engine.progress().0);
        let ui_subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            match *event {
                AppUiEvent::PlaybackStateChanged(state) => {
                    if this.playback_state != state {
                        this.playback_state = state;
                        cx.notify();
                    }
                }
                AppUiEvent::ProgressChanged { ratio, .. } => {
                    let next = ratio.map(f32::to_bits);
                    if this.drag_ratio_bits != next {
                        this.drag_ratio_bits = next;
                        if this.local_drag_ratio.is_none() || ratio.is_none() {
                            cx.notify();
                        }
                    }
                }
            }
        });
        Self {
            parent,
            engine,
            transport_generation,
            playback_state,
            visible: true,
            drag_ratio_bits: None,
            local_drag_ratio: None,
            slider: None,
            ui_events,
            last_preview_emit_at: Instant::now() - app_ui_events::PROGRESS_PREVIEW_INTERVAL,
            _ui_subscription: ui_subscription,
        }
    }

    fn ensure_slider(&mut self, cx: &mut Context<Self>) {
        if self.slider.is_some() {
            return;
        }

        let parent = self.parent.clone();
        let this = cx.weak_entity();
        let ui_events = self.ui_events.clone();
        self.slider = Some(InteractiveSliderState::new(
            "mini-progress-track",
            {
                let parent = parent.clone();
                let this = this.clone();
                move |ratio, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.local_drag_ratio = None;
                        cx.notify();
                    });
                    let _ = parent.update(cx, |app, app_cx| {
                        app.seek_to_ratio(ratio, app_cx);
                    });
                }
            },
            {
                let this = this.clone();
                let ui_events = ui_events.clone();
                move |ratio, cx| {
                    let ratio = ratio.clamp(0.0, 1.0);
                    let event = this
                        .update(cx, |this, cx| {
                            if this
                                .local_drag_ratio
                                .is_some_and(|current| (current - ratio).abs() < 0.001)
                            {
                                return None;
                            }
                            this.local_drag_ratio = Some(ratio);
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
                        })
                        .ok()
                        .flatten();
                    if let Some(event) = event {
                        app_ui_events::emit_from_app(&ui_events, event, cx);
                    }
                }
            },
            {
                let parent = parent.clone();
                let this = this.clone();
                move |ratio, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.local_drag_ratio = None;
                        cx.notify();
                    });
                    let _ = parent.update(cx, |app, app_cx| {
                        app.seek_to_ratio(ratio, app_cx);
                    });
                }
            },
        ));
    }

    pub(super) fn sync(
        &mut self,
        engine: Option<Arc<AudioEngine>>,
        playback_state: PlaybackState,
        visible: bool,
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
            || self.visible != visible;

        if changed {
            self.transport_generation = transport_generation;
            self.playback_state = playback_state;
            self.visible = visible;
            cx.notify();
        }
    }
}

impl Render for PlaybackProgress {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (engine_state, position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((PlaybackState::Stopped, 0, 0), |engine| engine.progress());
        let shared_drag_ratio = self.drag_ratio_bits.map(f32::from_bits);
        let drag_ratio = self.local_drag_ratio.or(shared_drag_ratio);

        let should_tick = self.visible
            && self.playback_state == PlaybackState::Playing
            && engine_state == PlaybackState::Playing
            && drag_ratio.is_none()
            && duration_ms > 0;
        if should_tick && !_window.is_minimized() {
            // GPUI owns the deadline and notifies only this cached View entity. This replaces the
            // old detached Timer loop, so transport state changes and view lifetime stay ordered by
            // the window event loop instead of racing a background task.
            _window.request_invalidation_at(Instant::now() + MINI_PROGRESS_REFRESH_INTERVAL, cx);
        }

        let ratio = drag_ratio.unwrap_or_else(|| {
            if duration_ms == 0 {
                0.0
            } else {
                (position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
            }
        });

        self.ensure_slider(cx);
        let slider = self
            .slider
            .as_ref()
            .expect("mini progress slider must be initialized");

        // A multi-minute renderer animation keeps the entire window in PresentationAnimation
        // mode for the whole song. At high refresh rates that still generates/presents frames
        // continuously. Sample the atomic engine clock in this cached child instead.
        slider
            .render(ratio, SliderStyle::mini_progress())
            .w_full()
            .into_any_element()
    }
}

pub(super) struct PlaybackTime {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    preview_position_ms: Option<u64>,
    _ui_subscription: Subscription,
}

impl PlaybackTime {
    pub(super) fn new(
        parent: WeakEntity<MusicApp>,
        engine: Option<Arc<AudioEngine>>,
        ui_events: Entity<AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let ui_subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            match *event {
                AppUiEvent::PlaybackStateChanged(_) => cx.notify(),
                AppUiEvent::ProgressChanged { position_ms, ratio } => {
                    let next = ratio.map(|_| position_ms);
                    if this.preview_position_ms != next {
                        this.preview_position_ms = next;
                        cx.notify();
                    } else if ratio.is_none() {
                        cx.notify();
                    }
                }
            }
        });
        Self {
            parent,
            engine,
            preview_position_ms: None,
            _ui_subscription: ui_subscription,
        }
    }

    pub(super) fn sync(&mut self, engine: Option<Arc<AudioEngine>>, cx: &mut Context<Self>) {
        let changed = match (&self.engine, &engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        if changed {
            self.engine = engine;
            cx.notify();
        }
    }
}

impl Render for PlaybackTime {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (_, position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((PlaybackState::Stopped, 0, 0), |engine| engine.progress());
        let preview_position_ms = self.preview_position_ms;
        if preview_position_ms.is_none()
            && mini_clock_should_run(&self.parent, &self.engine, cx)
            && !window.is_minimized()
        {
            let remainder = position_ms % 1_000;
            let delay = Duration::from_millis((1_000 - remainder).max(16));
            window.request_invalidation_at(Instant::now() + delay.min(MINI_TIME_REFRESH_INTERVAL), cx);
        }
        let display_position = preview_position_ms.unwrap_or(position_ms);

        div()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(TEXT_TERTIARY)
            .child(theme::format_time(display_position))
            .child("/")
            .child(theme::format_time(duration_ms))
    }
}


fn mini_cover_element(track_id: Option<i64>, artwork: Option<Arc<[u8]>>) -> impl IntoElement {
    if let Some(bytes) = artwork {
        return img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(46.0))
            .rounded_lg()
            .object_fit(ObjectFit::Cover)
            .into_any_element();
    }

    let id = track_id.unwrap_or(0);
    let (c1, c2) = elegant_gradient_for(id);
    div()
        .size(px(46.0))
        .rounded_lg()
        .bg(linear_gradient(
            135.0,
            linear_color_stop(c1, 0.0),
            linear_color_stop(c2, 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .child(themed_icon(icon!(disc_3), 22.0, hsla(0.0, 0.0, 1.0, 0.85)))
        .into_any_element()
}

#[allow(dead_code)]
pub struct NowPlaying {
    engine: Option<Arc<AudioEngine>>,
    dynamic_blur: bool,
    artwork: Option<Arc<[u8]>>,
}

#[allow(dead_code)]
impl NowPlaying {
    pub fn new(
        engine: Option<Arc<AudioEngine>>,
        dynamic_blur: bool,
        artwork: Option<Arc<[u8]>>,
    ) -> Self {
        Self {
            engine,
            dynamic_blur,
            artwork,
        }
    }
}

impl Render for NowPlaying {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.progress().0 == PlaybackState::Playing)
            && !window.is_minimized()
        {
            window.request_invalidation_at(Instant::now() + NOW_PLAYING_REFRESH_INTERVAL, cx);
        }

        let snapshot = self
            .engine
            .as_ref()
            .map_or_else(crate::model::PlayerSnapshot::default, |engine| {
                engine.snapshot()
            });
        let track = snapshot.current_track.as_ref();
        let title = track.map_or("等待播放", |track| track.title.as_str());
        let artist = track.map_or("从音栖岛歌库选择歌曲", |track| {
            track.artist.as_str()
        });
        let artwork = self.artwork.clone();
        let bg = if self.dynamic_blur {
            rgb(0x11131c)
        } else {
            rgb(0x0e0f16)
        };

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(bg)
            .text_color(TEXT_WHITE)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_5()
                    .child(if let Some(bytes) = artwork {
                        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
                            .size(px(280.0))
                            .rounded_2xl()
                            .object_fit(ObjectFit::Cover)
                            .into_any_element()
                    } else {
                        let (c1, c2) = elegant_gradient_for(track.map_or(0, |track| track.id));
                        div()
                            .size(px(280.0))
                            .rounded_2xl()
                            .bg(linear_gradient(
                                135.0,
                                linear_color_stop(c1, 0.0),
                                linear_color_stop(c2, 1.0),
                            ))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(themed_icon(icon!(disc_3), 96.0, hsla(0.0, 0.0, 1.0, 0.75)))
                            .into_any_element()
                    })
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(title.to_owned()),
                    )
                    .child(
                        div()
                            .text_base()
                            .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                            .child(artist.to_owned()),
                    ),
            )
    }
}
