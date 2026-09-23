use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, AnyView, BorrowAppContext as _,
    Context, Easing, ElementId, Entity, Global, HorizontalRevealEdge, IntoElement, Render,
    SharedString, StyleRefinement, Subscription, TransformOrigin, Transition, TransitionProperty,
    Visibility, WeakEntity, Window, bounds_observer, div, hsla, point, prelude::*, px, relative,
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
const LYRIC_ANCHOR_RATIO: f32 = 0.32;

// QueMusic/Apple-like focus channels are intentionally independent from row motion.
const LYRIC_FOCUS_ALPHA_DURATION: Duration = Duration::from_millis(320);
const LYRIC_FOCUS_SCALE_DURATION: Duration = Duration::from_millis(640);
const LYRIC_FOCUS_TRANSITION_DURATION: Duration = LYRIC_FOCUS_SCALE_DURATION;
const LYRIC_ACTIVE_SCALE: f32 = 1.02;
const LYRIC_INACTIVE_ALPHA: f32 = 0.50;
const LYRIC_ACTIVE_ALPHA: f32 = 0.90;

// Match QueMusic's authored stagger/settle curve exactly. Jitter must be fixed in retained capture
// and frame traversal rather than by shortening the animation until individual row steps become
// visually abrupt.
const LYRIC_MOTION_DELAY_BASE_MS: f32 = 24.0;
const LYRIC_MOTION_DELAY_POWER: f32 = 1.20;
const LYRIC_MOTION_BASE_DURATION_MS: f32 = 460.0;
const LYRIC_MOTION_DURATION_STEP_MS: f32 = 32.0;
const LYRIC_MOTION_BEZIER_X1: f32 = 0.24;
const LYRIC_MOTION_BEZIER_Y1: f32 = 0.06;
const LYRIC_MOTION_BEZIER_X2: f32 = 0.0;
const LYRIC_MOTION_BEZIER_Y2: f32 = 1.03;

// Blur is deliberately an edge-only depth cue. Keep the broad middle band sharp, cap the expensive
// Gaussian pass at a subtle radius, and quantize sigma so moving rows do not rebuild a new blur
// effect for every sub-pixel position.
const LYRIC_VIEWPORT_FADE_TOP_RATIO: f32 = 0.10;
const LYRIC_VIEWPORT_FADE_BOTTOM_RATIO: f32 = 0.18;
const LYRIC_VIEWPORT_BLUR_CLEAR_TOP_RATIO: f32 = 0.18;
const LYRIC_VIEWPORT_BLUR_CLEAR_BOTTOM_RATIO: f32 = 0.78;
const LYRIC_VIEWPORT_MAX_BLUR_PX: f32 = 2.50;
const LYRIC_VIEWPORT_BLUR_MIN_APPLY_PX: f32 = 0.50;
const LYRIC_VIEWPORT_BLUR_QUANTUM_PX: f32 = 0.25;

const PLAYBACK_STACK_HISTORY_ROWS: usize = 4;
const PLAYBACK_STACK_FUTURE_ROWS: usize = 7;
const PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX: f32 = 82.0;
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
    fn duration(self) -> Duration {
        let relative = self.last_index as isize - self.to_active as isize;
        let (delay, duration) = lyric_row_motion_timing(relative);
        delay + duration
    }

    #[inline]
    fn finished(self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.duration()
    }
}

#[derive(Clone, Copy, Debug)]
struct LyricRowVisualMotion {
    offset_y: f32,
    delay: Duration,
    duration: Duration,
    curve_x2: f32,
    from_viewport_alpha: f32,
    to_viewport_alpha: f32,
    from_viewport_blur: f32,
    to_viewport_blur: f32,
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

struct StageKaraokeLineView {
    engine: Option<Arc<AudioEngine>>,
    line: StageLyricLine,
    position_ms: u64,
    playback_state: PlaybackState,
    scrubbing: bool,
    animation_epoch: u64,
    _ui_subscription: Subscription,
}

impl StageKaraokeLineView {
    fn new(
        engine: Option<Arc<AudioEngine>>,
        line: StageLyricLine,
        position_ms: u64,
        playback_state: PlaybackState,
        scrubbing: bool,
        ui_events: Entity<app_ui_events::AppUiEventBridge>,
        animation_epoch: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| {
            match *event {
                AppUiEvent::PlaybackStateChanged(state) => {
                    if this.playback_state != state {
                        this.playback_state = state;
                        this.animation_epoch = this.animation_epoch.wrapping_add(1);
                        cx.notify();
                    }
                }
                AppUiEvent::ProgressChanged { position_ms, ratio } => {
                    let scrubbing = ratio.is_some();
                    if this.position_ms != position_ms || this.scrubbing != scrubbing {
                        this.position_ms = position_ms;
                        this.scrubbing = scrubbing;
                        this.animation_epoch = this.animation_epoch.wrapping_add(1);
                        cx.notify();
                    }
                }
            }
        });
        Self {
            engine,
            line,
            position_ms,
            playback_state,
            scrubbing,
            animation_epoch,
            _ui_subscription: subscription,
        }
    }

    fn refresh_position(&mut self) {
        if self.scrubbing || self.playback_state != PlaybackState::Playing {
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        self.position_ms = engine.progress().1;
    }

    fn next_deadline(&self) -> Option<Duration> {
        if self.scrubbing || self.playback_state != PlaybackState::Playing {
            return None;
        }
        let timestamp = next_karaoke_timestamp(&self.line, self.position_ms)?;
        Some(Duration::from_millis(
            timestamp
                .saturating_sub(self.position_ms)
                .max(TRANSPORT_MIN_SLEEP),
        ))
    }
}

impl Render for StageKaraokeLineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_position();
        if let Some(delay) = self.next_deadline() {
            window.request_invalidation_at(Instant::now() + delay, cx);
        }

