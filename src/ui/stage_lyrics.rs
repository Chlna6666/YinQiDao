use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, BorrowAppContext as _, Context,
    Easing, ElementId, Entity, Global, IntoElement, ListAlignment, ListOffset, ListState, Render,
    SharedString, Timer, Transition, TransitionProperty, WeakEntity, Window, div, hsla, list,
    prelude::*, px,
};
use lucide_gpui::icon;

use crate::{
    audio::AudioEngine,
    lyrics::LyricLine,
    model::{PlaybackState, TrackId},
};

use super::{shell::MusicApp, theme::themed_icon};

const READING_MODE_DURATION: Duration = Duration::from_secs(3);
const LIST_OVERDRAW_PX: f32 = 180.0;
const LYRIC_ANCHOR_RATIO: f32 = 0.43;
const SCROLL_EASING_RATE: f32 = 12.5;
const SCROLL_SETTLE_PX: f32 = 0.30;
const TRANSPORT_IDLE_POLL: Duration = Duration::from_millis(500);
const TRANSPORT_MAX_SLEEP: u64 = 1_000;
const TRANSPORT_MIN_SLEEP: u64 = 8;

#[derive(Default)]
struct StageLyricsViewCache {
    view: Option<Entity<StageLyricsView>>,
}

impl Global for StageLyricsViewCache {}

#[derive(Clone)]
struct StageLyricWord {
    timestamp_ms: u64,
    text: SharedString,
}

#[derive(Clone)]
struct StageLyricLine {
    timestamp_ms: u64,
    text: SharedString,
    translation: Option<SharedString>,
    words: Arc<[StageLyricWord]>,
    enhanced_complete: bool,
    time_label: SharedString,
}

impl StageLyricLine {
    fn from_source(line: &LyricLine) -> Self {
        let words = line
            .words
            .iter()
            .map(|word| StageLyricWord {
                timestamp_ms: word.timestamp_ms,
                text: SharedString::from(word.text.clone()),
            })
            .collect::<Vec<_>>()
            .into();
        Self {
            timestamp_ms: line.timestamp_ms,
            text: SharedString::from(line.text.clone()),
            translation: line
                .translation
                .as_deref()
                .filter(|translation| !translation.trim().is_empty())
                .map(|translation| SharedString::from(translation.to_owned())),
            words,
            enhanced_complete: enhanced_words_cover_primary_text(line),
            time_label: SharedString::from(format_lyric_time(line.timestamp_ms)),
        }
    }
}

pub(super) fn view(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> Entity<StageLyricsView> {
    let parent = cx.entity().downgrade();
    let engine = app.engine.clone();
    let view = cx.update_default_global(|cache: &mut StageLyricsViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| StageLyricsView::new(parent, engine));
        cache.view = Some(view.clone());
        view
    });

    let stage_active = app.stage_open || app.stage_animating;
    view.update(cx, |view, cx| view.sync_from_app(app, stage_active, cx));
    view
}

pub(super) struct StageLyricsView {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    list_state: ListState,
    lines: Arc<[StageLyricLine]>,
    track_id: Option<TrackId>,
    source_ptr: usize,
    source_len: usize,
    position_ms: u64,
    playback_state: PlaybackState,
    active_index: Option<usize>,
    hovered_index: Option<usize>,
    motion_epoch: u64,
    reading_until: Option<Instant>,
    reading_epoch: u64,
    scroll_target: Option<usize>,
    last_scroll_frame: Option<Instant>,
    stage_active: bool,
    timer_started: bool,
}

impl StageLyricsView {
    fn new(parent: WeakEntity<MusicApp>, engine: Option<Arc<AudioEngine>>) -> Self {
        Self {
            parent,
            engine,
            list_state: ListState::new(0, ListAlignment::Top, px(LIST_OVERDRAW_PX)),
            lines: Arc::from(Vec::<StageLyricLine>::new()),
            track_id: None,
            source_ptr: 0,
            source_len: 0,
            position_ms: 0,
            playback_state: PlaybackState::Paused,
            active_index: None,
            hovered_index: None,
            motion_epoch: 0,
            reading_until: None,
            reading_epoch: 0,
            scroll_target: None,
            last_scroll_frame: None,
            stage_active: false,
            timer_started: false,
        }
    }

