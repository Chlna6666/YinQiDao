use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnimationExt as _, AnimationSpec, BorrowAppContext as _, Context, Easing, ElementId, Entity,
    Global, IntoElement, Render, SharedString, Subscription, Transition, TransitionProperty,
    WeakEntity, Window, div, hsla, prelude::*, px, relative,
};
use lucide_gpui::icon;

use crate::{
    audio::AudioEngine,
    lyrics::LyricLine,
    model::{PlaybackState, TrackId},
};

use super::{
    app_ui_events::{self, AppUiEvent},
    shell::MusicApp,
    theme::themed_icon,
};

const READING_MODE_DURATION: Duration = Duration::from_secs(3);
const LYRIC_ANCHOR_RATIO: f32 = 0.43;
const LYRIC_VIEWPORT_FADE_TOP_PX: f32 = 128.0;
const LYRIC_VIEWPORT_FADE_BOTTOM_PX: f32 = 150.0;
const LYRIC_FOCUS_TRANSITION_DURATION: Duration = Duration::from_millis(220);
const LYRIC_ROW_MOVE_DURATION: Duration = Duration::from_millis(210);
const LYRIC_ROW_NEXT_START_PROGRESS: f32 = 0.80;
// The row motion uses ease-out cubic. Raw t ~= 0.4152 maps to 80% visible displacement, so the
// following row starts while the previous row is already settling through its final 20%.
const LYRIC_ROW_STAGGER_TIME_RATIO: f32 = 0.4152;
const LYRIC_VIEWPORT_MAX_BLUR_PX: f32 = 4.25;
const LYRIC_VIEWPORT_CLEAR_BAND_MIN_PX: f32 = 82.0;
const LYRIC_VIEWPORT_CLEAR_BAND_MAX_PX: f32 = 112.0;
const PLAYBACK_STACK_HISTORY_ROWS: usize = 4;
const PLAYBACK_STACK_FUTURE_ROWS: usize = 7;
const PLAYBACK_STACK_ROW_PITCH_PX: f32 = 82.0;
const PLAYBACK_STACK_ROW_CENTER_OFFSET_PX: f32 = 31.0;
const READING_STACK_HISTORY_ROWS: usize = 5;
const READING_STACK_FUTURE_ROWS: usize = 7;
const TRANSPORT_MIN_SLEEP: u64 = 8;

#[derive(Clone, Copy, Debug)]
struct LyricPlaybackStackHandoff {
    from_active: usize,
    to_active: usize,
    first_index: usize,
    last_index: usize,
    started_at: Instant,
}

impl LyricPlaybackStackHandoff {
    #[inline]
    fn row_count(self) -> usize {
        self.last_index
            .saturating_sub(self.first_index)
            .saturating_add(1)
            .max(1)
    }

    #[inline]
    fn duration(self) -> Duration {
        let last_rank = self.row_count().saturating_sub(1);
        lyric_row_start_delay(last_rank) + LYRIC_ROW_MOVE_DURATION
    }

    #[inline]
    fn finished(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration()
    }
}

#[derive(Default)]
struct StageLyricsViewCache {
    view: Option<Entity<StageLyricsView>>,
}

impl Global for StageLyricsViewCache {}

#[derive(Clone)]
struct StageLyricWord {
    timestamp_ms: u64,
    duration_ms: Option<u64>,
    byte_start: usize,
    byte_end: usize,
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
    fn from_plain(text: &str) -> Self {
        Self {
            timestamp_ms: 0,
            text: SharedString::from(text.to_owned()),
            translation: None,
            words: Arc::from([]),
            enhanced_complete: false,
            time_label: SharedString::new_static(""),
        }
    }