        let current_word = active_enhanced_word_index(&self.line, self.position_ms);
        let animate = self.playback_state == PlaybackState::Playing && !self.scrubbing;
        karaoke_words_overlay(
            &self.line,
            KaraokeLineState::Active,
            current_word,
            self.position_ms,
            animate,
            self.animation_epoch,
        )
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
    active_karaoke_index: Option<usize>,
    active_karaoke_line: Option<Entity<StageKaraokeLineView>>,
    ui_events: Entity<app_ui_events::AppUiEventBridge>,
    hovered_index: Option<usize>,
    karaoke_epoch: u64,
    motion_epoch: u64,
    transport_generation: u64,
    reading_until: Option<Instant>,
    reading_center_index: Option<usize>,
    playback_stack_handoff: Option<LyricPlaybackStackHandoff>,
    row_heights: Vec<f32>,
    row_height_measured: Vec<bool>,
    row_prefix_sum: Vec<f32>,
    viewport_width: f32,
    viewport_height: f32,
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
        let ui_events_for_view = ui_events.clone();
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
            active_karaoke_index: None,
            active_karaoke_line: None,
            ui_events: ui_events_for_view,
            hovered_index: None,
            karaoke_epoch: 0,
            motion_epoch: 0,
            transport_generation,
            reading_until: None,
            reading_center_index: None,
            playback_stack_handoff: None,
            row_heights: Vec::new(),
            row_height_measured: Vec::new(),
            row_prefix_sum: vec![0.0],
            viewport_width: 0.0,
            viewport_height: 0.0,
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
                }

                let active_changed = self.update_active_index();
                changed |= active_changed;

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
            self.active_karaoke_index = None;
            self.active_karaoke_line = None;
            self.hovered_index = None;
            self.reading_until = None;
            self.reading_center_index = None;
            self.playback_stack_handoff = None;
            self.reset_row_geometry();
            self.scrubbing = false;
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            self.motion_epoch = self.motion_epoch.wrapping_add(1);
            changed = true;
        }

        if transport_changed && !source_changed {
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            self.active_karaoke_index = None;
            self.active_karaoke_line = None;
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
            self.active_karaoke_index = None;
            self.active_karaoke_line = None;
            if stage_active {
                self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            } else {
                self.reading_until = None;
                self.reading_center_index = None;
            }
            changed = true;
        }

        let active_changed = self.update_active_index();
        changed |= active_changed;

        if changed {
            cx.notify();
        }
    }

    fn sync_active_karaoke_leaf(&mut self, cx: &mut Context<Self>) {
        let desired_index = if self.stage_active && !self.is_reading() {
            self.active_index.filter(|index| {
                self.lines
                    .get(*index)
                    .is_some_and(|line| line.enhanced_complete)
            })
        } else {
            None
        };

        if self.active_karaoke_index == desired_index
            && self.active_karaoke_line.is_some() == desired_index.is_some()
        {
            return;
        }

        self.active_karaoke_index = desired_index;
        self.active_karaoke_line = desired_index.map(|index| {
            let line = self.lines[index].clone();
            let engine = self.engine.clone();
            let ui_events = self.ui_events.clone();
            let position_ms = self.position_ms;
            let playback_state = self.playback_state;
            let scrubbing = self.scrubbing;
            let animation_epoch = self.karaoke_epoch;
            cx.new(move |cx| {
                StageKaraokeLineView::new(
                    engine,
                    line,
                    position_ms,
                    playback_state,
                    scrubbing,
                    ui_events,
                    animation_epoch,
                    cx,
                )
            })
        });
    }

    fn reset_row_geometry(&mut self) {
        self.row_heights.clear();
        self.row_heights
            .resize(self.lines.len(), PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
        self.row_height_measured.clear();
        self.row_height_measured.resize(self.lines.len(), false);
        self.rebuild_row_prefix_sum();
    }

    fn rebuild_row_prefix_sum(&mut self) {
        self.row_prefix_sum.clear();
        self.row_prefix_sum.reserve(self.row_heights.len() + 1);
        self.row_prefix_sum.push(0.0);
        let mut total = 0.0;
        for height in self.row_heights.iter().copied() {
            total += height.max(1.0);
            self.row_prefix_sum.push(total);
        }
    }

    fn update_row_height(&mut self, index: usize, height: f32, cx: &mut Context<Self>) {
        if !height.is_finite() || height <= 1.0 || index >= self.row_heights.len() {
            return;
        }

        let first_measurement = self
            .row_height_measured
            .get(index)
            .is_some_and(|measured| !*measured);
        if let Some(measured) = self.row_height_measured.get_mut(index) {
            *measured = true;
        }

        let height_changed = (self.row_heights[index] - height).abs() > 0.5;
        if height_changed {
            self.row_heights[index] = height;
            self.rebuild_row_prefix_sum();
        }

        // Remove the one-shot observer after its first stable measurement. During a hand-off this
        // avoids running an entity update callback for every visible row on every animation frame.
        if first_measurement || height_changed {
            cx.notify();
        }
    }

    fn update_viewport_size(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        if !width.is_finite() || !height.is_finite() || width <= 1.0 || height <= 1.0 {
            return;
        }

        let width_changed = (self.viewport_width - width).abs() > 0.5;
        let height_changed = (self.viewport_height - height).abs() > 0.5;
        if !width_changed && !height_changed {
            return;
        }

        self.viewport_width = width;
        self.viewport_height = height;
        if width_changed {
            self.row_height_measured.fill(false);
        }
        cx.notify();
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
        self.motion_epoch = self.motion_epoch.wrapping_add(1);
        self.focus_from_index = previous;
        self.focus_started_at = previous.map(|_| Instant::now());

        self.playback_stack_handoff = match (previous, active) {
            (Some(from_active), Some(to_active))
                if self.stage_active
                    && !self.is_reading()
                    && !self.scrubbing
                    && to_active > from_active
                    && to_active - from_active <= 3 =>
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
        self.active_karaoke_index = None;
        self.active_karaoke_line = None;
        self.hovered_index = None;
        self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        if !self.is_reading() {
            self.reading_center_index = None;
        }
        true
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

        self.position_ms = position_ms;
        let _active_changed = self.update_active_index();
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
        // Focus/row transitions are renderer-owned. Do not wake the UI thread merely because a
        // visual timeline reached its final sample: the retained scene already displays that final
        // value. Stale focus/handoff bookkeeping is retired on the next semantic render
        // (word/line boundary, input, pause/seek, etc.).
    }
}

impl Render for StageLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_now = window.animation_time();
        self.expire_deadlines(frame_now);
        self.refresh_transport();
        self.sync_active_karaoke_leaf(cx);
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

        let fallback_viewport = window.viewport_size();
        let viewport_height = if self.viewport_height > 1.0 {
            self.viewport_height
        } else {
            f32::from(fallback_viewport.height).max(1.0)
        };
        let anchor_y = viewport_height * LYRIC_ANCHOR_RATIO;
        let focus_from_index = self.focus_from_index;
        let focus_started_at = self.focus_started_at;
        let karaoke_epoch = self.karaoke_epoch;
        let motion_epoch = self.motion_epoch;
        let motion_spring_value = handoff.map_or(0.0, |handoff| {
            lyric_motion_spring_value(
                handoff.from_active,
                handoff.to_active,
                &self.row_prefix_sum,
            )
        });
        let lines = self.lines.clone();
        let active_karaoke_line = self.active_karaoke_line.clone();
        let view = cx.entity().downgrade();
        let parent = self.parent.clone();

        let mut stack = div()
            .id("stage-lyrics-visible-stack")
            .relative()
            .size_full();

        for index in first_index..=last_index {
            let row_height = self
                .row_heights
                .get(index)
                .copied()
                .unwrap_or(PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
            let hovered = self.hovered_index == Some(index);

            let (y, viewport_alpha, viewport_blur, row_motion) =
                if let Some(handoff) = handoff {
                    let from_y = playback_stack_row_top(
                        index,
                        handoff.from_active,
                        &self.row_prefix_sum,
                        anchor_y,
                    );
                    let to_y = playback_stack_row_top(
                        index,
                        handoff.to_active,
                        &self.row_prefix_sum,
                        anchor_y,
                    );
                    let (mut from_alpha, mut from_blur) =
                        playback_stack_viewport_profile(from_y, row_height, viewport_height);
                    let (mut to_alpha, mut to_blur) =
                        playback_stack_viewport_profile(to_y, row_height, viewport_height);
                    if hovered {
                        from_alpha = 1.0;
                        to_alpha = 1.0;
                        from_blur = 0.0;
                        to_blur = 0.0;
                    }

                    let relative = index as isize - handoff.to_active as isize;
                    let (delay, duration) = lyric_row_motion_timing(relative);
                    let offset_y = from_y - to_y;
                    let curve_x2 =
                        motion_spring_value.clamp(-0.50, LYRIC_MOTION_BEZIER_X2);
                    let motion = (offset_y.abs() > 0.01).then_some(LyricRowVisualMotion {
                        offset_y,
                        delay,
                        duration,
                        curve_x2,
                        from_viewport_alpha: from_alpha,
                        to_viewport_alpha: to_alpha,
                        from_viewport_blur: from_blur,
                        to_viewport_blur: to_blur,
                    });
                    (to_y, to_alpha, to_blur, motion)
                } else {
                    let y = playback_stack_row_top(
                        index,
                        center_index,
                        &self.row_prefix_sum,
                        anchor_y,
                    );
                    let (mut viewport_alpha, mut viewport_blur) =
                        playback_stack_viewport_profile(y, row_height, viewport_height);
                    if reading_mode || hovered {
                        viewport_alpha = 1.0;
                        viewport_blur = 0.0;
                    }
                    (y, viewport_alpha, viewport_blur, None)
                };

            let active_karaoke_overlay = if !reading_mode && index == active {
                active_karaoke_line.as_ref().map(|view| {
                    AnyView::from(view.clone())
                        .cached(
                            StyleRefinement::default()
                                .absolute()
                                .inset_0(),
                        )
                        .reuse_on_window_refresh()
                })
            } else {
                None
            };

            let row = render_lyric_row(
                &lines[index],
                index,
                active,
                focus_from_index,
                focus_started_at,
                frame_now,
                motion_epoch,
                viewport_blur,
                row_motion,
                reading_mode,
                !reading_mode,
                "lyric-text",
                hovered,
                reading_mode || handoff.is_none(),
                karaoke_epoch,
                active_karaoke_overlay,
                view.clone(),
                parent.clone(),
            );

            let row_is_measured = self
                .row_height_measured
                .get(index)
                .copied()
                .unwrap_or(false);
            let mut row_slot = div()
                .absolute()
                .left(px(0.0))
                .right(px(0.0))
                .top(px(y))
                .child(row);

            if !row_is_measured {
                let row_measure_view = cx.entity().downgrade();
                row_slot = row_slot.child(
                    bounds_observer(move |bounds, _window, cx| {
                        let height = f32::from(bounds.size.height);
                        let _ = row_measure_view.update(cx, |this, cx| {
                            this.update_row_height(index, height, cx);
                        });
                    })
                    .absolute()
                    .inset_0(),
                );
            }

            let row_slot = if let Some(motion) = row_motion {
                let animation = Animation::from_spec(
                    AnimationSpec::new(motion.duration)
                        .delay(motion.delay)
                        .ease(Easing::CubicBezier {
                            x1: LYRIC_MOTION_BEZIER_X1,
                            y1: LYRIC_MOTION_BEZIER_Y1,
                            x2: motion.curve_x2,
                            y2: LYRIC_MOTION_BEZIER_Y2,
                        }),
                )
                .with_property(AnimationProperty::translation_opacity(
                    point(px(0.0), px(motion.offset_y)),
                    point(px(0.0), px(0.0)),
                    motion.from_viewport_alpha,
                    motion.to_viewport_alpha,
                ));
                row_slot
                    .with_animation(
                        ElementId::NamedInteger(
                            SharedString::new_static("lyric-row-motion"),
                            lyric_animation_instance_id(motion_epoch, index),
                        ),
                        animation,
                        |element, _| element,
                    )
                    .into_any_element()
            } else {
                row_slot.opacity(viewport_alpha).into_any_element()
            };
            stack = stack.child(row_slot);
        }

        let stack = stack.into_any_element();

        let viewport_measure_view = cx.entity().downgrade();
        let viewport_measure = bounds_observer(move |bounds, _window, cx| {
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let _ = viewport_measure_view.update(cx, |this, cx| {
                this.update_viewport_size(width, height, cx);
            });
        })
        .absolute()
        .inset_0();

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
            .child(viewport_measure)
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
    motion_epoch: u64,
    viewport_blur: f32,
    row_motion: Option<LyricRowVisualMotion>,
    reading_mode: bool,
    depth_blur_active: bool,
    text_id: &'static str,
    hovered: bool,
    interactive: bool,
    karaoke_epoch: u64,
    active_karaoke_overlay: Option<AnyView>,
    view: WeakEntity<StageLyricsView>,
    parent: WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let (target_alpha, target_blur) = lyric_visual_profile(
        index,
        active,
        1.0,
        viewport_blur,
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
            1.0,
            viewport_blur,
            reading_mode,
            depth_blur_active,
        )
    });
    let target_scale = lyric_focus_scale(index, active, reading_mode);
    let previous_scale = previous_active
        .map(|previous| lyric_focus_scale(index, previous, reading_mode))
        .unwrap_or(target_scale);

    // Focus is presentation-only. Keep final text geometry in the retained tree and let Nova own
    // the 320ms opacity and 640ms scale timelines instead of rerendering all visible lyric rows.
    let target_alpha = if hovered { 1.0 } else { target_alpha };
    let previous_alpha = if hovered {
        1.0
    } else {
        previous_profile.map_or(target_alpha, |previous| previous.0)
    };
    let resolved_blur = if hovered { 0.0 } else { target_blur };
    let blur_transition = row_motion.filter(|motion| {
        (motion.from_viewport_blur - motion.to_viewport_blur).abs() > 0.001
    });
    let static_blur = if blur_transition.is_some() {
        0.0
    } else {
        resolved_blur
    };
    let focus_transition = previous_active.is_some() && focus_started_at.is_some();
    let alpha_still_running = focus_started_at
        .is_some_and(|started_at| frame_now < started_at + LYRIC_FOCUS_ALPHA_DURATION);
    let animation_base_alpha = if focus_transition {
        previous_alpha.max(target_alpha)
    } else {
        target_alpha
    };
    let base_alpha = if focus_transition && alpha_still_running {
        animation_base_alpha
    } else {
        target_alpha
    };

    let mut text = lyric_text_layer(
        line,
        karaoke_state,
        should_render_karaoke_detail(index, active, reading_mode),
        karaoke_epoch,
        active_karaoke_overlay,
        static_blur,
        text_id,
        index,
    )
    .opacity(base_alpha)
    .transform_origin(TransformOrigin::new(0.0, 1.0))
    .scale(target_scale)
    .into_any_element();

    if focus_transition {
        let animation_id = lyric_animation_instance_id(motion_epoch, index);
        if (previous_scale - target_scale).abs() > 0.0001 {
            let target_scale_safe = target_scale.abs().max(0.0001);
            let animation = Animation::from_spec(
                AnimationSpec::new(LYRIC_FOCUS_SCALE_DURATION).ease(Easing::InOutCubic),
            )
            .with_property(AnimationProperty::scale_opacity(
                previous_scale / target_scale_safe,
                1.0,
                1.0,
                1.0,
                TransformOrigin::new(0.0, 1.0),
            ));
            text = text
                .with_animation(
                    ElementId::NamedInteger(
                        SharedString::new_static("lyric-focus-scale"),
                        animation_id,
                    ),
                    animation,
                    |element, _| element,
                )
                .into_any_element();
        }

        if alpha_still_running && (previous_alpha - target_alpha).abs() > 0.0001 {
            let alpha_base = animation_base_alpha.max(0.0001);
            let animation = Animation::from_spec(
                AnimationSpec::new(LYRIC_FOCUS_ALPHA_DURATION).ease(Easing::Linear),
            )
            .with_property(AnimationProperty::opacity(
                (previous_alpha / alpha_base).clamp(0.0, 1.0),
                (target_alpha / alpha_base).clamp(0.0, 1.0),
            ));
            text = text
                .with_animation(
                    ElementId::NamedInteger(
                        SharedString::new_static("lyric-focus-opacity"),
                        animation_id,
                    ),
                    animation,
                    |element, _| element,
                )
                .into_any_element();
        }
    }

    if let Some(motion) = blur_transition {
        let animation = Animation::from_spec(
            AnimationSpec::new(motion.duration)
                .delay(motion.delay)
                .ease(Easing::CubicBezier {
                    x1: LYRIC_MOTION_BEZIER_X1,
                    y1: LYRIC_MOTION_BEZIER_Y1,
                    x2: motion.curve_x2,
                    y2: LYRIC_MOTION_BEZIER_Y2,
                }),
        )
        .with_property(AnimationProperty::blur(
            px(motion.from_viewport_blur),
            px(motion.to_viewport_blur),
        ));
        text = text
            .with_animation(
                ElementId::NamedInteger(
                    SharedString::new_static("lyric-viewport-blur"),
                    lyric_animation_instance_id(motion_epoch, index),
                ),
                animation,
                |element, _| element,
            )
            .into_any_element();
    }

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
            this.active_karaoke_index = None;
            this.active_karaoke_line = None;
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
    karaoke_detail: bool,
    karaoke_epoch: u64,
    active_karaoke_overlay: Option<AnyView>,
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
            karaoke_detail,
            karaoke_epoch,
            active_karaoke_overlay,
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

    if blur_sigma >= LYRIC_VIEWPORT_BLUR_MIN_APPLY_PX {
        text = text.blur(px(blur_sigma));
    }
    text
}