    fn sync_from_app(
        &mut self,
        app: &MusicApp,
        stage_active: bool,
        cx: &mut Context<Self>,
    ) {
        let engine_changed = match (&self.engine, &app.engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        if engine_changed {
            self.engine = app.engine.clone();
        }

        let track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
        let source = track_id
            .and_then(|id| app.lyrics.get(&id))
            .map_or(&[][..], |document| document.timed_lines());
        let source_ptr = source.as_ptr() as usize;
        let source_len = source.len();
        let source_changed = self.track_id != track_id
            || self.source_ptr != source_ptr
            || self.source_len != source_len;

        let mut changed = engine_changed;
        if source_changed {
            self.track_id = track_id;
            self.source_ptr = source_ptr;
            self.source_len = source_len;
            self.lines = source
                .iter()
                .map(StageLyricLine::from_source)
                .collect::<Vec<_>>()
                .into();
            self.list_state.reset(source_len);
            self.active_index = None;
            self.hovered_index = None;
            self.motion_epoch = self.motion_epoch.wrapping_add(1);
            self.reading_until = None;
            self.scroll_target = None;
            self.last_scroll_frame = None;
            changed = true;
        }

        let live_position_ms = self
            .engine
            .as_ref()
            .map_or(app.snapshot.position_ms, |engine| engine.progress().1);
        let position_ms = app.drag_progress_ratio.map_or(live_position_ms, |ratio| {
            (app.snapshot.duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64
        });
        let previous_word = if source_changed {
            None
        } else {
            self.active_word_index()
        };
        let position_changed = self.position_ms != position_ms;
        if position_changed {
            // Keep the hot transport sample locally, but do not invalidate the lyric view merely
            // because another 100 ms of audio elapsed. Rendering changes only at line/word edges.
            self.position_ms = position_ms;
        }
        if self.playback_state != app.snapshot.state {
            self.playback_state = app.snapshot.state;
            changed = true;
        }
        if self.stage_active != stage_active {
            self.stage_active = stage_active;
            changed = true;
        }

        let active_changed = self.update_active_index();
        let word_changed = position_changed
            && !source_changed
            && !active_changed
            && previous_word != self.active_word_index();
        if active_changed || word_changed {
            changed = true;
        }

        if source_changed
            && let Some(active) = self.active_index
        {
            // Start close to the current lyric even before the first variable-height measurement.
            // This avoids measuring every preceding row just to warm an offscreen Stage.
            self.list_state.scroll_to(ListOffset {
                item_ix: active.saturating_sub(2),
                offset_in_item: px(0.0),
            });
            self.scroll_target = Some(active);
        }

        if changed {
            cx.notify();
        }
    }

    fn update_active_index(&mut self) -> bool {
        let active = (!self.lines.is_empty()).then(|| {
            self.lines
                .partition_point(|line| line.timestamp_ms <= self.position_ms)
                .saturating_sub(1)
        });
        if self.active_index == active {
            return false;
        }
        self.active_index = active;
        self.hovered_index = None;
        self.motion_epoch = self.motion_epoch.wrapping_add(1);
        if !self.is_reading() {
            self.scroll_target = active;
            self.last_scroll_frame = None;
        }
        true
    }

    fn active_word_index(&self) -> Option<usize> {
        if self.is_reading() {
            return None;
        }
        let line = self.active_index.and_then(|index| self.lines.get(index))?;
        if !line.enhanced_complete {
            return None;
        }
        let count = line
            .words
            .partition_point(|word| word.timestamp_ms <= self.position_ms);
        count.checked_sub(1)
    }

    fn next_transport_delay(&self) -> Duration {
        if !self.stage_active || self.playback_state != PlaybackState::Playing {
            return TRANSPORT_IDLE_POLL;
        }
        let Some(engine) = &self.engine else {
            return TRANSPORT_IDLE_POLL;
        };
        let (_, position_ms, _) = engine.progress();
        let active = if self.lines.is_empty() {
            None
        } else {
            Some(
                self.lines
                    .partition_point(|line| line.timestamp_ms <= position_ms)
                    .saturating_sub(1),
            )
        };

        let mut next_timestamp = active
            .and_then(|index| self.lines.get(index + 1))
            .map(|line| line.timestamp_ms);
        if !self.is_reading()
            && let Some(line) = active.and_then(|index| self.lines.get(index))
            && line.enhanced_complete
            && let Some(word) = line.words.iter().find(|word| word.timestamp_ms > position_ms)
        {
            next_timestamp = Some(next_timestamp.map_or(word.timestamp_ms, |current| {
                current.min(word.timestamp_ms)
            }));
        }

        let delay_ms = next_timestamp
            .map(|timestamp| timestamp.saturating_sub(position_ms))
            .unwrap_or(TRANSPORT_MAX_SLEEP)
            .clamp(TRANSPORT_MIN_SLEEP, TRANSPORT_MAX_SLEEP);
        Duration::from_millis(delay_ms)
    }

    fn refresh_transport(&mut self, cx: &mut Context<Self>) {
        if !self.stage_active || self.playback_state != PlaybackState::Playing {
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let (_, position_ms, _) = engine.progress();
        if position_ms == self.position_ms {
            return;
        }

        let previous_word = self.active_word_index();
        self.position_ms = position_ms;
        let active_changed = self.update_active_index();
        let word_changed = !active_changed && previous_word != self.active_word_index();
        if active_changed || word_changed {
            // Text shaping and blur/list item invalidation happen only at semantic lyric boundaries;
            // the smooth vertical motion itself remains driven by request_animation_frame().
            cx.notify();
        }
    }

    fn ensure_transport_timer(&mut self, cx: &mut Context<Self>) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        cx.spawn(async move |this, cx| -> Result<()> {
            loop {
                let delay = match this.update(cx, |this, _cx| this.next_transport_delay()) {
                    Ok(delay) => delay,
                    Err(_) => break,
                };
                Timer::after(delay).await;
                if this
                    .update(cx, |this, cx| this.refresh_transport(cx))
                    .is_err()
                {
                    break;
                }
            }
            Ok(())
        })
        .detach();
    }

    #[inline]
    fn is_reading(&self) -> bool {
        self.reading_until.is_some_and(|until| until > Instant::now())
    }

    fn begin_reading_mode(&mut self, cx: &mut Context<Self>) {
        self.reading_epoch = self.reading_epoch.wrapping_add(1);
        let epoch = self.reading_epoch;
        self.reading_until = Some(Instant::now() + READING_MODE_DURATION);
        self.scroll_target = None;
        self.hovered_index = None;
        self.last_scroll_frame = None;
        cx.notify();

        cx.spawn(async move |this, cx| -> Result<()> {
            Timer::after(READING_MODE_DURATION).await;
            this.update(cx, |this, cx| {
                if this.reading_epoch != epoch {
                    return;
                }
                this.reading_until = None;
                this.scroll_target = this.active_index;
                this.last_scroll_frame = None;
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    fn advance_scroll(&mut self, window: &mut Window) {
        if !self.stage_active || self.is_reading() {
            self.last_scroll_frame = None;
            return;
        }
        let Some(target) = self.scroll_target else {
            self.last_scroll_frame = None;
            return;
        };

        let viewport = self.list_state.viewport_bounds();
        if f32::from(viewport.size.height) <= 0.5 {
            // The first active render may precede List's initial prepaint. Keep the wake local to
            // this entity so the next frame can use the measured viewport without invalidating Stage.
            window.request_animation_frame();
            return;
        }

        let Some(line_bounds) = self.list_state.bounds_for_item(target) else {
            let top = self.list_state.logical_scroll_top().item_ix;
            if target < top || target > top.saturating_add(8) {
                self.list_state.scroll_to(ListOffset {
                    item_ix: target.saturating_sub(2),
                    offset_in_item: px(0.0),
                });
            } else {
                self.list_state.scroll_to_reveal_item(target);
            }
            window.request_animation_frame();
            return;
        };

        let anchor_y = f32::from(viewport.origin.y)
            + f32::from(viewport.size.height) * LYRIC_ANCHOR_RATIO;
        let diff = f32::from(line_bounds.center().y) - anchor_y;
        if diff.abs() <= SCROLL_SETTLE_PX {
            self.scroll_target = None;
            self.last_scroll_frame = None;
            return;
        }

        let now = Instant::now();
        let dt = self
            .last_scroll_frame
            .map(|last| now.saturating_duration_since(last).as_secs_f32())
            .unwrap_or(1.0 / 120.0)
            .clamp(1.0 / 500.0, 0.05);
        self.last_scroll_frame = Some(now);
        let factor = 1.0 - (-SCROLL_EASING_RATE * dt).exp();
        self.list_state.scroll_by(px(diff * factor));
        // This call runs inside StageLyricsView::render, so this GPUI fork only invalidates this
        // entity on the next animation frame instead of waking MusicApp and the entire Stage.
        window.request_animation_frame();
    }
}

impl Render for StageLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_transport_timer(cx);

        if self.lines.is_empty() {
            return div()
                .id("stage-lyrics-view")
                .flex_1()
                .h_full()
                .min_w(px(0.0))
                .min_h(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(themed_icon(
                    icon!(music),
                    36.0,
                    hsla(0.0, 0.0, 1.0, 0.25),
                ))
                .child(
                    div()
                        .text_lg()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.50))
                        .child("暂无同步滚动歌词"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.30))
                        .child("支持内嵌 LRC 或联网自动检索"),
                );
        }

        if self.reading_until.is_some_and(|until| until <= Instant::now()) {
            self.reading_until = None;
            self.scroll_target = self.active_index;
        }
        self.advance_scroll(window);

        let active = self.active_index.unwrap_or(0);
        let reading_mode = self.is_reading();
        let scroll_animating = self.scroll_target.is_some() && !reading_mode;
        // Element blur captures an offscreen Scene per blurred row. Avoid rebuilding those captures
        // on every auto-scroll frame; restore the depth cue immediately after the row settles.
        let depth_blur_active = self.playback_state == PlaybackState::Playing
            && !reading_mode
            && !scroll_animating;
        let text_id = if depth_blur_active {
            "lyric-text-blur"
        } else {
            "lyric-text-direct"
        };
        let position_ms = self.position_ms;
        let motion_epoch = self.motion_epoch;
        let hovered_index = self.hovered_index;
        let lines = self.lines.clone();
        let view = cx.entity().downgrade();
        let parent = self.parent.clone();

        let lyrics = list(self.list_state.clone(), move |index, _window, _cx| {
            render_lyric_row(
                &lines[index],
                index,
                active,
                position_ms,
                reading_mode,
                depth_blur_active,
                text_id,
                hovered_index == Some(index),
                motion_epoch,
                view.clone(),
                parent.clone(),
            )
        })
        .size_full()
        .pt(px(96.0))
        .pb(px(112.0))
        .pr(px(8.0));

        div()
            .id("stage-lyrics-view")
            .relative()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .on_mouse_down(
                gpui::MouseButton::Left,
                {
                    let parent = self.parent.clone();
                    move |_, _, cx| {
                        let _ = parent.update(cx, |app, cx| {
                            app.wake_stage_controls_immediately(cx);
                        });
                    }
                },
            )
            .on_scroll_wheel(cx.listener(|this, _: &gpui::ScrollWheelEvent, _, cx| {
                this.begin_reading_mode(cx);
                let _ = this.parent.update(cx, |app, cx| app.wake_stage_controls(cx));
            }))
            .child(lyrics)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_lyric_row(
    line: &StageLyricLine,
    index: usize,
    active: usize,
    position_ms: u64,
    reading_mode: bool,
    depth_blur_active: bool,
    text_id: &'static str,
    hovered: bool,
    motion_epoch: u64,
    view: WeakEntity<StageLyricsView>,
    parent: WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let distance = index.abs_diff(active);
    let (alpha, blur_sigma) = lyric_focus_profile(distance, reading_mode, depth_blur_active);
    let timestamp = line.timestamp_ms;
    let weight = if index == active {
        gpui::FontWeight::BOLD
    } else if distance == 1 {
        gpui::FontWeight::SEMIBOLD
    } else {
        gpui::FontWeight::MEDIUM
    };
    let karaoke_active = index == active && !reading_mode;

    let mut text = div()
        .id(ElementId::named_usize(text_id, index))
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_1()
        .font_weight(weight)
        .child(stage_primary_lyric(line, position_ms, karaoke_active));

    if let Some(translation) = &line.translation {
        text = text.child(
            div()
                .w_full()
                .min_w(px(0.0))
                .text_size(px(17.0))
                .text_color(hsla(0.0, 0.0, 1.0, 0.72))
                .child(translation.clone()),
        );
    }

    if blur_sigma > 0.0 && !hovered {
        text = text.blur(px(blur_sigma));
    }
    text = text
        .opacity(if hovered { 1.0 } else { alpha })
        .transition(lyric_focus_transition());

    let text = if index == active && !reading_mode {
        let active_focus = Animation::from_spec(
            AnimationSpec::new(Duration::from_millis(150)).ease(Easing::OutCubic),
        )
        .with_property(AnimationProperty::opacity(0.80, 1.0));
        let animation_key = motion_epoch
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(index as u64);
        text.with_animation(
            ElementId::NamedInteger(
                SharedString::new_static("lyric-active-focus"),
                animation_key,
            ),
            active_focus,
            |element, _| element,
        )
        .into_any_element()
    } else {
        text.into_any_element()
    };

    let hover_enter = view.clone();
    let hover_leave = view.clone();
    let mut row = div()
        .id(ElementId::named_usize("lyric-line", index))
        .relative()
        .w_full()
        .min_w(px(0.0))
        .flex_none()
        .pl(px(16.0))
        .pr(px(104.0))
        .py(px(11.0))
        .mb(px(10.0))
        .cursor_pointer()
        .child(text)
        // Enter only on real pointer motion. A stationary pointer must not hand the badge to a
        // different row merely because automatic scrolling moved that row underneath it.
        .on_mouse_move(move |_: &gpui::MouseMoveEvent, _, cx| {
            let _ = hover_enter.update(cx, |this, cx| {
                if this.hovered_index != Some(index) {
                    this.hovered_index = Some(index);
                    cx.notify();
                }
            });
        })
        .on_hover(move |hovered: &bool, _, cx| {
            if *hovered {
                return;
            }
            let _ = hover_leave.update(cx, |this, cx| {
                if this.hovered_index == Some(index) {
                    this.hovered_index = None;
                    cx.notify();
                }
            });
        });

    if !reading_mode {
        row = row.child(
            div()
                .id(ElementId::named_usize("lyric-time", index))
                .absolute()
                .right(px(10.0))
                .top(px(13.0))
                .min_w(px(88.0))
                .px_2p5()
                .py_1()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .opacity(if hovered { 1.0 } else { 0.0 })
                .transition(
                    Transition::new(Duration::from_millis(80))
                        .ease(Easing::OutCubic)
                        .properties([TransitionProperty::Opacity]),
                )
                .bg(hsla(0.0, 0.0, 0.0, 0.28))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hsla(0.0, 0.0, 1.0, 0.92))
                .child(line.time_label.clone()),
        );
    }

    let local = view;
    row.on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
        cx.stop_propagation();
        let _ = local.update(cx, |this, cx| {
            this.reading_epoch = this.reading_epoch.wrapping_add(1);
            this.reading_until = None;
            this.hovered_index = None;
            this.position_ms = timestamp;
            this.active_index = Some(index);
            this.motion_epoch = this.motion_epoch.wrapping_add(1);
            this.scroll_target = Some(index);
            this.last_scroll_frame = None;
            cx.notify();
        });
        let _ = parent.update(cx, |app, cx| {
            app.seek_to_ms(timestamp, cx);
            app.wake_stage_controls_immediately(cx);
        });
    })
    .into_any_element()
}

fn lyric_focus_profile(
    distance: usize,
    reading_mode: bool,
    depth_blur_active: bool,
) -> (f32, f32) {
    if reading_mode {
        return (1.0, 0.0);
    }

    let alpha = match distance {
        0 => 1.0,
        1 => 0.56,
        2 => 0.42,
        3 => 0.32,
        _ => 0.26,
    };
    let blur_sigma = if depth_blur_active {
        match distance {
            0 => 0.0,
            1 => 0.40,
            2 => 0.80,
            3 => 1.15,
            4 => 1.40,
            _ => 0.0,
        }
    } else {
        0.0
    };
    (alpha, blur_sigma)
}

fn stage_primary_lyric(
    line: &StageLyricLine,
    position_ms: u64,
    karaoke_active: bool,
) -> gpui::AnyElement {
    if !karaoke_active || !line.enhanced_complete {
        return div()
            .w_full()
            .min_w(px(0.0))
            .text_size(px(28.0))
            .text_color(hsla(0.0, 0.0, 1.0, 1.0))
            .child(line.text.clone())
            .into_any_element();
    }

    let current_word = line
        .words
        .iter()
        .rposition(|word| word.timestamp_ms <= position_ms);
    let mut row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_wrap()
        .items_baseline()
        .text_size(px(28.0));

    for (index, word) in line.words.iter().enumerate() {
        let alpha = match current_word {
            Some(current) if index < current => 0.88,
            Some(current) if index == current => 1.0,
            Some(_) => 0.42,
            None if index == 0 => 0.90,
            None => 0.42,
        };
        row = row.child(
            div()
                .flex_none()
                .font_weight(if current_word == Some(index) || (current_word.is_none() && index == 0)
                {
                    gpui::FontWeight::BOLD
                } else {
                    gpui::FontWeight::SEMIBOLD
                })
                .text_color(hsla(0.0, 0.0, 1.0, alpha))
                .child(word.text.clone()),
        );
    }
    row.into_any_element()
}

fn enhanced_words_cover_primary_text(line: &LyricLine) -> bool {
    if line.words.is_empty() || line.text.is_empty() {
        return false;
    }
    let mut remaining = line.text.as_str();
    for word in line.words.iter() {
        let Some(rest) = remaining.strip_prefix(word.text.as_str()) else {
            return false;
        };
        remaining = rest;
    }
    remaining.is_empty()
}

fn format_lyric_time(ms: u64) -> String {
    let total_secs = ms / 1_000;
    let millis = ms % 1_000;
    let hours = total_secs / 3_600;
    let minutes = (total_secs / 60) % 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

fn lyric_focus_transition() -> Transition {
    Transition::new(Duration::from_millis(120))
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::LyricWord;

    #[test]
    fn precise_lyric_time_keeps_subsecond_timing() {
        assert_eq!(format_lyric_time(62_345), "01:02.345");
        assert_eq!(format_lyric_time(3_662_007), "01:01:02.007");
    }

    #[test]
    fn lyric_depth_profile_keeps_the_active_line_unambiguous() {
        assert_eq!(lyric_focus_profile(0, false, true), (1.0, 0.0));
        assert_eq!(lyric_focus_profile(1, false, true), (0.56, 0.40));
        assert_eq!(lyric_focus_profile(3, false, true), (0.32, 1.15));
        assert_eq!(lyric_focus_profile(5, false, true), (0.26, 0.0));
        assert_eq!(lyric_focus_profile(2, false, false), (0.42, 0.0));
        assert_eq!(lyric_focus_profile(2, true, true), (1.0, 0.0));
    }

    #[test]
    fn incomplete_enhanced_lrc_falls_back_to_full_line() {
        let complete = LyricLine {
            timestamp_ms: 1_000,
            text: "你好 世界".into(),
            translation: None,
            words: Arc::from([
                LyricWord {
                    timestamp_ms: 1_000,
                    text: "你好 ".into(),
                },
                LyricWord {
                    timestamp_ms: 1_500,
                    text: "世界".into(),
                },
            ]),
        };
        assert!(enhanced_words_cover_primary_text(&complete));

        let incomplete = LyricLine {
            timestamp_ms: 1_000,
            text: "前缀你好".into(),
            translation: None,
            words: Arc::from([LyricWord {
                timestamp_ms: 1_200,
                text: "你好".into(),
            }]),
        };
        assert!(!enhanced_words_cover_primary_text(&incomplete));
    }
}