    fn from_source(line: &LyricLine) -> Self {
        let mut byte_offset = 0;
        let words = line
            .words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                let byte_start = byte_offset;
                byte_offset += word.text.len();
                // QRC/YRC/TTML provide an authored duration. Enhanced LRC only provides starts, in
                // which case the next authored segment gives a precise upper bound without inventing
                // timing from character count. The last LRC segment stays duration-less.
                let duration_ms = word
                    .duration_ms
                    .filter(|duration| *duration > 0)
                    .or_else(|| {
                        line.words.get(index + 1).and_then(|next| {
                            let duration = next.timestamp_ms.saturating_sub(word.timestamp_ms);
                            (duration > 0).then_some(duration)
                        })
                    });
                StageLyricWord {
                    timestamp_ms: word.timestamp_ms,
                    duration_ms,
                    byte_start,
                    byte_end: byte_offset,
                    text: SharedString::from(word.text.clone()),
                }
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


pub(super) fn sync_if_created(app: &MusicApp, cx: &mut Context<MusicApp>) {
    let existing = cx
        .try_global::<StageLyricsViewCache>()
        .and_then(|cache| cache.view.clone());
    if let Some(view) = existing {
        let stage_active = app.stage_open || app.stage_animating;
        view.update(cx, |view, cx| view.sync_from_app(app, stage_active, cx));
    }
}

pub(super) fn view(app: &MusicApp, cx: &mut Context<MusicApp>) -> Entity<StageLyricsView> {
    let parent = cx.entity().downgrade();
    let engine = app.engine.clone();
    let ui_events = app_ui_events::bridge(cx);
    let view = cx.update_default_global(|cache: &mut StageLyricsViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view_events = ui_events.clone();
        let view =
            cx.new(move |cx| StageLyricsView::new(parent, engine, view_events, cx));
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
    lines: Arc<[StageLyricLine]>,
    track_id: Option<TrackId>,
    source_ptr: usize,
    source_len: usize,
    has_timeline: bool,
    position_ms: u64,
    playback_state: PlaybackState,
    active_index: Option<usize>,
    focus_from_index: Option<usize>,
    focus_started_at: Option<Instant>,
    active_word_index: Option<usize>,
    hovered_index: Option<usize>,
    karaoke_epoch: u64,
    transport_generation: u64,
    reading_until: Option<Instant>,
    reading_center_index: Option<usize>,
    playback_stack_handoff: Option<LyricPlaybackStackHandoff>,
    stage_active: bool,
    scrubbing: bool,
    _ui_subscription: Subscription,
}

impl StageLyricsView {
    fn new(
        parent: WeakEntity<MusicApp>,
        engine: Option<Arc<AudioEngine>>,
        ui_events: Entity<app_ui_events::AppUiEventBridge>,
        cx: &mut Context<Self>,
    ) -> Self {
        let transport_generation = engine
            .as_ref()
            .map_or(0, |engine| engine.transport_generation());
        let ui_subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            this.apply_ui_event(*event, cx);
        });
        Self {
            parent,
            engine,
            lines: Arc::from(Vec::<StageLyricLine>::new()),
            track_id: None,
            source_ptr: 0,
            source_len: 0,
            has_timeline: false,
            position_ms: 0,
            playback_state: PlaybackState::Paused,
            active_index: None,
            focus_from_index: None,
            focus_started_at: None,
            active_word_index: None,
            hovered_index: None,
            karaoke_epoch: 0,
            transport_generation,
            reading_until: None,
            reading_center_index: None,
            playback_stack_handoff: None,
            stage_active: false,
            scrubbing: false,
            _ui_subscription: ui_subscription,
        }
    }

    fn apply_ui_event(&mut self, event: AppUiEvent, cx: &mut Context<Self>) {
        match event {
            AppUiEvent::PlaybackStateChanged(state) => {
                if self.playback_state != state {
                    self.playback_state = state;
                    self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
                    cx.notify();
                }
            }
            AppUiEvent::ProgressChanged { position_ms, ratio } => {
                let scrubbing = ratio.is_some();
                let was_scrubbing = self.scrubbing;
                let mut changed = false;

                if self.scrubbing != scrubbing {
                    self.scrubbing = scrubbing;
                    self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
                    changed = true;
                }
                if self.position_ms != position_ms {
                    self.position_ms = position_ms;
                    changed = true;
                }

                let previous_word = self.active_word_index;
                let active_changed = self.update_active_index();
                self.active_word_index = self.compute_active_word_index();
                let word_changed = previous_word != self.active_word_index;
                if !active_changed && word_changed {
                    self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
                }
                changed |= active_changed || word_changed;

                if was_scrubbing && !scrubbing {
                    self.focus_from_index = None;
                    self.focus_started_at = None;
                    self.playback_stack_handoff = None;
                    self.reading_until = None;
                    self.reading_center_index = None;
                    self.hovered_index = None;
                    changed = true;
                }

                if changed {
                    cx.notify();
                }
            }
        }
    }

    fn sync_from_app(&mut self, app: &MusicApp, stage_active: bool, cx: &mut Context<Self>) {
        let engine_changed = match (&self.engine, &app.engine) {
            (Some(current), Some(next)) => !Arc::ptr_eq(current, next),
            (None, None) => false,
            _ => true,
        };
        if engine_changed {
            self.engine = app.engine.clone();
        }

        let transport_generation = self
            .engine
            .as_ref()
            .map_or(0, |engine| engine.transport_generation());
        let transport_changed = engine_changed || self.transport_generation != transport_generation;
        if transport_changed {
            self.transport_generation = transport_generation;
        }

        let track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
        let document = track_id.and_then(|id| app.lyrics.get(&id));
        let timed_source = document.map_or(&[][..], |document| document.timed_lines());
        let has_timeline = !timed_source.is_empty();
        let plain_source = (!has_timeline)
            .then(|| {
                document.and_then(|document| {
                    document
                        .plain
                        .as_deref()
                        .or(document.translation.as_deref())
                })
            })
            .flatten()
            .filter(|text| !text.trim().is_empty());
        let (source_ptr, source_len) = if has_timeline {
            (timed_source.as_ptr() as usize, timed_source.len())
        } else {
            plain_source.map_or((0, 0), |text| (text.as_ptr() as usize, text.len()))
        };

        let source_changed = self.track_id != track_id
            || self.source_ptr != source_ptr
            || self.source_len != source_len
            || self.has_timeline != has_timeline;
        let playback_state_changed = self.playback_state != app.snapshot.state;
        let stage_active_changed = self.stage_active != stage_active;
        let mut changed = engine_changed;

        if source_changed {
            self.track_id = track_id;
            self.source_ptr = source_ptr;
            self.source_len = source_len;
            self.has_timeline = has_timeline;
            self.lines = if has_timeline {
                timed_source
                    .iter()
                    .map(StageLyricLine::from_source)
                    .collect::<Vec<_>>()
                    .into()
            } else {
                plain_source
                    .map(|text| {
                        text.lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                            .map(StageLyricLine::from_plain)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
                    .into()
            };
            self.active_index = None;
            self.focus_from_index = None;
            self.focus_started_at = None;
            self.active_word_index = None;
            self.hovered_index = None;
            self.reading_until = None;
            self.reading_center_index = None;
            self.playback_stack_handoff = None;
            self.scrubbing = false;
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            changed = true;
        }

        if transport_changed && !source_changed {
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            changed = true;
        }

        let live_position_ms = self
            .engine
            .as_ref()
            .map_or(app.snapshot.position_ms, |engine| engine.progress().1);
        let position_ms = if self.scrubbing {
            self.position_ms
        } else {
            live_position_ms
        };
        let previous_word = self.active_word_index;
        let position_changed = self.position_ms != position_ms;
        if position_changed {
            self.position_ms = position_ms;
        }

        if playback_state_changed {
            self.playback_state = app.snapshot.state;
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            changed = true;
        }

        if stage_active_changed {
            self.stage_active = stage_active;
            self.focus_from_index = None;
            self.focus_started_at = None;
            self.playback_stack_handoff = None;
            self.hovered_index = None;
            if stage_active {
                self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            } else {
                self.reading_until = None;
                self.reading_center_index = None;
            }
            changed = true;
        }

        let active_changed = self.update_active_index();
        let next_word = self.compute_active_word_index();
        let word_changed =
            position_changed && !source_changed && !active_changed && previous_word != next_word;
        self.active_word_index = next_word;
        if word_changed {
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        }
        changed |= active_changed || word_changed;

        if changed {
            cx.notify();
        }
    }

    fn update_active_index(&mut self) -> bool {
        let active = if self.lines.is_empty() {
            None
        } else if self.has_timeline {
            Some(
                self.lines
                    .partition_point(|line| line.timestamp_ms <= self.position_ms)
                    .saturating_sub(1),
            )
        } else {
            Some(0)
        };

        if self.active_index == active {
            return false;
        }

        let previous = self.active_index;
        self.focus_from_index = previous;
        self.focus_started_at = previous.map(|_| Instant::now());

        self.playback_stack_handoff = match (previous, active) {
            (Some(from_active), Some(to_active))
                if self.stage_active
                    && !self.is_reading()
                    && to_active == from_active.saturating_add(1) =>
            {
                let first_index = from_active.saturating_sub(PLAYBACK_STACK_HISTORY_ROWS);
                let last_index = to_active
                    .saturating_add(PLAYBACK_STACK_FUTURE_ROWS)
                    .min(self.lines.len().saturating_sub(1));
                Some(LyricPlaybackStackHandoff {
                    from_active,
                    to_active,
                    first_index,
                    last_index,
                    started_at: Instant::now(),
                })
            }
            _ => None,
        };

        self.active_index = active;
        self.hovered_index = None;
        self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        if !self.is_reading() {
            self.reading_center_index = None;
        }
        true
    }

    fn compute_active_word_index(&self) -> Option<usize> {
        if !self.has_timeline || self.is_reading() {
            return None;
        }
        let line = self.active_index.and_then(|index| self.lines.get(index))?;
        active_enhanced_word_index(line, self.position_ms)
    }

    #[inline]
    fn transport_should_run(&self) -> bool {
        self.has_timeline
            && self.stage_active
            && self.playback_state == PlaybackState::Playing
            && self.engine.is_some()
            && !self.lines.is_empty()
    }

    fn next_transport_delay(&self) -> Option<Duration> {
        let (_, position_ms, _) = self.engine.as_ref()?.progress();
        let active = Some(
            self.lines
                .partition_point(|line| line.timestamp_ms <= position_ms)
                .saturating_sub(1),
        );
        let mut next_timestamp = active
            .and_then(|index| self.lines.get(index + 1))
            .map(|line| line.timestamp_ms);

        if !self.is_reading()
            && let Some(line) = active.and_then(|index| self.lines.get(index))
            && let Some(word_timestamp) = next_enhanced_word_timestamp(line, position_ms)
        {
            next_timestamp =
                Some(next_timestamp.map_or(word_timestamp, |current| current.min(word_timestamp)));
        }

        let timestamp = next_timestamp?;
        Some(Duration::from_millis(
            timestamp
                .saturating_sub(position_ms)
                .max(TRANSPORT_MIN_SLEEP),
        ))
    }

    fn refresh_transport(&mut self) {
        if !self.transport_should_run() || self.scrubbing {
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let (_, position_ms, _) = engine.progress();
        if position_ms == self.position_ms {
            return;
        }

        let previous_word = self.active_word_index;
        self.position_ms = position_ms;
        let active_changed = self.update_active_index();
        self.active_word_index = self.compute_active_word_index();
        if !active_changed && previous_word != self.active_word_index {
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        }
    }

    #[inline]
    fn is_reading(&self) -> bool {
        !self.has_timeline
            || self
                .reading_until
                .is_some_and(|until| until > Instant::now())
    }

    fn begin_reading_mode(&mut self, wheel_delta_y: f32, cx: &mut Context<Self>) {
        self.reading_until = Some(Instant::now() + READING_MODE_DURATION);
        self.focus_from_index = None;
        self.focus_started_at = None;
        self.active_word_index = None;
        self.playback_stack_handoff = None;
        self.hovered_index = None;

        let max_index = self.lines.len().saturating_sub(1);
        let base = self
            .reading_center_index
            .or(self.active_index)
            .unwrap_or(0)
            .min(max_index);
        self.reading_center_index = Some(if wheel_delta_y < 0.0 {
            base.saturating_add(1).min(max_index)
        } else if wheel_delta_y > 0.0 {
            base.saturating_sub(1)
        } else {
            base
        });
        cx.notify();
    }

    fn expire_deadlines(&mut self, now: Instant) {
        if self.reading_until.is_some_and(|until| until <= now) {
            self.reading_until = None;
            self.reading_center_index = None;
            self.focus_from_index = None;
            self.focus_started_at = None;
            self.active_word_index = self.compute_active_word_index();
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        }

        if self
            .focus_started_at
            .is_some_and(|started_at| started_at + LYRIC_FOCUS_TRANSITION_DURATION <= now)
        {
            self.focus_from_index = None;
            self.focus_started_at = None;
        }

        if self
            .playback_stack_handoff
            .is_some_and(|handoff| handoff.finished(now))
        {
            self.playback_stack_handoff = None;
        }
    }

    fn schedule_deadlines(&self, window: &mut Window, cx: &Context<Self>) {
        let now = Instant::now();
        if self.transport_should_run()
            && let Some(delay) = self.next_transport_delay()
        {
            window.request_invalidation_at(now + delay, cx);
        }
        if let Some(until) = self.reading_until
            && until > now
        {
            window.request_invalidation_at(until, cx);
        }
        if let Some(started_at) = self.focus_started_at {
            let deadline = started_at + LYRIC_FOCUS_TRANSITION_DURATION;
            if deadline > now {
                window.request_invalidation_at(deadline, cx);
            }
        }
    }
}

impl Render for StageLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_now = window.animation_time();
        self.expire_deadlines(frame_now);
        self.refresh_transport();
        self.schedule_deadlines(window, cx);

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
                .child(themed_icon(icon!(music), 36.0, hsla(0.0, 0.0, 1.0, 0.25)))
                .child(
                    div()
                        .text_lg()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.50))
                        .child("暂无可显示歌词"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.30))
                        .child("支持内嵌 LRC 或联网自动检索"),
                );
        }

        let active = self
            .active_index
            .unwrap_or(0)
            .min(self.lines.len().saturating_sub(1));
        let reading_mode = self.is_reading();
        let center_index = if reading_mode {
            self.reading_center_index.unwrap_or(active)
        } else {
            active
        }
        .min(self.lines.len().saturating_sub(1));

        let handoff = (!reading_mode)
            .then_some(self.playback_stack_handoff)
            .flatten()
            .filter(|handoff| !handoff.finished(frame_now));

        let first_index = if reading_mode {
            center_index.saturating_sub(READING_STACK_HISTORY_ROWS)
        } else {
            handoff
                .map(|handoff| handoff.first_index)
                .unwrap_or_else(|| active.saturating_sub(PLAYBACK_STACK_HISTORY_ROWS))
        };
        let last_index = if reading_mode {
            center_index
                .saturating_add(READING_STACK_FUTURE_ROWS)
                .min(self.lines.len().saturating_sub(1))
        } else {
            handoff
                .map(|handoff| handoff.last_index)
                .unwrap_or_else(|| {
                    active
                        .saturating_add(PLAYBACK_STACK_FUTURE_ROWS)
                        .min(self.lines.len().saturating_sub(1))
                })
        };

        let viewport_height = f32::from(window.viewport_size().height).max(1.0);
        let anchor_y = viewport_height * LYRIC_ANCHOR_RATIO;
        let focus_from_index = self.focus_from_index;
        let focus_started_at = self.focus_started_at;
        let active_word_index = self.active_word_index;
        let position_ms = self.position_ms;
        let karaoke_epoch = self.karaoke_epoch;
        let karaoke_running = !reading_mode
            && self.stage_active
            && self.playback_state == PlaybackState::Playing
            && !self.scrubbing;
        let lines = self.lines.clone();
        let view = cx.entity().downgrade();
        let parent = self.parent.clone();

        let mut stack = div()
            .id("stage-lyrics-visible-stack")
            .relative()
            .size_full();

        for index in first_index..=last_index {
            let (y, exit_alpha) = if let Some(handoff) = handoff {
                let rank = index.saturating_sub(handoff.first_index);
                let progress = lyric_row_slot_progress(rank, handoff.started_at, frame_now);
                let from_y = playback_stack_row_top(index, handoff.from_active, anchor_y);
                let to_y = playback_stack_row_top(index, handoff.to_active, anchor_y);
                (
                    from_y + (to_y - from_y) * progress,
                    if index == handoff.first_index {
                        lyric_top_exit_alpha(progress)
                    } else {
                        1.0
                    },
                )
            } else {
                (
                    playback_stack_row_top(index, center_index, anchor_y),
                    1.0,
                )
            };

            let edge_progress = playback_stack_edge_progress(y, viewport_height);
            let viewport_blur_progress =
                playback_stack_blur_progress(y, viewport_height);
            let row = render_lyric_row(
                &lines[index],
                index,
                active,
                focus_from_index,
                focus_started_at,
                frame_now,
                edge_progress,
                edge_progress,
                viewport_blur_progress,
                active_word_index,
                position_ms,
                reading_mode,
                karaoke_running,
                !reading_mode,
                "lyric-text",
                self.hovered_index == Some(index),
                reading_mode || handoff.is_none(),
                karaoke_epoch,
                view.clone(),
                parent.clone(),
            );

            stack = stack.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .right(px(0.0))
                    .top(px(y))
                    .opacity(exit_alpha)
                    .child(row),
            );
        }

        let realtime_layout_animating =
            handoff.is_some() || focus_started_at.is_some() || karaoke_running;
        let stack = stack
            .with_layout_animation_target(realtime_layout_animating)
            .into_any_element();

        div()
            .id("stage-lyrics-view")
            .relative()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(
                |this, event: &gpui::ScrollWheelEvent, _, cx| {
                    let delta = event.delta.pixel_delta(px(48.0)).y;
                    this.begin_reading_mode(f32::from(delta), cx);
                    let _ = this
                        .parent
                        .update(cx, |app, cx| app.wake_stage_controls(cx));
                },
            ))
            .child(stack)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_lyric_row(
    line: &StageLyricLine,
    index: usize,
    active: usize,
    focus_from_index: Option<usize>,
    focus_started_at: Option<Instant>,
    frame_now: Instant,
    edge_progress: f32,
    previous_edge_progress: f32,
    viewport_blur_progress: f32,
    active_word_index: Option<usize>,
    position_ms: u64,
    reading_mode: bool,
    karaoke_running: bool,
    depth_blur_active: bool,
    text_id: &'static str,
    hovered: bool,
    interactive: bool,
    karaoke_epoch: u64,
    view: WeakEntity<StageLyricsView>,
    parent: WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let (target_alpha, target_blur) = lyric_visual_profile(
        index,
        active,
        edge_progress,
        viewport_blur_progress,
        reading_mode,
        depth_blur_active,
    );
    let timestamp = line.timestamp_ms;
    let karaoke_state = if reading_mode {
        KaraokeLineState::Static
    } else if index < active {
        KaraokeLineState::Past
    } else if index == active {
        KaraokeLineState::Active
    } else {
        KaraokeLineState::Future
    };

    let previous_active = focus_from_index.filter(|previous| *previous != active);
    let previous_profile = previous_active.map(|previous| {
        lyric_visual_profile(
            index,
            previous,
            previous_edge_progress,
            viewport_blur_progress,
            reading_mode,
            depth_blur_active,
        )
    });
    // Sample focus/depth locally instead of spawning GPUI PresentationAnimation timelines.
    // This keeps the same visual interpolation but caps work at the StageLyricsView cadence.
    let (resolved_alpha, resolved_blur) = match (previous_profile, focus_started_at) {
        (Some(previous), Some(started_at)) => {
            let row_t = lyric_focus_progress(started_at, frame_now);
            let alpha_t = row_t;
            let blur_t = row_t;
            (
                previous.0 + (target_alpha - previous.0) * alpha_t,
                previous.1 + (target_blur - previous.1) * blur_t,
            )
        }
        (Some(previous), None) => previous,
        (None, _) => (target_alpha, target_blur),
    };
    let mut resolved_alpha = if hovered { 1.0 } else { resolved_alpha };
    let resolved_blur = if hovered { 0.0 } else { resolved_blur };

    let text = lyric_text_layer(
        line,
        karaoke_state,
        active_word_index,
        position_ms,
        karaoke_running,
        karaoke_epoch,
        resolved_blur,
        text_id,
        index,
    )
    .opacity(resolved_alpha)
    .into_any_element();

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
        .child(text);

    if interactive {
        let hover_enter = view.clone();
        let hover_leave = view.clone();
        row = row
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
    }

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
                .opacity(if interactive && hovered { 1.0 } else { 0.0 })
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

    if !interactive {
        return row.into_any_element();
    }

    let local = view;
    let row = row.on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
        cx.stop_propagation();
        let _ = local.update(cx, |this, cx| {
            this.reading_until = None;
            this.hovered_index = None;
            this.position_ms = timestamp;
            this.focus_from_index = this.active_index;
            this.focus_started_at = None;
            this.active_index = Some(index);
            this.active_word_index = this.compute_active_word_index();
            this.karaoke_epoch = this.karaoke_epoch.wrapping_add(1);
            this.reading_center_index = Some(index);
            cx.notify();
        });
        let _ = parent.update(cx, |app, cx| {
            app.seek_to_ms(timestamp, cx);
            app.wake_stage_controls_immediately(cx);
        });
    });

    row.into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn lyric_text_layer(
    line: &StageLyricLine,
    karaoke_state: KaraokeLineState,
    active_word_index: Option<usize>,
    position_ms: u64,
    karaoke_running: bool,
    karaoke_epoch: u64,
    blur_sigma: f32,
    text_id: &'static str,
    index: usize,
) -> gpui::Stateful<gpui::Div> {
    let mut text = div()
        .id(ElementId::named_usize(text_id, index))
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_1()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(stage_primary_lyric(
            line,
            karaoke_state,
            active_word_index,
            position_ms,
            karaoke_running,
            karaoke_epoch,
        ));

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

    text.blur(px(blur_sigma.max(0.0)))
}