#[inline]
fn lyric_animation_instance_id(epoch: u64, index: usize) -> u64 {
    epoch
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(index as u64)
}

#[inline]
fn smoothstep01(value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lyric_visual_profile(
    index: usize,
    active: usize,
    viewport_alpha: f32,
    viewport_blur: f32,
    reading_mode: bool,
    depth_blur_active: bool,
) -> (f32, f32) {
    if reading_mode {
        return (1.0, 0.0);
    }

    // QueMusic keeps inactive rows around 0.5 opacity and promotes only the current row. Focus
    // alpha is independent from the viewport field; callers may pass viewport_alpha=1 when the
    // row container owns edge fade through renderer translation_opacity. Blur remains text-only.
    let focus_alpha = lyric_focus_alpha(index.abs_diff(active) as f32);
    let alpha = (focus_alpha * viewport_alpha).clamp(0.0, 1.0);
    let blur = if depth_blur_active {
        viewport_blur.clamp(0.0, LYRIC_VIEWPORT_MAX_BLUR_PX)
    } else {
        0.0
    };
    (alpha, blur)
}

#[inline]
fn lyric_focus_alpha(distance: f32) -> f32 {
    if distance < 0.5 {
        LYRIC_ACTIVE_ALPHA
    } else {
        LYRIC_INACTIVE_ALPHA
    }
}

#[inline]
fn lyric_focus_scale(index: usize, active: usize, reading_mode: bool) -> f32 {
    if !reading_mode && index == active {
        LYRIC_ACTIVE_SCALE
    } else {
        1.0
    }
}

// Standalone helper retained for focused regression tests.
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

#[inline]
fn should_render_karaoke_detail(index: usize, active: usize, reading_mode: bool) -> bool {
    !reading_mode && (index == active || index.checked_add(1) == Some(active))
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

fn next_karaoke_timestamp(line: &StageLyricLine, position_ms: u64) -> Option<u64> {
    let mut next_timestamp = next_enhanced_word_timestamp(line, position_ms);

    if let Some(word_index) = active_enhanced_word_index(line, position_ms)
        && let Some(word) = line.words.get(word_index)
    {
        if let Some(duration_ms) = word.duration_ms.filter(|duration| *duration > 0) {
            let word_end = word.timestamp_ms.saturating_add(duration_ms);
            if word_end > position_ms {
                next_timestamp =
                    Some(next_timestamp.map_or(word_end, |current| current.min(word_end)));
            }
        }

        if let Some(release_timestamp) = sustained_release_timestamp(word)
            && release_timestamp > position_ms
        {
            next_timestamp = Some(
                next_timestamp
                    .map_or(release_timestamp, |current| current.min(release_timestamp)),
            );
        }
    }

    next_timestamp
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
    let (attack_ms, release_ms) =
        sustained_attack_release_ms(word).unwrap_or((120, 140));

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
    if !is_current_word {
        return SustainedWordEmphasis {
            scale: 1.0,
            ..SustainedWordEmphasis::default()
        };
    }

    let peak = sustained_word_peak_emphasis(word, is_last_word);
    if peak.glow_alpha <= 0.0 {
        return peak;
    }
    let envelope = sustained_time_envelope(word, position_ms);
    SustainedWordEmphasis {
        glow_alpha: peak.glow_alpha * envelope,
        glow_blur_px: peak.glow_blur_px,
        scale: 1.0 + (peak.scale - 1.0) * envelope,
        lift_px: peak.lift_px * envelope,
    }
}

fn karaoke_reveal_layer(
    content: impl IntoElement + 'static,
    word: &StageLyricWord,
    word_index: usize,
    progress: f32,
    position_ms: u64,
    animate: bool,
    karaoke_epoch: u64,
    animation_name: &'static str,
) -> gpui::AnyElement {
    let progress = progress.clamp(0.0, 1.0);
    let layer = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .h_full()
        .overflow_hidden()
        .child(content);

    if animate
        && progress < 1.0
        && let Some(remaining) = word_reveal_remaining_duration(word, position_ms)
    {
        let animation = Animation::from_spec(
            AnimationSpec::new(remaining).ease(Easing::Linear),
        )
        .with_property(AnimationProperty::horizontal_reveal(
            HorizontalRevealEdge::Left,
            progress,
            1.0,
        ));
        return layer
            .w_full()
            .with_animation(
                ElementId::NamedInteger(
                    SharedString::new_static(animation_name),
                    lyric_animation_instance_id(karaoke_epoch, word_index),
                ),
                animation,
                |element, _| element,
            )
            .into_any_element();
    }

    layer.w(relative(progress)).into_any_element()
}

fn word_reveal_remaining_duration(
    word: &StageLyricWord,
    position_ms: u64,
) -> Option<Duration> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    let end_ms = word.timestamp_ms.saturating_add(duration_ms);
    (position_ms < end_ms).then(|| Duration::from_millis(end_ms - position_ms))
}

fn sustained_attack_release_ms(word: &StageLyricWord) -> Option<(u64, u64)> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    Some((
        (duration_ms as f32 * 0.18).clamp(120.0, 260.0) as u64,
        (duration_ms as f32 * 0.20).clamp(140.0, 300.0) as u64,
    ))
}

fn sustained_release_timestamp(word: &StageLyricWord) -> Option<u64> {
    if !should_emphasize_sustained_word(word) {
        return None;
    }
    let duration_ms = word.duration_ms?;
    let (_, release_ms) = sustained_attack_release_ms(word)?;
    Some(
        word.timestamp_ms
            .saturating_add(duration_ms)
            .saturating_sub(release_ms),
    )
}

fn sustained_word_peak_emphasis(
    word: &StageLyricWord,
    is_last_word: bool,
) -> SustainedWordEmphasis {
    if !should_emphasize_sustained_word(word) {
        return SustainedWordEmphasis {
            scale: 1.0,
            ..SustainedWordEmphasis::default()
        };
    }

    let duration_ms = word.duration_ms.unwrap_or(1_000).max(1_000) as f32;
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
        glow_alpha: (blur * 0.95).clamp(0.0, 0.78),
        glow_blur_px: 3.0 + blur * 6.0,
        scale: 1.0 + 0.10 * amount,
        lift_px: 0.70 * amount,
    }
}

fn sustained_animation_segment(
    word: &StageLyricWord,
    position_ms: u64,
) -> Option<(f32, f32, Duration)> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    let (attack_ms, release_ms) = sustained_attack_release_ms(word)?;
    let start_ms = word.timestamp_ms;
    let end_ms = start_ms.saturating_add(duration_ms);
    if position_ms < start_ms || position_ms >= end_ms {
        return None;
    }

    let elapsed_ms = position_ms - start_ms;
    let remaining_ms = end_ms - position_ms;
    let envelope = sustained_time_envelope(word, position_ms);

    if elapsed_ms < attack_ms {
        Some((
            envelope,
            1.0,
            Duration::from_millis((attack_ms - elapsed_ms).max(1)),
        ))
    } else if remaining_ms <= release_ms {
        Some((
            envelope,
            0.0,
            Duration::from_millis(remaining_ms.max(1)),
        ))
    } else {
        None
    }
}