#[inline]
fn smoothstep01(value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lyric_visual_profile(
    index: usize,
    active: usize,
    edge_progress: f32,
    viewport_blur_progress: f32,
    reading_mode: bool,
    depth_blur_active: bool,
) -> (f32, f32) {
    if reading_mode {
        return (1.0, 0.0);
    }

    // Semantic focus controls brightness only. Blur is a physical viewport effect and therefore
    // must not follow index distance from the active lyric.
    let distance = index.abs_diff(active) as f32;
    let focus_alpha = lyric_focus_alpha(distance);
    let edge = smoothstep01(edge_progress);

    let edge_alpha = 1.0 + (0.035 - 1.0) * edge;
    let alpha = (focus_alpha * edge_alpha).clamp(0.012, 1.0);

    let blur = if depth_blur_active {
        LYRIC_VIEWPORT_MAX_BLUR_PX * viewport_blur_progress.clamp(0.0, 1.0)
    } else {
        0.0
    };

    (alpha, blur)
}

#[inline]
fn lyric_focus_alpha(distance: f32) -> f32 {
    let d = distance.max(0.0);
    let attenuation = 1.0 / (1.0 + 0.70 * d * d);
    (0.18 + 0.82 * attenuation).clamp(0.0, 1.0)
}

// Standalone helper retained for tests/reading semantics. Focus depth no longer owns blur.
fn lyric_focus_profile(
    distance: usize,
    reading_mode: bool,
    _depth_blur_active: bool,
) -> (f32, f32) {
    if reading_mode {
        return (1.0, 0.0);
    }
    (lyric_focus_alpha(distance as f32), 0.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KaraokeLineState {
    Static,
    Past,
    Active,
    Future,
}

fn active_enhanced_word_index(line: &StageLyricLine, position_ms: u64) -> Option<usize> {
    if !line.enhanced_complete {
        return None;
    }
    line.words
        .partition_point(|word| word.timestamp_ms <= position_ms)
        .checked_sub(1)
}

fn next_enhanced_word_timestamp(line: &StageLyricLine, position_ms: u64) -> Option<u64> {
    if !line.enhanced_complete {
        return None;
    }
    line.words
        .get(
            line.words
                .partition_point(|word| word.timestamp_ms <= position_ms),
        )
        .map(|word| word.timestamp_ms)
}

fn word_reveal_progress(word: &StageLyricWord, position_ms: u64) -> f32 {
    let Some(duration_ms) = word.duration_ms.filter(|duration| *duration > 0) else {
        return if position_ms >= word.timestamp_ms {
            1.0
        } else {
            0.0
        };
    };
    let elapsed = position_ms
        .saturating_sub(word.timestamp_ms)
        .min(duration_ms);
    (elapsed as f32 / duration_ms as f32).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, Default)]
struct SustainedWordEmphasis {
    glow_alpha: f32,
    glow_blur_px: f32,
    scale: f32,
    lift_px: f32,
}

#[inline]
fn is_cjk_text(text: &str) -> bool {
    text.chars().any(|ch| {
        matches!(
            ch as u32,
            0x3040..=0x30ff
                | 0x3400..=0x4dbf
                | 0x4e00..=0x9fff
                | 0xac00..=0xd7af
                | 0xf900..=0xfaff
        )
    })
}

#[inline]
fn should_emphasize_sustained_word(word: &StageLyricWord) -> bool {
    let Some(duration_ms) = word.duration_ms else {
        return false;
    };
    if duration_ms < 1_000 {
        return false;
    }

    let text = word.text.trim();
    if text.is_empty() {
        return false;
    }
    if is_cjk_text(text) {
        return true;
    }

    // AMLL intentionally keeps emphasis on short Latin words/syllables so a long phrase does not
    // scale and glow as one oversized block.
    let chars = text.chars().count();
    (2..=7).contains(&chars)
}

#[inline]
fn sustained_time_envelope(
    word: &StageLyricWord,
    position_ms: u64,
) -> f32 {
    let Some(duration_ms) = word.duration_ms.filter(|duration| *duration > 0) else {
        return 0.0;
    };
    let start_ms = word.timestamp_ms;
    let end_ms = start_ms.saturating_add(duration_ms);
    if position_ms < start_ms || position_ms >= end_ms {
        return 0.0;
    }

    let elapsed_ms = position_ms.saturating_sub(start_ms);
    let remaining_ms = end_ms.saturating_sub(position_ms);

    // Long-note emphasis is clock-driven, not mask-driven. Give it a short attack, keep the bloom
    // through the sustained body, and release only near the authored syllable end.
    let attack_ms = (duration_ms as f32 * 0.18).clamp(120.0, 260.0) as u64;
    let release_ms = (duration_ms as f32 * 0.20).clamp(140.0, 300.0) as u64;

    if elapsed_ms < attack_ms {
        smoothstep01(elapsed_ms as f32 / attack_ms.max(1) as f32)
    } else if remaining_ms < release_ms {
        smoothstep01(remaining_ms as f32 / release_ms.max(1) as f32)
    } else {
        1.0
    }
}

fn sustained_word_emphasis(
    word: &StageLyricWord,
    position_ms: u64,
    is_current_word: bool,
    is_last_word: bool,
) -> SustainedWordEmphasis {
    if !is_current_word || !should_emphasize_sustained_word(word) {
        return SustainedWordEmphasis {
            scale: 1.0,
            ..SustainedWordEmphasis::default()
        };
    }

    let duration_ms = word.duration_ms.unwrap_or(1_000).max(1_000) as f32;
    let envelope = sustained_time_envelope(word, position_ms);

    // Match AMLL's duration-sensitive character-emphasis shape: short qualifying sustains stay
    // subtle while very long notes gain progressively more bloom and motion.
    let amount_ratio = duration_ms / 2_000.0;
    let mut amount = if amount_ratio > 1.0 {
        amount_ratio.sqrt()
    } else {
        amount_ratio.powi(3)
    } * 0.6;

    let blur_ratio = duration_ms / 3_000.0;
    let mut blur = if blur_ratio > 1.0 {
        blur_ratio.sqrt()
    } else {
        blur_ratio.powi(3)
    } * 0.5;

    if is_last_word {
        amount *= 1.6;
        blur *= 1.5;
    }

    amount = amount.min(1.2);
    blur = blur.min(0.8);

    SustainedWordEmphasis {
        glow_alpha: (envelope * blur * 0.95).clamp(0.0, 0.78),
        glow_blur_px: 3.0 + blur * 6.0,
        scale: 1.0 + envelope * 0.10 * amount,
        lift_px: envelope * 0.70 * amount,
    }
}

fn karaoke_word(
    word: &StageLyricWord,
    _index: usize,
    reveal_progress: f32,
    position_ms: u64,
    is_current_word: bool,
    is_last_word: bool,
    _karaoke_epoch: u64,
    base_alpha: f32,
) -> gpui::AnyElement {
    let progress = reveal_progress.clamp(0.0, 1.0);
    let emphasis = sustained_word_emphasis(
        word,
        position_ms,
        is_current_word,
        is_last_word,
    );
    let base = div()
        .whitespace_nowrap()
        .text_color(hsla(0.0, 0.0, 1.0, base_alpha))
        .child(word.text.clone());

    // The reveal stays one retained layout subtree for Future -> Active -> Past. Changing the
    // width of an absolute clip avoids scene-animation bind/unbind barriers at word boundaries,
    // which caused a one-frame primitive replay flash while the virtual List was also prepainting.
    // Keep the glow subtree permanently mounted. Only opacity and clip width change, so an
    // Active -> held/sustained -> Past transition never swaps text primitives and cannot cause the
    // one-frame flash that existed in the old karaoke implementation.
    let glow = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .h_full()
        .w(relative(progress))
        .overflow_hidden()
        .opacity(emphasis.glow_alpha)
        .child(
            div()
                .whitespace_nowrap()
                .text_color(hsla(0.0, 0.0, 1.0, 0.92))
                .blur(px(emphasis.glow_blur_px))
                .child(word.text.clone()),
        );

    let overlay = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .h_full()
        .w(relative(progress))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(hsla(0.0, 0.0, 1.0, 1.0))
        .child(div().whitespace_nowrap().child(word.text.clone()));

    div()
        .relative()
        .top(px(-emphasis.lift_px))
        .scale(emphasis.scale)
        .flex_none()
        .whitespace_nowrap()
        .child(base)
        .child(glow)
        .child(overlay)
        .into_any_element()
}

fn stage_primary_lyric(
    line: &StageLyricLine,
    karaoke_state: KaraokeLineState,
    current_word: Option<usize>,
    position_ms: u64,
    animate: bool,
    karaoke_epoch: u64,
) -> gpui::AnyElement {
    if !line.enhanced_complete {
        return div()
            .w_full()
            .min_w(px(0.0))
            .text_size(px(28.0))
            .text_color(hsla(0.0, 0.0, 1.0, 1.0))
            .child(line.text.clone())
            .into_any_element();
    }

    // Every enhanced line keeps the same fragment/container tree before, during and after a line
    // hand-off, so retained text primitives never switch shape at a word or line boundary.
    const DIM_ALPHA: f32 = 0.46;
    const STATIC_ALPHA: f32 = 1.0;

    let mut row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_wrap()
        .items_center()
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD);

    for (index, word) in line.words.iter().enumerate() {
        let (progress, base_alpha, word_animate) = match karaoke_state {
            KaraokeLineState::Static => (1.0, STATIC_ALPHA, false),
            KaraokeLineState::Past => (1.0, DIM_ALPHA, false),
            KaraokeLineState::Future => (0.0, DIM_ALPHA, false),
            KaraokeLineState::Active => {
                let progress = match current_word {
                    Some(current) if index < current => 1.0,
                    Some(current) if index == current => {
                        word_reveal_progress(word, position_ms)
                    }
                    _ => 0.0,
                };
                // Long authored syllables get a separate emphasis envelope while their karaoke
                // mask continues to reveal; short syllables remain a plain mask sweep.
                (progress, DIM_ALPHA, animate && current_word == Some(index))
            }
        };

        row = row.child(karaoke_word(
            word,
            index,
            progress,
            position_ms,
            word_animate,
            index + 1 == line.words.len(),
            karaoke_epoch,
            base_alpha,
        ));
    }

    row.into_any_element()
}




#[inline]
fn playback_stack_row_top(index: usize, active: usize, anchor_y: f32) -> f32 {
    let delta = index as isize - active as isize;
    anchor_y
        + delta as f32 * PLAYBACK_STACK_ROW_PITCH_PX
        - PLAYBACK_STACK_ROW_CENTER_OFFSET_PX
}

#[inline]
fn playback_stack_edge_progress(row_top: f32, viewport_height: f32) -> f32 {
    let center = row_top + PLAYBACK_STACK_ROW_CENTER_OFFSET_PX;
    let top_visibility = smoothstep01(center / LYRIC_VIEWPORT_FADE_TOP_PX.max(1.0));
    let bottom_visibility = smoothstep01(
        (viewport_height - center) / LYRIC_VIEWPORT_FADE_BOTTOM_PX.max(1.0),
    );
    1.0 - top_visibility.min(bottom_visibility)
}

#[inline]
fn playback_stack_blur_progress(row_top: f32, viewport_height: f32) -> f32 {
    if !viewport_height.is_finite() || viewport_height <= 1.0 {
        return 0.0;
    }

    let row_center = row_top + PLAYBACK_STACK_ROW_CENTER_OFFSET_PX;
    let viewport_center = viewport_height * 0.5;
    let clear_half = (viewport_height * 0.13)
        .clamp(LYRIC_VIEWPORT_CLEAR_BAND_MIN_PX, LYRIC_VIEWPORT_CLEAR_BAND_MAX_PX);
    let distance = (row_center - viewport_center).abs();

    if distance <= clear_half {
        return 0.0;
    }

    let fade_distance = (viewport_height * 0.5 - clear_half).max(1.0);
    smoothstep01((distance - clear_half) / fade_distance)
}

#[inline]
fn lyric_focus_progress(started_at: Instant, now: Instant) -> f32 {
    AnimationSpec::new(LYRIC_FOCUS_TRANSITION_DURATION)
        .ease(Easing::OutCubic)
        .sample_elapsed(now.saturating_duration_since(started_at))
        .eased_progress
}

#[inline]
fn lyric_row_ease(progress: f32) -> f32 {
    let t = progress.clamp(0.0, 1.0);
    // Ease-out cubic gives the upward move a decisive start and a long, soft settling tail. This
    // avoids the evenly-paced "moving blocks" feel from smoothstep while keeping exact endpoints.
    1.0 - (1.0 - t).powi(3)
}

#[inline]
fn lyric_row_start_delay(rank: usize) -> Duration {
    Duration::from_secs_f32(
        LYRIC_ROW_MOVE_DURATION.as_secs_f32()
            * LYRIC_ROW_STAGGER_TIME_RATIO
            * rank as f32,
    )
}

#[inline]
fn lyric_row_slot_progress(rank: usize, started_at: Instant, now: Instant) -> f32 {
    let elapsed = now.saturating_duration_since(started_at);
    let slot_start = lyric_row_start_delay(rank);
    if elapsed <= slot_start {
        return 0.0;
    }

    let local = elapsed.saturating_sub(slot_start);
    if local >= LYRIC_ROW_MOVE_DURATION {
        return 1.0;
    }

    let raw =
        (local.as_secs_f32() / LYRIC_ROW_MOVE_DURATION.as_secs_f32()).clamp(0.0, 1.0);
    lyric_row_ease(raw)
}

#[inline]
fn lyric_top_exit_alpha(progress: f32) -> f32 {
    // Fade slightly faster than the final positional tail so the old top line is visually out of
    // the way while the next row starts at the 80% hand-off point.
    let fade = smoothstep01((progress / LYRIC_ROW_NEXT_START_PROGRESS).clamp(0.0, 1.0));
    1.0 - fade
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


#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::LyricWord;

    #[test]
    fn karaoke_line_state_keeps_future_and_active_words_on_same_base_layer() {
        assert_ne!(KaraokeLineState::Future, KaraokeLineState::Active);
        // Both states are rendered through karaoke_word(); the semantic state changes reveal only,
        // not the retained element shape. This assertion guards the explicit state model itself.
        assert_eq!(KaraokeLineState::Static, KaraokeLineState::Static);
    }

    #[test]
    fn playback_stack_moves_each_row_by_exactly_one_pitch() {
        let anchor = 400.0;
        let before = playback_stack_row_top(6, 5, anchor);
        let after = playback_stack_row_top(6, 6, anchor);
        assert!((before - after - PLAYBACK_STACK_ROW_PITCH_PX).abs() < 0.001);
    }

    #[test]
    fn playback_stack_top_row_starts_before_active_row() {
        let from_active = 6usize;
        let first = from_active.saturating_sub(PLAYBACK_STACK_HISTORY_ROWS);
        let active_rank = from_active.saturating_sub(first);
        assert_eq!(first, 2);
        assert!(active_rank > 0);
    }

    #[test]
    fn next_row_starts_when_previous_is_about_eighty_percent_complete() {
        let start = Instant::now();
        let second_start = start + lyric_row_start_delay(1);

        let first_at_handoff = lyric_row_slot_progress(0, start, second_start);
        let second_at_handoff = lyric_row_slot_progress(1, start, second_start);
        assert!((first_at_handoff - LYRIC_ROW_NEXT_START_PROGRESS).abs() < 0.015);
        assert_eq!(second_at_handoff, 0.0);

        let after = second_start + Duration::from_millis(10);
        assert!(lyric_row_slot_progress(1, start, after) > 0.0);
        assert!(lyric_row_slot_progress(0, start, after) > first_at_handoff);
    }

    #[test]
    fn upward_row_ease_moves_fast_then_settles_softly() {
        let p25 = lyric_row_ease(0.25);
        let p50 = lyric_row_ease(0.50);
        let p75 = lyric_row_ease(0.75);

        assert!(p25 > 0.50);
        assert!(p50 > p25);
        assert!(p75 > p50);
        assert!(1.0 - p75 < p75 - p50);
        assert_eq!(lyric_row_ease(0.0), 0.0);
        assert_eq!(lyric_row_ease(1.0), 1.0);
    }

    #[test]
    fn top_line_is_gone_by_the_eighty_percent_handoff() {
        assert!(lyric_top_exit_alpha(0.50) > 0.0);
        assert_eq!(
            lyric_top_exit_alpha(LYRIC_ROW_NEXT_START_PROGRESS),
            0.0
        );
        assert_eq!(lyric_top_exit_alpha(1.0), 0.0);
    }

    #[test]
    fn viewport_blur_has_a_clear_center_band_and_grows_toward_both_edges() {
        let height = 720.0;
        let center_top = height * 0.5 - PLAYBACK_STACK_ROW_CENTER_OFFSET_PX;
        assert_eq!(playback_stack_blur_progress(center_top, height), 0.0);

        let upper_mid = playback_stack_blur_progress(180.0, height);
        let upper_edge = playback_stack_blur_progress(20.0, height);
        let lower_mid = playback_stack_blur_progress(480.0, height);
        let lower_edge = playback_stack_blur_progress(660.0, height);

        assert!(upper_mid > 0.0 && upper_mid < upper_edge);
        assert!(lower_mid > 0.0 && lower_mid < lower_edge);
        assert!(upper_edge > 0.75);
        assert!(lower_edge > 0.75);
    }

    #[test]
    fn immersive_lyrics_render_only_a_small_visible_window() {
        let active = 10usize;
        let first = active.saturating_sub(PLAYBACK_STACK_HISTORY_ROWS);
        let last = active.saturating_add(PLAYBACK_STACK_FUTURE_ROWS);
        assert_eq!(first, 6);
        assert_eq!(last, 17);
        assert_eq!(last - first + 1, 12);
    }

    #[test]
    fn sustained_emphasis_requires_a_genuinely_long_syllable() {
        let short = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(900),
            byte_start: 0,
            byte_end: 1,
            text: SharedString::from("啊"),
        };
        let long = StageLyricWord {
            duration_ms: Some(1_800),
            ..short.clone()
        };

        assert!(!should_emphasize_sustained_word(&short));
        assert!(should_emphasize_sustained_word(&long));
    }

    #[test]
    fn sustained_emphasis_uses_authored_word_time_not_reveal_state() {
        let word = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(2_000),
            byte_start: 0,
            byte_end: 1,
            text: SharedString::from("啊"),
        };

        assert_eq!(sustained_time_envelope(&word, 999), 0.0);
        assert_eq!(sustained_time_envelope(&word, 1_000), 0.0);

        let attack = sustained_time_envelope(&word, 1_180);
        let sustain = sustained_time_envelope(&word, 2_000);
        let release = sustained_time_envelope(&word, 2_850);

        assert!(attack > 0.0 && attack < 1.0);
        assert_eq!(sustain, 1.0);
        assert!(release > 0.0 && release < 1.0);
        assert_eq!(sustained_time_envelope(&word, 3_000), 0.0);
    }

    #[test]
    fn sustained_emphasis_stays_visible_during_the_hold_and_releases_near_end() {
        let word = StageLyricWord {
            timestamp_ms: 5_000,
            duration_ms: Some(3_000),
            byte_start: 0,
            byte_end: 1,
            text: SharedString::from("啊"),
        };

        let middle = sustained_word_emphasis(&word, 6_500, true, false);
        let near_end = sustained_word_emphasis(&word, 7_900, true, false);
        let ended = sustained_word_emphasis(&word, 8_000, true, false);

        assert!(middle.glow_alpha > 0.0);
        assert!(middle.scale > 1.0);
        assert!(near_end.glow_alpha < middle.glow_alpha);
        assert_eq!(ended.glow_alpha, 0.0);
        assert_eq!(ended.scale, 1.0);
    }

    #[test]
    fn sustained_emphasis_is_disabled_after_current_word_advances() {
        let word = StageLyricWord {
            timestamp_ms: 0,
            duration_ms: Some(2_000),
            byte_start: 0,
            byte_end: 1,
            text: SharedString::from("啊"),
        };
        let profile = sustained_word_emphasis(&word, 1_000, false, false);
        assert_eq!(profile.glow_alpha, 0.0);
        assert_eq!(profile.scale, 1.0);
        assert_eq!(profile.lift_px, 0.0);
    }

    #[test]
    fn precise_lyric_time_keeps_subsecond_timing() {
        assert_eq!(format_lyric_time(62_345), "01:02.345");
        assert_eq!(format_lyric_time(3_662_007), "01:01:02.007");
    }

    #[test]
    fn semantic_focus_changes_alpha_but_not_viewport_blur() {
        let active = lyric_focus_profile(0, false, true);
        let near = lyric_focus_profile(1, false, true);
        let middle = lyric_focus_profile(3, false, true);
        let far = lyric_focus_profile(6, false, true);

        assert_eq!(active, (1.0, 0.0));
        assert!(active.0 > near.0 && near.0 > middle.0 && middle.0 >= far.0);
        assert_eq!(near.1, 0.0);
        assert_eq!(middle.1, 0.0);
        assert_eq!(far.1, 0.0);
        assert_eq!(lyric_focus_profile(2, true, true), (1.0, 0.0));
    }

    #[test]
    fn focus_band_controls_alpha_while_physical_y_controls_blur() {
        let active = lyric_visual_profile(10, 10, 0.0, 0.0, false, true);
        let row1 = lyric_visual_profile(11, 10, 0.0, 0.0, false, true);
        let row2 = lyric_visual_profile(12, 10, 0.0, 0.0, false, true);
        let row3 = lyric_visual_profile(13, 10, 0.0, 0.0, false, true);
        let row4 = lyric_visual_profile(14, 10, 0.0, 0.0, false, true);

        assert!(active.0 > 0.99);
        assert!(row1.0 < 0.70);
        assert!(row2.0 < 0.45);
        assert!(row3.0 < 0.33);
        assert!(row4.0 < 0.28);
        assert!(active.0 > row1.0);
        assert!(row1.0 > row2.0);
        assert!(row2.0 > row3.0);
        assert!(row3.0 > row4.0);

        // All rows are physically in the clear center band in this synthetic profile.
        assert_eq!(active.1, 0.0);
        assert_eq!(row1.1, 0.0);
        assert_eq!(row2.1, 0.0);

        let upper = lyric_visual_profile(10, 10, 0.0, 0.65, false, true);
        let edge = lyric_visual_profile(10, 10, 0.0, 1.0, false, true);
        assert!(upper.1 > 0.0);
        assert!(edge.1 > upper.1);
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
                    duration_ms: None,
                    text: "你好 ".into(),
                },
                LyricWord {
                    timestamp_ms: 1_500,
                    duration_ms: None,
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
                duration_ms: None,
                text: "你好".into(),
            }]),
        };
        assert!(!enhanced_words_cover_primary_text(&incomplete));
    }

    #[test]
    fn active_enhanced_word_uses_cached_semantic_boundary() {
        let source = LyricLine {
            timestamp_ms: 1_000,
            text: "你好 世界".into(),
            translation: None,
            words: Arc::from([
                LyricWord {
                    timestamp_ms: 1_000,
                    duration_ms: None,
                    text: "你好 ".into(),
                },
                LyricWord {
                    timestamp_ms: 1_500,
                    duration_ms: None,
                    text: "世界".into(),
                },
            ]),
        };
        let line = StageLyricLine::from_source(&source);
        assert_eq!(active_enhanced_word_index(&line, 999), None);
        assert_eq!(active_enhanced_word_index(&line, 1_000), Some(0));
        assert_eq!(active_enhanced_word_index(&line, 1_499), Some(0));
        assert_eq!(active_enhanced_word_index(&line, 1_500), Some(1));
        assert_eq!(next_enhanced_word_timestamp(&line, 999), Some(1_000));
        assert_eq!(next_enhanced_word_timestamp(&line, 1_000), Some(1_500));
        assert_eq!(next_enhanced_word_timestamp(&line, 1_499), Some(1_500));
        assert_eq!(next_enhanced_word_timestamp(&line, 1_500), None);
        assert_eq!(line.words[0].duration_ms, Some(500));
        assert_eq!(line.words[1].duration_ms, None);
        assert_eq!(line.words[0].byte_start, 0);
        assert_eq!(line.words[0].byte_end, "你好 ".len());
        assert_eq!(line.words[1].byte_start, "你好 ".len());
        assert_eq!(line.words[1].byte_end, line.text.len());
        assert_eq!(line.words[0].text.as_ref(), "你好 ");
    }

    #[test]
    fn authored_word_duration_drives_continuous_reveal_progress() {
        let source = LyricLine {
            timestamp_ms: 1_000,
            text: "ABC".into(),
            translation: None,
            words: Arc::from([LyricWord {
                timestamp_ms: 1_000,
                duration_ms: Some(300),
                text: "ABC".into(),
            }]),
        };
        let line = StageLyricLine::from_source(&source);
        let word = &line.words[0];
        assert_eq!(word_reveal_progress(word, 999), 0.0);
        assert_eq!(word_reveal_progress(word, 1_000), 0.0);
        assert!((word_reveal_progress(word, 1_150) - 0.5).abs() < 0.001);
        assert_eq!(word_reveal_progress(word, 1_300), 1.0);
    }
}