fn karaoke_word(
    word: &StageLyricWord,
    index: usize,
    reveal_progress: f32,
    position_ms: u64,
    is_current_word: bool,
    is_last_word: bool,
    karaoke_epoch: u64,
) -> gpui::AnyElement {
    let progress = reveal_progress.clamp(0.0, 1.0);
    let animate = is_current_word && word_reveal_remaining_duration(word, position_ms).is_some();
    // The stable full-line text below owns layout and the dim glyphs. This transparent copy only
    // supplies each word's exact inline box for the absolute karaoke overlay, so it cannot change
    // row height or double the dim alpha.
    let mut base = div()
        .whitespace_nowrap()
        .child(word.text.clone());
    // This copy exists only to preserve each authored word's exact flex/text layout box. Hidden
    // visibility participates in layout but Div exits before paint, so it does not emit a second
    // transparent glyph run into the scene or glyph raster path.
    base.style().visibility = Some(Visibility::Hidden);

    let overlay = karaoke_reveal_layer(
        div()
            .whitespace_nowrap()
            .text_color(hsla(0.0, 0.0, 1.0, 1.0))
            .child(word.text.clone()),
        word,
        index,
        progress,
        position_ms,
        animate,
        karaoke_epoch,
        "lyric-word-reveal",
    );

    let peak = sustained_word_peak_emphasis(word, is_last_word);
    let sustained = is_current_word && peak.glow_alpha > 0.0;
    let static_emphasis = sustained_word_emphasis(
        word,
        position_ms,
        is_current_word,
        is_last_word,
    );

    let mut word_root = div()
        .relative()
        .flex_none()
        .whitespace_nowrap()
        .child(base);

    if sustained {
        let mut glow = div()
            .whitespace_nowrap()
            .text_color(hsla(0.0, 0.0, 1.0, 0.92))
            .blur(px(peak.glow_blur_px))
            .child(word.text.clone())
            .into_any_element();

        if animate {
            if let Some((from_envelope, to_envelope, duration)) =
                sustained_animation_segment(word, position_ms)
            {
                let animation = Animation::from_spec(
                    AnimationSpec::new(duration).ease(Easing::InOutCubic),
                )
                .with_property(AnimationProperty::opacity(
                    from_envelope.clamp(0.0, 1.0),
                    to_envelope.clamp(0.0, 1.0),
                ));
                glow = glow
                    .with_animation(
                        ElementId::NamedInteger(
                            SharedString::new_static("lyric-word-glow"),
                            lyric_animation_instance_id(karaoke_epoch, index),
                        ),
                        animation,
                        |element, _| element,
                    )
                    .into_any_element();
            }
            glow = div()
                .opacity(peak.glow_alpha)
                .child(glow)
                .into_any_element();
        } else {
            glow = div()
                .opacity(static_emphasis.glow_alpha)
                .child(glow)
                .into_any_element();
        }

        word_root = word_root.child(karaoke_reveal_layer(
            glow,
            word,
            index,
            progress,
            position_ms,
            animate,
            karaoke_epoch,
            "lyric-word-glow-reveal",
        ));
    }

    word_root = word_root.child(overlay);

    if sustained {
        if animate {
            if let Some((from_envelope, to_envelope, duration)) =
                sustained_animation_segment(word, position_ms)
            {
                let from_scale = 1.0 + (peak.scale - 1.0) * from_envelope;
                let to_scale = 1.0 + (peak.scale - 1.0) * to_envelope;
                let peak_scale = peak.scale.max(0.0001);
                let animation = Animation::from_spec(
                    AnimationSpec::new(duration).ease(Easing::InOutCubic),
                )
                .with_property(AnimationProperty::scale_opacity(
                    from_scale / peak_scale,
                    to_scale / peak_scale,
                    1.0,
                    1.0,
                    TransformOrigin::new(0.0, 1.0),
                ));
                return word_root
                    .top(px(-peak.lift_px))
                    .scale(peak.scale)
                    .with_animation(
                        ElementId::NamedInteger(
                            SharedString::new_static("lyric-word-emphasis"),
                            lyric_animation_instance_id(karaoke_epoch, index),
                        ),
                        animation,
                        |element, _| element,
                    )
                    .into_any_element();
            }

            return word_root
                .top(px(-peak.lift_px))
                .scale(peak.scale)
                .into_any_element();
        }

        return word_root
            .top(px(-static_emphasis.lift_px))
            .scale(static_emphasis.scale)
            .into_any_element();
    }

    word_root.into_any_element()
}

fn karaoke_words_overlay(
    line: &StageLyricLine,
    karaoke_state: KaraokeLineState,
    current_word: Option<usize>,
    position_ms: u64,
    animate: bool,
    karaoke_epoch: u64,
) -> gpui::Div {
    let mut overlay = div()
        .w_full()
        .flex()
        .flex_wrap()
        .items_center()
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD);

    for (index, word) in line.words.iter().enumerate() {
        let (progress, word_animate) = match karaoke_state {
            KaraokeLineState::Static | KaraokeLineState::Past => (1.0, false),
            KaraokeLineState::Future => (0.0, false),
            KaraokeLineState::Active => {
                let progress = match current_word {
                    Some(current) if index < current => 1.0,
                    Some(current) if index == current => word_reveal_progress(word, position_ms),
                    _ => 0.0,
                };
                (progress, animate && current_word == Some(index))
            }
        };

        overlay = overlay.child(karaoke_word(
            word,
            index,
            progress,
            position_ms,
            word_animate,
            index + 1 == line.words.len(),
            karaoke_epoch,
        ));
    }

    overlay
}

fn stage_primary_lyric(
    line: &StageLyricLine,
    karaoke_state: KaraokeLineState,
    karaoke_detail: bool,
    karaoke_epoch: u64,
    active_karaoke_overlay: Option<AnyView>,
) -> gpui::AnyElement {
    const DIM_ALPHA: f32 = 0.46;
    const STATIC_ALPHA: f32 = 1.0;

    // A single full-line text node always owns wrapping and row height. Non-adjacent rows stop here,
    // so a 12-row viewport no longer expands every enhanced line into one subtree per authored word.
    // Old history rows keep their already-completed brightness; current/future rows keep the dim
    // base that the white karaoke mask reveals over.
    let base_alpha = match karaoke_state {
        KaraokeLineState::Static | KaraokeLineState::Past if !karaoke_detail => STATIC_ALPHA,
        KaraokeLineState::Static => STATIC_ALPHA,
        KaraokeLineState::Past | KaraokeLineState::Active | KaraokeLineState::Future => DIM_ALPHA,
    };
    let base = div()
        .w_full()
        .min_w(px(0.0))
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(hsla(0.0, 0.0, 1.0, base_alpha))
        .child(line.text.clone());

    if !line.enhanced_complete || !karaoke_detail {
        return base.into_any_element();
    }

    // The current line's moving word state lives in a dedicated cached Entity. The previous line
    // is static/completed and can be rebuilt only when the parent changes line.
    let overlay = if let Some(active_overlay) = active_karaoke_overlay {
        active_overlay.into_any_element()
    } else {
        div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(0.0))
            .child(karaoke_words_overlay(
                line,
                karaoke_state,
                None,
                0,
                false,
                karaoke_epoch,
            ))
            .into_any_element()
    };

    div()
        .relative()
        .w_full()
        .min_w(px(0.0))
        .child(base)
        .child(overlay)
        .into_any_element()
}




#[inline]
fn playback_stack_row_top(
    index: usize,
    active: usize,
    prefix_sum: &[f32],
    anchor_y: f32,
) -> f32 {
    let index_prefix = prefix_sum
        .get(index)
        .copied()
        .unwrap_or(index as f32 * PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
    let active_prefix = prefix_sum
        .get(active)
        .copied()
        .unwrap_or(active as f32 * PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);

    // QueMusic: prefixSum[i] - prefixSum[currentLine] + lyricContent.height * alignPos.
    anchor_y + index_prefix - active_prefix
}

#[inline]
fn lyric_motion_spring_value(
    from_active: usize,
    to_active: usize,
    prefix_sum: &[f32],
) -> f32 {
    let from = prefix_sum
        .get(from_active)
        .copied()
        .unwrap_or(from_active as f32 * PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
    let to = prefix_sum
        .get(to_active)
        .copied()
        .unwrap_or(to_active as f32 * PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
    let anime_height = (to - from).max(0.0);

    if anime_height <= 400.0 {
        return 0.0;
    }

    (((anime_height - 400.0) / 20.0).floor() / -200.0).max(-0.50)
}

#[inline]
fn quantize_viewport_blur(blur_px: f32) -> f32 {
    let blur = blur_px.clamp(0.0, LYRIC_VIEWPORT_MAX_BLUR_PX);
    if blur < LYRIC_VIEWPORT_BLUR_MIN_APPLY_PX {
        return 0.0;
    }

    ((blur / LYRIC_VIEWPORT_BLUR_QUANTUM_PX).round() * LYRIC_VIEWPORT_BLUR_QUANTUM_PX)
        .clamp(LYRIC_VIEWPORT_BLUR_MIN_APPLY_PX, LYRIC_VIEWPORT_MAX_BLUR_PX)
}

#[inline]
fn playback_stack_viewport_profile(
    row_top: f32,
    row_height: f32,
    viewport_height: f32,
) -> (f32, f32) {
    if !viewport_height.is_finite() || viewport_height <= 1.0 {
        return (1.0, 0.0);
    }

    let y = ((row_top + row_height * 0.5) / viewport_height).clamp(0.0, 1.0);
    let blur_k = if y < LYRIC_VIEWPORT_BLUR_CLEAR_TOP_RATIO {
        smoothstep01(
            (LYRIC_VIEWPORT_BLUR_CLEAR_TOP_RATIO - y)
                / LYRIC_VIEWPORT_BLUR_CLEAR_TOP_RATIO.max(0.001),
        )
    } else if y > LYRIC_VIEWPORT_BLUR_CLEAR_BOTTOM_RATIO {
        smoothstep01(
            (y - LYRIC_VIEWPORT_BLUR_CLEAR_BOTTOM_RATIO)
                / (1.0 - LYRIC_VIEWPORT_BLUR_CLEAR_BOTTOM_RATIO).max(0.001),
        )
    } else {
        0.0
    };
    let fade = smoothstep01(y / LYRIC_VIEWPORT_FADE_TOP_RATIO.max(0.001))
        * smoothstep01(
            (1.0 - y) / LYRIC_VIEWPORT_FADE_BOTTOM_RATIO.max(0.001),
        );

    (
        fade.clamp(0.0, 1.0),
        quantize_viewport_blur(LYRIC_VIEWPORT_MAX_BLUR_PX * blur_k),
    )
}

#[inline]
fn lyric_row_motion_timing(relative_to_active: isize) -> (Duration, Duration) {
    if relative_to_active < -3 {
        return (
            Duration::ZERO,
            Duration::from_secs_f32(LYRIC_MOTION_BASE_DURATION_MS / 1_000.0),
        );
    }

    let shifted = (relative_to_active + 4).max(0) as f32;
    let delay_ms = shifted.powf(LYRIC_MOTION_DELAY_POWER) * LYRIC_MOTION_DELAY_BASE_MS;
    let duration_ms =
        LYRIC_MOTION_BASE_DURATION_MS + shifted * LYRIC_MOTION_DURATION_STEP_MS;

    (
        Duration::from_secs_f32(delay_ms / 1_000.0),
        Duration::from_secs_f32(duration_ms / 1_000.0),
    )
}

#[cfg(test)]
#[inline]
fn lyric_row_motion_progress(
    index: usize,
    target_active: usize,
    spring_value: f32,
    started_at: Instant,
    now: Instant,
) -> f32 {
    let relative = index as isize - target_active as isize;
    let (delay, duration) = lyric_row_motion_timing(relative);
    let elapsed = now.saturating_duration_since(started_at);
    if elapsed <= delay {
        return 0.0;
    }
    let local = elapsed.saturating_sub(delay);
    if local >= duration {
        return 1.0;
    }

    let raw = (local.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    Easing::CubicBezier {
        x1: LYRIC_MOTION_BEZIER_X1,
        y1: LYRIC_MOTION_BEZIER_Y1,
        x2: spring_value.clamp(-0.50, LYRIC_MOTION_BEZIER_X2),
        y2: LYRIC_MOTION_BEZIER_Y2,
    }
    .sample(raw)
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
    fn karaoke_detail_is_limited_to_current_and_previous_rows() {
        let active = 10;
        assert!(should_render_karaoke_detail(10, active, false));
        assert!(should_render_karaoke_detail(9, active, false));
        assert!(!should_render_karaoke_detail(8, active, false));
        assert!(!should_render_karaoke_detail(11, active, false));
        assert!(!should_render_karaoke_detail(10, active, true));
    }

    #[test]
    fn prefix_sum_geometry_uses_real_row_heights() {
        let heights = [82.0, 110.0, 90.0];
        let prefix = [0.0, 82.0, 192.0, 282.0];
        let anchor = 400.0;

        let active_top = playback_stack_row_top(1, 1, &prefix, anchor);
        let next_top = playback_stack_row_top(2, 1, &prefix, anchor);
        let previous_top = playback_stack_row_top(0, 1, &prefix, anchor);

        assert_eq!(active_top, anchor);
        assert!((next_top - active_top - 110.0).abs() < 0.001);
        assert!((active_top - previous_top - 82.0).abs() < 0.001);
    }

    #[test]
    fn quemusic_motion_constants_match_reference_schedule() {
        assert_eq!(LYRIC_MOTION_DELAY_BASE_MS, 24.0);
        assert_eq!(LYRIC_MOTION_DELAY_POWER, 1.20);
        assert_eq!(LYRIC_MOTION_BASE_DURATION_MS, 460.0);
        assert_eq!(LYRIC_MOTION_DURATION_STEP_MS, 32.0);

        let (delay, duration) = lyric_row_motion_timing(0);
        let expected_delay_ms = 4.0_f32.powf(1.20) * 24.0;
        assert!((delay.as_secs_f32() * 1_000.0 - expected_delay_ms).abs() < 0.05);
        assert!((duration.as_secs_f32() * 1_000.0 - 588.0).abs() < 0.05);
    }

    #[test]
    fn quemusic_row_delay_and_duration_grow_down_the_stack() {
        let (top_delay, top_duration) = lyric_row_motion_timing(-4);
        let (near_delay, near_duration) = lyric_row_motion_timing(-3);
        let (active_delay, active_duration) = lyric_row_motion_timing(0);
        let (future_delay, future_duration) = lyric_row_motion_timing(3);

        assert_eq!(top_delay, Duration::ZERO);
        assert!(near_delay > top_delay);
        assert!(active_delay > near_delay);
        assert!(future_delay > active_delay);
        assert!(near_duration > top_duration);
        assert!(active_duration > near_duration);
        assert!(future_duration > active_duration);
    }

    #[test]
    fn quemusic_motion_curve_starts_slow_and_settles_with_soft_overshoot() {
        let start = Instant::now();
        let (delay, duration) = lyric_row_motion_timing(0);
        let p0 = lyric_row_motion_progress(10, 10, 0.0, start, start + delay);
        let p25 = lyric_row_motion_progress(
            10,
            10,
            0.0,
            start,
            start + delay + duration / 4,
        );
        let p75 = lyric_row_motion_progress(
            10,
            10,
            0.0,
            start,
            start + delay + duration * 3 / 4,
        );
        let p100 =
            lyric_row_motion_progress(10, 10, 0.0, start, start + delay + duration);

        assert_eq!(p0, 0.0);
        assert!(p25 > 0.0);
        assert!(p75 > p25);
        assert_eq!(p100, 1.0);
    }

    #[test]
    fn quemusic_alignment_uses_thirty_two_percent_of_local_lyrics_viewport() {
        let viewport_height = 600.0;
        let prefix = [0.0, 82.0, 164.0];
        let anchor = viewport_height * LYRIC_ANCHOR_RATIO;
        assert_eq!(LYRIC_ANCHOR_RATIO, 0.32);
        assert!((playback_stack_row_top(1, 1, &prefix, anchor) - 192.0).abs() < 0.001);
    }

    #[test]
    fn quemusic_dynamic_spring_only_changes_for_large_line_jumps() {
        let short_prefix = [0.0, 82.0, 164.0];
        assert_eq!(lyric_motion_spring_value(0, 1, &short_prefix), 0.0);

        let tall_prefix = [0.0, 510.0, 1020.0];
        let spring = lyric_motion_spring_value(0, 1, &tall_prefix);
        assert!(spring < 0.0);
        assert!(spring >= -0.50);
    }

    #[test]
    fn viewport_field_keeps_middle_rows_crisp_and_blurs_only_edges() {
        let height = 720.0;
        let center = playback_stack_viewport_profile(330.0, 82.0, height);
        let upper_middle = playback_stack_viewport_profile(92.0, 82.0, height);
        let lower_middle = playback_stack_viewport_profile(500.0, 82.0, height);
        let top = playback_stack_viewport_profile(-10.0, 82.0, height);
        let bottom = playback_stack_viewport_profile(660.0, 82.0, height);

        assert!(center.0 > 0.95);
        assert_eq!(center.1, 0.0);
        assert_eq!(upper_middle.1, 0.0);
        assert_eq!(lower_middle.1, 0.0);
        assert!(top.0 < center.0);
        assert!(bottom.0 < center.0);
        assert!(top.1 > 0.0 && top.1 <= LYRIC_VIEWPORT_MAX_BLUR_PX);
        assert!(bottom.1 > 0.0 && bottom.1 <= LYRIC_VIEWPORT_MAX_BLUR_PX);
        assert_eq!(quantize_viewport_blur(0.20), 0.0);
        assert_eq!(
            quantize_viewport_blur(8.0),
            LYRIC_VIEWPORT_MAX_BLUR_PX
        );
    }

    #[test]
    fn current_line_focus_matches_quemusic_scale_and_opacity_channels() {
        assert_eq!(lyric_focus_alpha(0.0), LYRIC_ACTIVE_ALPHA);
        assert_eq!(lyric_focus_alpha(1.0), LYRIC_INACTIVE_ALPHA);
        assert_eq!(lyric_focus_scale(4, 4, false), LYRIC_ACTIVE_SCALE);
        assert_eq!(lyric_focus_scale(3, 4, false), 1.0);
        assert_eq!(lyric_focus_scale(4, 4, true), 1.0);
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
    fn semantic_focus_is_binary_while_viewport_field_owns_blur() {
        assert_eq!(
            lyric_focus_profile(0, false, true),
            (LYRIC_ACTIVE_ALPHA, 0.0)
        );
        assert_eq!(
            lyric_focus_profile(1, false, true),
            (LYRIC_INACTIVE_ALPHA, 0.0)
        );
        assert_eq!(
            lyric_focus_profile(5, false, true),
            (LYRIC_INACTIVE_ALPHA, 0.0)
        );
        assert_eq!(lyric_focus_profile(2, true, true), (1.0, 0.0));

        let active = lyric_visual_profile(10, 10, 0.8, 2.0, false, true);
        let inactive = lyric_visual_profile(12, 10, 0.8, 2.0, false, true);
        assert!(active.0 > inactive.0);
        assert_eq!(active.1, 2.0);
        assert_eq!(inactive.1, 2.0);
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
    fn karaoke_leaf_owns_word_boundaries_and_release_deadlines() {
        let line = StageLyricLine {
            timestamp_ms: 1_000,
            text: SharedString::from("啊好"),
            translation: None,
            words: Arc::from([
                StageLyricWord {
                    timestamp_ms: 1_000,
                    duration_ms: Some(2_000),
                    byte_start: 0,
                    byte_end: 3,
                    text: SharedString::from("啊"),
                },
                StageLyricWord {
                    timestamp_ms: 3_000,
                    duration_ms: Some(400),
                    byte_start: 3,
                    byte_end: 6,
                    text: SharedString::from("好"),
                },
            ]),
            enhanced_complete: true,
            time_label: SharedString::new_static(""),
        };

        let release = sustained_release_timestamp(&line.words[0]).unwrap();
        assert_eq!(next_karaoke_timestamp(&line, 1_100), Some(release.min(3_000)));
        assert_eq!(next_karaoke_timestamp(&line, 2_999), Some(3_000));
        assert_eq!(next_karaoke_timestamp(&line, 3_100), Some(3_400));
        assert_eq!(next_karaoke_timestamp(&line, 3_400), None);
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
