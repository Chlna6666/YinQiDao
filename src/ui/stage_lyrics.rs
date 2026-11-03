use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, AnyView, BorrowAppContext as _,
    Context, Easing, ElementId, Entity, Global, HorizontalRevealEdge, IntoElement, Render,
    SharedString, Subscription, TransformOrigin, WeakEntity, Window, bounds_observer, div, point,
    prelude::*, px,
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
    theme::{dark, hover_transition, themed_icon},
};

const READING_MODE_DURATION: Duration = Duration::from_secs(3);
const READING_WHEEL_STEP_PX: f32 = 48.0;
const MAX_READING_SCROLL_ROWS_PER_EVENT: usize = 3;
// Focus the active lyric just above the artwork's visual center, rather than
// anchoring it close to the top of the fullscreen viewport.
const LYRIC_ANCHOR_RATIO: f32 = 0.42;

// QueMusic/Apple-like focus channels are intentionally independent from row motion.
// One compositor timeline owns focus opacity + scale, rather than two nested
// animation trees with independent durations and retained element state.
const LYRIC_FOCUS_TRANSITION_DURATION: Duration = Duration::from_millis(480);
const LYRIC_ACTIVE_SCALE: f32 = 1.02;
const LYRIC_INACTIVE_ALPHA: f32 = 0.40;
const LYRIC_ACTIVE_ALPHA: f32 = 1.0;

// Treat the lyrics as one continuous scroll surface. The entire visible
// stack shares a single compositor translation; only focus and timed syllables
// have independent animations. Staggering each row made the top and bottom
// look like different wheels and desynchronized edge transitions.
const LYRIC_STACK_SCROLL_DURATION: Duration = Duration::from_millis(560);

fn lyric_stack_easing() -> Easing {
    Easing::CubicBezier {
        x1: 0.22,
        y1: 0.05,
        x2: 0.12,
        y2: 1.0,
    }
}

// Blur is deliberately an edge-only depth cue. Keep the broad middle band sharp, cap the expensive
// Gaussian pass at a subtle radius, and quantize sigma so moving rows do not rebuild a new blur
// effect for every sub-pixel position.
const LYRIC_VIEWPORT_FADE_TOP_RATIO: f32 = 0.20;
const LYRIC_VIEWPORT_FADE_BOTTOM_RATIO: f32 = 0.28;
const LYRIC_VIEWPORT_BLUR_CLEAR_TOP_RATIO: f32 = 0.23;
const LYRIC_VIEWPORT_BLUR_CLEAR_BOTTOM_RATIO: f32 = 0.73;
const LYRIC_VIEWPORT_MAX_BLUR_PX: f32 = 2.50;
const LYRIC_VIEWPORT_BLUR_MIN_APPLY_PX: f32 = 0.50;
const LYRIC_VIEWPORT_BLUR_QUANTUM_PX: f32 = 0.25;

const PLAYBACK_STACK_HISTORY_ROWS: usize = 4;
const PLAYBACK_STACK_FUTURE_ROWS: usize = 7;
const PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX: f32 = 82.0;
const READING_STACK_HISTORY_ROWS: usize = 5;
const READING_STACK_FUTURE_ROWS: usize = 7;
const TRANSPORT_MIN_SLEEP: u64 = 8;
// The current karaoke leaf alone samples the short sustained attack/release
// at 60 Hz; holds and ordinary words remain event/deadline-driven.
const SUSTAINED_TRANSITION_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
// Limit malformed enhanced lyrics from expanding into an unbounded number of
// per-word renderer trees on the main UI stack.
const MAX_KARAOKE_WORDS_PER_LINE: usize = 96;

#[derive(Clone, Copy, Debug)]
struct LyricPlaybackStackHandoff {
    // Store pixel offsets instead of only row indices so an interrupted
    // transition can resume from the compositor's actual sampled position.
    from_scroll_px: f32,
    to_scroll_px: f32,
    first_index: usize,
    last_index: usize,
    started_at: Instant,
}

impl LyricPlaybackStackHandoff {
    #[inline]
    fn duration(self) -> Duration {
        LYRIC_STACK_SCROLL_DURATION
    }

    #[inline]
    fn progress(self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started_at);
        let t = (elapsed.as_secs_f32() / self.duration().as_secs_f32()).clamp(0.0, 1.0);
        if t >= 1.0 {
            1.0
        } else {
            lyric_stack_easing().sample(t)
        }
    }

    #[inline]
    fn scroll_px(self, now: Instant) -> f32 {
        self.from_scroll_px + (self.to_scroll_px - self.from_scroll_px) * self.progress(now)
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
        let enhanced_complete = line.words.len() <= MAX_KARAOKE_WORDS_PER_LINE
            && enhanced_words_cover_primary_text(line);
        // Providers occasionally concatenate English timed tokens without
        // spaces. Normalize once for both static text and karaoke rendering.
        let mut display_text = String::new();
        let mut words = Vec::with_capacity(line.words.len());
        for (index, word) in line.words.iter().enumerate() {
            let byte_start = display_text.len();
            let segment = if enhanced_complete {
                display_word_segment(&line.words, index)
            } else {
                word.text.clone()
            };
            display_text.push_str(&segment);
            let byte_end = display_text.len();
            let duration_ms = word
                .duration_ms
                .filter(|duration| *duration > 0)
                .or_else(|| {
                    line.words.get(index + 1).and_then(|next| {
                        let duration = next.timestamp_ms.saturating_sub(word.timestamp_ms);
                        (duration > 0).then_some(duration)
                    })
                });
            words.push(StageLyricWord {
                timestamp_ms: word.timestamp_ms,
                duration_ms,
                byte_start,
                byte_end,
                text: SharedString::from(segment),
            });
        }
        Self {
            timestamp_ms: line.timestamp_ms,
            text: SharedString::from(if enhanced_complete {
                display_text
            } else {
                line.text.clone()
            }),
            translation: line
                .translation
                .as_deref()
                .filter(|translation| !translation.trim().is_empty())
                .map(|translation| SharedString::from(translation.to_owned())),
            words: words.into(),
            enhanced_complete,
            time_label: SharedString::from(format_lyric_time(line.timestamp_ms)),
        }
    }
}

/// Restore missing English word separators without changing source timestamps.
fn display_word_segment(words: &[crate::lyrics::LyricWord], index: usize) -> String {
    let text = words[index].text.as_str();
    let mut output = text.to_owned();
    if let Some(next) = words.get(index + 1) {
        let last = text.chars().last();
        let first = next.text.chars().next();
        let ends_word = last.is_some_and(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, ',' | '.' | '!' | '?' | ';' | ':')
        });
        let begins_word = first.is_some_and(|ch| ch.is_ascii_alphanumeric());
        if ends_word && begins_word {
            output.push(' ');
        }
    }
    output
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
        let subscription = cx.subscribe(&ui_events, |this, _bridge, event, cx| match *event {
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
                    // Do not restart a running GPU word-reveal animation on
                    // every routine position notification.
                    if karaoke_seek_discontinuity(
                        this.position_ms,
                        position_ms,
                        this.scrubbing,
                        scrubbing,
                    ) {
                        this.animation_epoch = this.animation_epoch.wrapping_add(1);
                    }
                    this.position_ms = position_ms;
                    this.scrubbing = scrubbing;
                    cx.notify();
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
        let boundary_delay =
            next_karaoke_timestamp(&self.line, self.position_ms).map(|timestamp| {
                Duration::from_millis(
                    timestamp
                        .saturating_sub(self.position_ms)
                        .max(TRANSPORT_MIN_SLEEP),
                )
            });
        let mut animated_delay: Option<Duration> = None;
        if let Some(active) = active_enhanced_word_index(&self.line, self.position_ms) {
            // Drive both the newly expanding glyph and the previous glyph's
            // post-highlight settle; no full-row polling while they hold.
            for index in [Some(active), active.checked_sub(1)].into_iter().flatten() {
                let Some(word) = self.line.words.get(index) else {
                    continue;
                };
                if !should_emphasize_sustained_word(word) {
                    continue;
                }
                let visual = bounded_sustained_word(
                    word,
                    self.line.words.get(index + 1).map(|next| next.timestamp_ms),
                );
                if let Some(delay) = sustained_transition_frame_delay(&visual, self.position_ms) {
                    animated_delay =
                        Some(animated_delay.map_or(delay, |current| current.min(delay)));
                }
            }
        }
        match (boundary_delay, animated_delay) {
            (Some(boundary), Some(frame)) => Some(boundary.min(frame)),
            (Some(boundary), None) => Some(boundary),
            (None, Some(frame)) => Some(frame),
            (None, None) => None,
        }
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
        let view = cx.new(move |cx| StageLyricsView::new(parent, engine, view_events, cx));
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
    reading_scroll_remainder: f32,
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
            reading_scroll_remainder: 0.0,
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
            self.reading_scroll_remainder = 0.0;
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
                self.reading_scroll_remainder = 0.0;
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
        // Multi-line primary lyrics and their translations must be allocated
        // BEFORE the first paint. A live cached karaoke leaf can temporarily
        // report a zero-sized intrinsic box; reserving only the 82px single-
        // line slot makes the translation and following line paint on top of
        // the active English lyric until an observer updates the prefix sum.
        let content_width = stage_lyric_content_width(self.viewport_width, 1100.0);
        self.row_heights = self
            .lines
            .iter()
            .map(|line| estimated_lyric_row_height(line, content_width))
            .collect();
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

        if let Some(measured) = self.row_height_measured.get_mut(index) {
            *measured = true;
        }

        // Mark a row measured without invalidating the full lyric tree when
        // its geometry is unchanged. Observers previously caused redundant
        // first-measurement redraws across every visible row during stage open.
        // Absolute lyric slots derive their offsets from the prefix sum.
        // GPUI can briefly report the text-only box (without row margins) as
        // the first measurement while karaoke/blur layers are rebuilt. Never
        // compress below one full line, or an adjacent lyric paints over it.
        // A measured multiline row may grow; keep that footprint for this
        // viewport until the next width change / track reset. Shrinking on
        // each active-word transition caused unstable adjacent offsets.
        let reserved = height.max(PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
        if reserved > self.row_heights[index] + 0.5 {
            self.row_heights[index] = reserved;
            self.rebuild_row_prefix_sum();
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
            // A narrower column may wrap additional rows; a wider one may
            // unwrap them. Recalculate only when the real column width changes.
            self.reset_row_geometry();
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
                let now = Instant::now();
                let from_scroll_px = self
                    .playback_stack_handoff
                    .filter(|handoff| !handoff.finished(now))
                    .map_or_else(
                        || lyric_stack_scroll_offset(&self.row_prefix_sum, from_active),
                        |handoff| handoff.scroll_px(now),
                    );
                Some(LyricPlaybackStackHandoff {
                    from_scroll_px,
                    to_scroll_px: lyric_stack_scroll_offset(&self.row_prefix_sum, to_active),
                    first_index,
                    last_index,
                    started_at: now,
                })
            }
            _ => None,
        };

        // The same lyric row changes from static text to a timed word
        // layout when it becomes active. Old measured heights are no longer
        // valid; without remeasuring, the next absolute row overlaps wrapped
        // Japanese/Chinese/English lines.
        for index in [previous, active].into_iter().flatten() {
            if let Some(measured) = self.row_height_measured.get_mut(index) {
                *measured = false;
            }
        }

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
        let next_timestamp = active
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
        if !wheel_delta_y.is_finite() || self.lines.is_empty() {
            return;
        }
        let was_reading = self.is_reading();
        self.reading_until = Some(Instant::now() + READING_MODE_DURATION);
        self.focus_from_index = None;
        self.focus_started_at = None;
        self.hovered_index = None;

        let wheel_steps =
            take_reading_wheel_steps(&mut self.reading_scroll_remainder, wheel_delta_y);
        if wheel_steps == 0 {
            if !was_reading {
                cx.notify();
            }
            return;
        }

        let max_index = self.lines.len().saturating_sub(1);
        let previous = self
            .reading_center_index
            .or(self.active_index)
            .unwrap_or(0)
            .min(max_index);
        let next = if wheel_steps < 0 {
            previous
                .saturating_add(wheel_steps.unsigned_abs())
                .min(max_index)
        } else {
            previous.saturating_sub(wheel_steps as usize)
        };
        if previous == next {
            return;
        }

        // Reuse the compositor-owned row motion used by playback, rather
        // than remounting a different set of rows instantly on every wheel
        // message. Include BOTH sets of visible rows for the transition.
        let now = Instant::now();
        let from_scroll_px = self
            .playback_stack_handoff
            .filter(|handoff| !handoff.finished(now))
            .map_or_else(
                || lyric_stack_scroll_offset(&self.row_prefix_sum, previous),
                |handoff| handoff.scroll_px(now),
            );
        self.playback_stack_handoff = Some(LyricPlaybackStackHandoff {
            from_scroll_px,
            to_scroll_px: lyric_stack_scroll_offset(&self.row_prefix_sum, next),
            first_index: previous
                .min(next)
                .saturating_sub(READING_STACK_HISTORY_ROWS),
            last_index: previous
                .max(next)
                .saturating_add(READING_STACK_FUTURE_ROWS)
                .min(max_index),
            started_at: now,
        });
        self.reading_center_index = Some(next);
        self.motion_epoch = self.motion_epoch.wrapping_add(1);
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
        // The compositor animates the intermediate samples; schedule only ONE
        // semantic repaint at the boundary. Otherwise the retained transition
        // can keep stale focus/blur state until the next lyric or user event,
        // causing a sudden flash or disappearing text on the following repaint.
        if let Some(handoff) = self.playback_stack_handoff {
            let deadline = handoff.started_at + handoff.duration();
            if deadline > now {
                window.request_invalidation_at(deadline, cx);
            }
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
                .child(themed_icon(icon!(music), 36.0, dark::white(0.25)))
                .child(
                    div()
                        .text_lg()
                        .text_color(dark::white(0.50))
                        .child("暂无可显示歌词"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(dark::white(0.30))
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

        let handoff = self
            .playback_stack_handoff
            .filter(|handoff| !handoff.finished(frame_now));

        let first_index = if reading_mode {
            handoff
                .map(|motion| motion.first_index)
                .unwrap_or_else(|| center_index.saturating_sub(READING_STACK_HISTORY_ROWS))
        } else {
            handoff
                .map(|handoff| handoff.first_index)
                .unwrap_or_else(|| active.saturating_sub(PLAYBACK_STACK_HISTORY_ROWS))
        };
        let last_index = if reading_mode {
            handoff.map(|motion| motion.last_index).unwrap_or_else(|| {
                center_index
                    .saturating_add(READING_STACK_FUTURE_ROWS)
                    .min(self.lines.len().saturating_sub(1))
            })
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
        // Explicit width sidesteps Taffy 0.11's percent-width text measurement
        // mismatch when a padded flex parent contains wrapped lyric text.
        let lyric_text_width =
            stage_lyric_content_width(self.viewport_width, f32::from(fallback_viewport.width));
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
                .unwrap_or(PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX)
                .max(PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX);
            let hovered = self.hovered_index == Some(index);

            let (y, viewport_alpha, previous_viewport_alpha, viewport_blur) = if let Some(handoff) =
                handoff
            {
                let to_y =
                    playback_stack_row_top(index, center_index, &self.row_prefix_sum, anchor_y);
                let from_y = to_y + handoff.to_scroll_px - handoff.from_scroll_px;
                let (mut from_alpha, _) =
                    playback_stack_viewport_profile(from_y, row_height, viewport_height);
                let (mut to_alpha, mut to_blur) =
                    playback_stack_viewport_profile(to_y, row_height, viewport_height);
                if hovered {
                    from_alpha = 1.0;
                    to_alpha = 1.0;
                    to_blur = 0.0;
                }
                (to_y, to_alpha, from_alpha, to_blur)
            } else {
                let y = playback_stack_row_top(index, center_index, &self.row_prefix_sum, anchor_y);
                let (mut alpha, mut blur) =
                    playback_stack_viewport_profile(y, row_height, viewport_height);
                if hovered {
                    alpha = 1.0;
                    blur = 0.0;
                }
                (y, alpha, alpha, blur)
            };

            // During a handoff the compositor may still paint a previous
            // visual position even after a newer line boundary is reported.
            // Keep ALL bounded endpoint rows mounted until the animation
            // settles; endpoint-only culling made lines blink/disappear when
            // wheel input or lyric boundaries arrived mid-transition.
            // Outside a handoff we can safely cull genuinely offscreen rows.
            if handoff.is_none()
                && !lyric_row_intersects_viewport(y, y, row_height, viewport_height)
            {
                continue;
            }

            let active_karaoke_overlay = if !reading_mode && index == active {
                active_karaoke_line.as_ref().map(|view| {
                    // Keep the active karaoke view in the NORMAL layout tree.
                    // Cached/reuse-on-window-refresh AnyView could report an
                    // out-of-date height while its 28px words rewrapped,
                    // placing the 17px translation over the English text.
                    AnyView::from(view.clone())
                })
            } else {
                None
            };

            let row = render_lyric_row(
                &lines[index],
                lyric_text_width,
                index,
                active,
                focus_from_index,
                focus_started_at,
                frame_now,
                motion_epoch,
                viewport_blur,
                reading_mode,
                true,
                "lyric-text",
                hovered,
                reading_mode || handoff.is_none(),
                karaoke_epoch,
                active_karaoke_overlay,
                true, // Track wrapped row geometry throughout karaoke/focus transitions.
                view.clone(),
                parent.clone(),
            );

            // Observe the REAL row below (including wrapped text, translation
            // and padding), not the absolute slot which has no intrinsic
            // height. The row itself registers its bounds observer.
            let row_slot = div()
                .absolute()
                .left(px(0.0))
                .right(px(0.0))
                .top(px(y))
                .child(row);

            // Edge fades share the same easing and duration as the ONE scroll
            // transform. Only opacity is per-row: no glyph moves separately.
            let row_slot =
                if handoff.is_some() && (previous_viewport_alpha - viewport_alpha).abs() > 0.001 {
                    let animation = Animation::from_spec(
                        AnimationSpec::new(LYRIC_STACK_SCROLL_DURATION).ease(lyric_stack_easing()),
                    )
                    .with_property(AnimationProperty::opacity(
                        previous_viewport_alpha,
                        viewport_alpha,
                    ));
                    row_slot
                        .with_animation(
                            ElementId::NamedInteger(
                                SharedString::new_static("lyric-edge-fade"),
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

        let stack = if let Some(handoff) = handoff {
            let initial_offset = handoff.to_scroll_px - handoff.from_scroll_px;
            let animation = Animation::from_spec(
                AnimationSpec::new(LYRIC_STACK_SCROLL_DURATION).ease(lyric_stack_easing()),
            )
            .with_property(AnimationProperty::translation_opacity(
                point(px(0.0), px(initial_offset)),
                point(px(0.0), px(0.0)),
                1.0,
                1.0,
            ));
            stack
                .with_animation(
                    ElementId::NamedInteger(
                        SharedString::new_static("lyric-stack-scroll"),
                        motion_epoch,
                    ),
                    animation,
                    |element, _| element,
                )
                .into_any_element()
        } else {
            stack.into_any_element()
        };

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
            .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(48.0)).y;
                this.begin_reading_mode(f32::from(delta), cx);
                let _ = this
                    .parent
                    .update(cx, |app, cx| app.wake_stage_controls(cx));
            }))
            .child(viewport_measure)
            .child(stack)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_lyric_row(
    line: &StageLyricLine,
    content_width_px: f32,
    index: usize,
    active: usize,
    focus_from_index: Option<usize>,
    focus_started_at: Option<Instant>,
    frame_now: Instant,
    motion_epoch: u64,
    viewport_blur: f32,
    reading_mode: bool,
    depth_blur_active: bool,
    text_id: &'static str,
    hovered: bool,
    interactive: bool,
    karaoke_epoch: u64,
    active_karaoke_overlay: Option<AnyView>,
    measure_height: bool,
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

    let target_alpha = if hovered { 1.0 } else { target_alpha };
    let previous_alpha = if hovered {
        1.0
    } else {
        previous_profile.map_or(target_alpha, |previous| previous.0)
    };
    // Gaussian edge blur remains quantized and constant throughout the
    // row's compositor translation; the transition itself never animates
    // blur sigma. Disabling blur here made every row snap sharp on movement
    // and abruptly blurry again at completion.
    let static_blur = if hovered { 0.0 } else { target_blur };
    let focus_transition = previous_active.is_some()
        && focus_started_at
            .is_some_and(|started_at| frame_now < started_at + LYRIC_FOCUS_TRANSITION_DURATION);
    let base_alpha = if focus_transition {
        previous_alpha.max(target_alpha)
    } else {
        target_alpha
    };

    let mut text = lyric_text_layer(
        line,
        karaoke_state,
        should_render_karaoke_detail(index, active, reading_mode),
        karaoke_epoch,
        active_karaoke_overlay,
        content_width_px,
        static_blur,
        text_id,
        index,
    )
    .opacity(base_alpha)
    .transform_origin(TransformOrigin::new(0.0, 1.0))
    .scale(target_scale)
    .into_any_element();

    // The GPU applies scale and opacity together in one compositor pass;
    // previously two independent nested animations doubled prepaint depth.
    if focus_transition
        && ((previous_scale - target_scale).abs() > 0.0001
            || (previous_alpha - target_alpha).abs() > 0.0001)
    {
        let safe_alpha = base_alpha.max(0.0001);
        let animation = Animation::from_spec(
            AnimationSpec::new(LYRIC_FOCUS_TRANSITION_DURATION).ease(Easing::InOutCubic),
        )
        .with_property(AnimationProperty::scale_opacity(
            previous_scale / target_scale.max(0.0001),
            1.0,
            (previous_alpha / safe_alpha).clamp(0.0, 1.0),
            (target_alpha / safe_alpha).clamp(0.0, 1.0),
            TransformOrigin::new(0.0, 1.0),
        ));
        text = text
            .with_animation(
                ElementId::NamedInteger(
                    SharedString::new_static("lyric-focus"),
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
        .min_h(px(estimated_lyric_row_height(line, content_width_px)))
        .pl(px(16.0))
        .pr(px(104.0))
        .py(px(11.0))
        .mb(px(10.0))
        .cursor_pointer()
        .child(text);

    // Must be a child of the natural-height, relative row. Attaching this
    // to an absolute row slot instead measured its layout box rather than
    // the height of multiline/karaoke glyphs.
    if measure_height {
        let measure_view = view.clone();
        row = row.child(
            bounds_observer(move |bounds, _window, cx| {
                let height = f32::from(bounds.size.height);
                let _ = measure_view.update(cx, |this, cx| {
                    this.update_row_height(index, height, cx);
                });
            })
            .absolute()
            .inset_0(),
        );
    }

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
                .transition(hover_transition())
                .bg(dark::scrim(0.28))
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(dark::white(0.92))
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
    content_width_px: f32,
    blur_sigma: f32,
    text_id: &'static str,
    index: usize,
) -> gpui::Stateful<gpui::Div> {
    let mut text = div()
        .id(ElementId::named_usize(text_id, index))
        .w(px(content_width_px))
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(
            div()
                .w_full()
                .min_w(px(0.0))
                // Child AnyView is independently updated at word boundaries.
                // The flex column must reserve primary glyph height even
                // when that child has a transient zero-height layout cache.
                .min_h(px(estimated_primary_lyric_height(line, content_width_px)))
                .child(stage_primary_lyric(
                    line,
                    karaoke_state,
                    karaoke_detail,
                    karaoke_epoch,
                    active_karaoke_overlay,
                )),
        );

    if let Some(translation) = &line.translation {
        text = text.child(
            div()
                .w_full()
                .min_w(px(0.0))
                .text_size(px(15.0))
                .line_height(px(21.0))
                .font_weight(gpui::FontWeight::NORMAL)
                .text_color(dark::text_secondary())
                .child(translation.clone()),
        );
    }

    if blur_sigma >= LYRIC_VIEWPORT_BLUR_MIN_APPLY_PX {
        text = text.filter_blur(px(blur_sigma));
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

#[inline]
fn smootherstep01(value: f32) -> f32 {
    // Fifth-order eased attack/settle, with no velocity or acceleration jump.
    let t = value.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
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
        return (
            1.0,
            if depth_blur_active {
                viewport_blur.clamp(0.0, LYRIC_VIEWPORT_MAX_BLUR_PX)
            } else {
                0.0
            },
        );
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

#[inline]
fn karaoke_seek_discontinuity(
    previous_ms: u64,
    position_ms: u64,
    was_scrubbing: bool,
    scrubbing: bool,
) -> bool {
    was_scrubbing != scrubbing || previous_ms.abs_diff(position_ms) > 1_500
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

        let visual_word = bounded_sustained_word(
            word,
            line.words.get(word_index + 1).map(|next| next.timestamp_ms),
        );
        if let Some(release_timestamp) = sustained_tail_end_timestamp(&visual_word)
            && release_timestamp > position_ms
        {
            next_timestamp = Some(
                next_timestamp.map_or(release_timestamp, |current| current.min(release_timestamp)),
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
fn sustained_time_envelope(word: &StageLyricWord, position_ms: u64) -> f32 {
    let Some(duration_ms) = word.duration_ms.filter(|duration| *duration > 0) else {
        return 0.0;
    };
    let start_ms = word.timestamp_ms;
    let end_ms = start_ms.saturating_add(duration_ms);
    if position_ms < start_ms {
        return 0.0;
    }
    let (attack_ms, release_ms) = sustained_attack_release_ms(word).unwrap_or((280, 320));
    if position_ms >= end_ms.saturating_add(release_ms) {
        return 0.0;
    }
    let attack_ms = attack_ms.min(duration_ms);
    if position_ms < start_ms.saturating_add(attack_ms) {
        smootherstep01(position_ms.saturating_sub(start_ms) as f32 / attack_ms.max(1) as f32)
    } else if position_ms < end_ms {
        1.0
    } else {
        // At 100% highlight, keep the glyph enlarged and settle afterwards.
        1.0 - smootherstep01(position_ms.saturating_sub(end_ms) as f32 / release_ms.max(1) as f32)
    }
}

/// The next authored onset bounds the previous note's visible hold.
/// Its smooth release continues after that onset rather than snapping.
fn bounded_sustained_word(word: &StageLyricWord, next_start_ms: Option<u64>) -> StageLyricWord {
    let mut visual = word.clone();
    if let Some(next_start_ms) = next_start_ms
        && next_start_ms >= word.timestamp_ms
    {
        let available_ms = next_start_ms - word.timestamp_ms;
        visual.duration_ms = visual.duration_ms.map(|ms| ms.min(available_ms));
    }
    visual
}

/// The note envelope is authored-time driven. Request short invalidations
/// ONLY while the scalar envelope is changing; a steady sustained vowel
/// costs zero scheduled layout frames and retains its GPU word reveal.
fn sustained_transition_frame_delay(word: &StageLyricWord, position_ms: u64) -> Option<Duration> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    let (attack_ms, release_ms) = sustained_attack_release_ms(word)?;
    let elapsed_ms = position_ms.checked_sub(word.timestamp_ms)?;
    if elapsed_ms >= duration_ms.saturating_add(release_ms) {
        return None;
    }
    (elapsed_ms < attack_ms.min(duration_ms) || elapsed_ms >= duration_ms)
        .then_some(SUSTAINED_TRANSITION_FRAME_INTERVAL)
}

fn sustained_visual_emphasis(
    original: &StageLyricWord,
    visual: &StageLyricWord,
    position_ms: u64,
    is_last_word: bool,
) -> SustainedWordEmphasis {
    let peak = sustained_word_peak_emphasis(original, is_last_word);
    if peak.glow_alpha <= 0.0 {
        return peak;
    }
    let envelope = sustained_time_envelope(visual, position_ms);
    SustainedWordEmphasis {
        glow_alpha: peak.glow_alpha * envelope,
        glow_blur_px: peak.glow_blur_px,
        scale: 1.0 + (peak.scale.clamp(1.0, 1.035) - 1.0) * envelope,
        lift_px: peak.lift_px * envelope,
    }
}

fn sustained_tail_active(
    original: &StageLyricWord,
    visual: &StageLyricWord,
    position_ms: u64,
) -> bool {
    if !should_emphasize_sustained_word(original) {
        return false;
    }
    let Some(duration_ms) = visual.duration_ms.filter(|duration| *duration > 0) else {
        return false;
    };
    let end_ms = visual.timestamp_ms.saturating_add(duration_ms);
    let (_, release_ms) = sustained_attack_release_ms(visual).unwrap_or((280, 320));
    position_ms >= end_ms && position_ms < end_ms.saturating_add(release_ms)
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

    sustained_visual_emphasis(word, word, position_ms, is_last_word)
}

fn karaoke_reveal_word(
    content: impl IntoElement + 'static,
    word: &StageLyricWord,
    word_index: usize,
    progress: f32,
    position_ms: u64,
    animate: bool,
    karaoke_epoch: u64,
) -> gpui::AnyElement {
    let progress = progress.clamp(0.0, 1.0);
    let (duration, to_progress) = if animate
        && progress < 1.0
        && let Some(remaining) = word_reveal_remaining_duration(word, position_ms)
    {
        (remaining, 1.0)
    } else {
        // Keep future/past/current words on the same AnimationElement type. A zero-duration
        // timeline samples its terminal value immediately, so this is a paint-only static mask
        // while the child keeps its full intrinsic width for flex wrapping.
        (Duration::ZERO, progress)
    };

    // No animation wrapper is necessary for the numerous already-complete
    // or not-yet-started words. This also bounds recursion depth in GPUI's
    // layout, prepaint and retained animation traversal when entering Stage.
    // A 0%-revealed current word must still animate from zero; returning
    // an empty element here made the highlight appear only at the next tick.
    if progress <= 0.0 && !animate {
        return div().into_any_element();
    }
    if progress >= 1.0 {
        return content.into_any_element();
    }

    let animation =
        Animation::from_spec(AnimationSpec::new(duration).ease(Easing::Linear)).with_property(
            AnimationProperty::horizontal_reveal(HorizontalRevealEdge::Left, progress, to_progress),
        );

    content
        .with_animation(
            ElementId::NamedInteger(
                SharedString::new_static("lyric-word-reveal"),
                lyric_animation_instance_id(karaoke_epoch, word_index),
            ),
            animation,
            |element, _| element,
        )
        .into_any_element()
}

fn word_reveal_remaining_duration(word: &StageLyricWord, position_ms: u64) -> Option<Duration> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    let end_ms = word.timestamp_ms.saturating_add(duration_ms);
    (position_ms < end_ms).then(|| Duration::from_millis(end_ms - position_ms))
}

fn sustained_attack_release_ms(word: &StageLyricWord) -> Option<(u64, u64)> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    Some((
        (duration_ms as f32 * 0.22).clamp(280.0, 440.0) as u64,
        (duration_ms as f32 * 0.20).clamp(260.0, 420.0) as u64,
    ))
}

fn sustained_tail_end_timestamp(word: &StageLyricWord) -> Option<u64> {
    if !should_emphasize_sustained_word(word) {
        return None;
    }
    let duration_ms = word.duration_ms?;
    let (_, release_ms) = sustained_attack_release_ms(word)?;
    Some(
        word.timestamp_ms
            .saturating_add(duration_ms)
            .saturating_add(release_ms),
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

/// A single stable flex item per authored syllable.
///
/// Past/future words use ONE GPUI text element, not an invisible word, a
/// duplicate foreground, a glow layer and several nested animations. Only
/// the currently revealing syllable uses one dim text + one clipped text.
fn karaoke_word(
    word: &StageLyricWord,
    index: usize,
    next_word_start_ms: Option<u64>,
    reveal_progress: f32,
    position_ms: u64,
    is_current_word: bool,
    is_last_word: bool,
    karaoke_epoch: u64,
    playback_animating: bool,
) -> gpui::AnyElement {
    let progress = reveal_progress.clamp(0.0, 1.0);
    let animate = is_current_word
        && playback_animating
        && word_reveal_remaining_duration(word, position_ms).is_some();
    let visual_word = bounded_sustained_word(word, next_word_start_ms);
    let emphasize = should_emphasize_sustained_word(word)
        && (is_current_word || sustained_tail_active(word, &visual_word, position_ms));
    // Never replace the animated glyph at precisely 100% reveal. Keep its
    // transform and glow until the post-completion settling tail finishes.
    if progress >= 1.0 && !animate && !emphasize {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(dark::white(1.0))
            .child(word.text.clone())
            .into_any_element();
    }
    if progress <= 0.0 && !animate && !emphasize {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(dark::white(0.64))
            .child(word.text.clone())
            .into_any_element();
    }

    let highlighted = karaoke_reveal_word(
        div()
            .whitespace_nowrap()
            .text_color(dark::white(1.0))
            .child(word.text.clone()),
        word,
        index,
        progress,
        position_ms,
        animate,
        karaoke_epoch,
    );
    // The same scalar drives zoom and bloom through the full reveal and
    // immediately following settle. Amplitude stays tied to original duration.
    let emphasis = if !emphasize {
        SustainedWordEmphasis {
            scale: 1.0,
            ..SustainedWordEmphasis::default()
        }
    } else if visual_word.duration_ms == word.duration_ms {
        sustained_word_emphasis(word, position_ms, true, is_last_word)
    } else {
        sustained_visual_emphasis(word, &visual_word, position_ms, is_last_word)
    };
    let mut root = div().relative().flex_none().whitespace_nowrap();
    if emphasize {
        let peak = sustained_word_peak_emphasis(word, is_last_word);
        root = root.child(
            div()
                .absolute()
                .inset_0()
                .opacity(emphasis.glow_alpha)
                .text_color(dark::white(1.0))
                .filter_blur(px(peak.glow_blur_px.clamp(2.0, 4.0)))
                .child(word.text.clone()),
        );
    }
    let root = root
        .child(div().text_color(dark::white(0.64)).child(word.text.clone()))
        .child(div().absolute().inset_0().child(highlighted));
    if emphasize {
        return root
            .scale(emphasis.scale.clamp(1.0, 1.035))
            .transform_origin(TransformOrigin::new(0.0, 1.0))
            .into_any_element();
    }
    root.into_any_element()
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
        let progress = match karaoke_state {
            KaraokeLineState::Static | KaraokeLineState::Past => 1.0,
            KaraokeLineState::Future => 0.0,
            KaraokeLineState::Active => match current_word {
                Some(current) if index < current => 1.0,
                Some(current) if index == current => word_reveal_progress(word, position_ms),
                _ => 0.0,
            },
        };

        overlay = overlay.child(karaoke_word(
            word,
            index,
            line.words.get(index + 1).map(|next| next.timestamp_ms),
            progress,
            position_ms,
            karaoke_state == KaraokeLineState::Active && current_word == Some(index),
            index + 1 == line.words.len(),
            karaoke_epoch,
            animate,
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
    // Enhanced lines must be laid out exactly once. A full-sentence text
    // under an independently flex-wrapped word overlay breaks at different
    // positions (especially with long English words), making two copies of
    // the playing lyric overlap.
    if line.enhanced_complete && karaoke_detail {
        if let Some(active_overlay) = active_karaoke_overlay {
            return div()
                .w_full()
                .min_w(px(0.0))
                .child(active_overlay)
                .into_any_element();
        }
        return karaoke_words_overlay(line, karaoke_state, None, 0, false, karaoke_epoch)
            .into_any_element();
    }

    let base_alpha = match karaoke_state {
        KaraokeLineState::Static | KaraokeLineState::Past | KaraokeLineState::Active => 1.0,
        KaraokeLineState::Future => 0.68,
    };
    div()
        .w_full()
        .min_w(px(0.0))
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(dark::white(base_alpha))
        .child(line.text.clone())
        .into_any_element()
}

/// Convert high-resolution trackpad deltas into bounded logical rows.
/// Positive delta scrolls toward earlier lines; negative toward later ones.
fn take_reading_wheel_steps(remainder: &mut f32, delta: f32) -> isize {
    if !delta.is_finite() {
        return 0;
    }
    *remainder = (*remainder + delta.clamp(-192.0, 192.0)).clamp(-192.0, 192.0);
    let rows = ((*remainder).abs() / READING_WHEEL_STEP_PX)
        .floor()
        .min(MAX_READING_SCROLL_ROWS_PER_EVENT as f32) as isize;
    if rows == 0 {
        return 0;
    }
    let signed = if *remainder < 0.0 { -rows } else { rows };
    *remainder -= signed as f32 * READING_WHEEL_STEP_PX;
    signed
}

/// Fast first-paint geometry for 28px semibold lyrics.
///
/// This estimate feeds the primary container's min-height, so overestimating
/// a single line as two lines inserts a visible 38px gap before translation.
/// Keep the character advances close to actual glyph widths and let the
/// bounds observer grow genuinely wrapped rows. No per-frame shaping needed.
fn approximate_lyric_lines(text: &str, width_px: f32, font_px: f32) -> usize {
    // The previous 80% width budget plus 0.76em ASCII advance incorrectly
    // classified a one-line lyric as multiline at normal desktop widths.
    // Retain only a small safety margin; actual GPUI layout is authoritative.
    let max_width = (width_px * 0.97).max(font_px);
    let mut lines = 0usize;
    for paragraph in text.split('\n') {
        lines += 1;
        let mut width = 0.0f32;
        for ch in paragraph.chars() {
            let glyph_width = if ch.is_whitespace() {
                font_px * 0.36
            } else if ch.is_ascii() {
                if matches!(ch, 'M' | 'W' | 'm' | 'w' | '@' | '%') {
                    font_px * 0.80
                } else if matches!(
                    ch,
                    'i' | 'l' | 'I' | '1' | '.' | ',' | '\'' | '!' | ':' | ';'
                ) {
                    font_px * 0.30
                } else {
                    font_px * 0.53
                }
            } else {
                font_px
            };
            if width > 0.0 && width + glyph_width > max_width {
                lines += 1;
                width = 0.0;
            }
            width += glyph_width;
        }
    }
    lines.max(1)
}

fn estimated_primary_lyric_height(line: &StageLyricLine, width: f32) -> f32 {
    approximate_lyric_lines(line.text.as_str(), width, 28.0) as f32 * 38.0
}

fn estimated_lyric_row_height(line: &StageLyricLine, width: f32) -> f32 {
    let primary = estimated_primary_lyric_height(line, width);
    let translation = line.translation.as_ref().map_or(0.0, |text| {
        approximate_lyric_lines(text.as_str(), width, 15.0) as f32 * 21.0 + 4.0
    });
    // Keep measured/estimated rows in the same coordinate system. A
    // scaled karaoke leaf can be taller than its initial cached bounds.
    (primary + translation + 52.0).max(PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX)
}

/// Resolve the lyric column before measuring wrapped text. Current Taffy
/// 0.11 can otherwise measure at the outer flex width and paint at the
/// content width after 16px left + 104px right padding.
#[inline]
fn stage_lyric_content_width(viewport_width: f32, window_width: f32) -> f32 {
    let column_width = if viewport_width > 1.0 {
        viewport_width
    } else {
        window_width * 0.55
    };
    (column_width - 120.0).max(1.0)
}

/// Keep one measured row outside either endpoint of an interrupted scroll.
#[inline]
fn lyric_row_intersects_viewport(
    from_top: f32,
    to_top: f32,
    row_height: f32,
    viewport_height: f32,
) -> bool {
    let overscan = PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX;
    let bottom = from_top.max(to_top) + row_height;
    let top = from_top.min(to_top);
    bottom >= -overscan && top <= viewport_height + overscan
}

#[inline]
fn lyric_stack_scroll_offset(prefix_sum: &[f32], index: usize) -> f32 {
    prefix_sum
        .get(index)
        .copied()
        .unwrap_or(index as f32 * PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX)
}

#[inline]
fn playback_stack_row_top(index: usize, active: usize, prefix_sum: &[f32], anchor_y: f32) -> f32 {
    anchor_y + lyric_stack_scroll_offset(prefix_sum, index)
        - lyric_stack_scroll_offset(prefix_sum, active)
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
        * smoothstep01((1.0 - y) / LYRIC_VIEWPORT_FADE_BOTTOM_RATIO.max(0.001));

    (
        fade.clamp(0.0, 1.0),
        quantize_viewport_blur(LYRIC_VIEWPORT_MAX_BLUR_PX * blur_k),
    )
}

fn enhanced_words_cover_primary_text(line: &LyricLine) -> bool {
    if line.words.is_empty() || line.text.is_empty() {
        return false;
    }
    // Timed provider syllables may omit spaces while the display transcript
    // includes them (or vice versa). Compare meaningful characters only;
    // never infer timing or allow mismatched punctuation/letters.
    line.words
        .iter()
        .flat_map(|word| word.text.chars().filter(|ch| !ch.is_whitespace()))
        .eq(line.text.chars().filter(|ch| !ch.is_whitespace()))
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
    fn enhanced_tokens_allow_spacing_variants_but_not_different_lyrics() {
        let words = ["And", "all", "the", "dreams"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| LyricWord {
                timestamp_ms: index as u64 * 100,
                duration_ms: Some(100),
                text: text.to_owned(),
            })
            .collect::<Vec<_>>();
        let mut line = LyricLine {
            timestamp_ms: 0,
            text: "And all the dreams".to_owned(),
            translation: None,
            words: words.into(),
        };
        assert!(enhanced_words_cover_primary_text(&line));
        line.text = "And all the nights".to_owned();
        assert!(!enhanced_words_cover_primary_text(&line));
    }

    #[test]
    fn english_timed_word_spacing_keeps_source_immutable() {
        let words = ["and", "we'll", "get", "it", "right"]
            .into_iter()
            .enumerate()
            .map(|(index, text)| LyricWord {
                timestamp_ms: index as u64 * 100,
                duration_ms: Some(100),
                text: text.into(),
            })
            .collect::<Vec<_>>();
        let line = LyricLine {
            timestamp_ms: 0,
            text: "andwe'llgetitright".to_owned(),
            translation: None,
            words: words.into(),
        };
        let stage = StageLyricLine::from_source(&line);
        assert_eq!(stage.text.as_ref(), "and we'll get it right");
        assert_eq!(stage.words[0].text.as_ref(), "and ");
        assert_eq!(line.text, "andwe'llgetitright");
    }

    #[test]
    fn cjk_and_existing_spaces_are_preserved() {
        let words = |parts: &[&str]| {
            parts
                .iter()
                .enumerate()
                .map(|(index, text)| LyricWord {
                    timestamp_ms: index as u64 * 100,
                    duration_ms: Some(100),
                    text: (*text).to_owned(),
                })
                .collect::<Vec<_>>()
        };
        let cjk = words(&["我", "爱"]);
        assert_eq!(display_word_segment(&cjk, 0), "我");
        let english = words(&["I ", "know"]);
        assert_eq!(display_word_segment(&english, 0), "I ");
    }

    #[test]
    fn dense_enhanced_lyrics_fall_back_to_single_text_layout() {
        let words = (0..=MAX_KARAOKE_WORDS_PER_LINE)
            .map(|index| LyricWord {
                timestamp_ms: index as u64 * 10,
                duration_ms: Some(10),
                text: "a".to_string(),
            })
            .collect::<Vec<_>>();
        let line = LyricLine {
            timestamp_ms: 0,
            text: "a".repeat(words.len()),
            translation: None,
            words: words.into(),
        };
        assert!(!StageLyricLine::from_source(&line).enhanced_complete);
    }

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
    fn desktop_sunshine_lyrics_keep_translation_close_to_primary() {
        // Regression: a 1115px stage gives the lyric text about 495px.
        // These examples had been allocated 2/2/3 rows despite painting 1/1/2.
        let width = 495.0;
        let first = StageLyricLine::from_plain("Running through this strange life");
        let second = StageLyricLine::from_plain("Chasing all them green lights");
        let third = StageLyricLine::from_plain(
            "Throwing off the shade for a little bit of sunshine",
        );
        assert_eq!(approximate_lyric_lines(first.text.as_str(), width, 28.0), 1);
        assert_eq!(approximate_lyric_lines(second.text.as_str(), width, 28.0), 1);
        assert_eq!(approximate_lyric_lines(third.text.as_str(), width, 28.0), 2);
        assert_eq!(estimated_primary_lyric_height(&second, width), 38.0);

        let mut translated = second;
        translated.translation = Some(SharedString::from("畅想着通行无阻"));
        let row = estimated_lyric_row_height(&translated, width);
        assert!((row - (38.0 + 25.0 + 52.0)).abs() < 0.01);
    }

    #[test]
    fn multiline_english_and_chinese_translation_reserve_distinct_row_space() {
        let mut line = StageLyricLine::from_plain("We were both young when I first saw you");
        line.translation = Some(SharedString::from("第一次见到你的时候我们都还很年轻"));
        let width = 450.0;
        assert!(approximate_lyric_lines(line.text.as_str(), width, 28.0) >= 2);
        let primary = estimated_primary_lyric_height(&line, width);
        let row = estimated_lyric_row_height(&line, width);
        assert!(primary >= 76.0);
        assert!(row >= primary + 25.0 + 40.0);
        let prefix = [0.0, row, row + 82.0];
        let current = playback_stack_row_top(0, 0, &prefix, 310.0);
        let next = playback_stack_row_top(1, 0, &prefix, 310.0);
        assert!(next - current >= row);
    }

    #[test]
    fn active_karaoke_long_english_and_translation_do_not_share_row_space() {
        let mut line = StageLyricLine::from_plain("I'll be waiting all there's left to do is run");
        line.translation = Some(SharedString::from("我会一直等到最后"));
        let width = 380.0;
        let primary = estimated_primary_lyric_height(&line, width);
        let row = estimated_lyric_row_height(&line, width);
        assert!(
            primary >= 76.0,
            "English lyric must reserve at least two lines"
        );
        assert!(row >= primary + 25.0 + 52.0);
        let prefix = [0.0, row, row + 82.0];
        assert!(
            playback_stack_row_top(1, 0, &prefix, 300.0)
                - playback_stack_row_top(0, 0, &prefix, 300.0)
                >= row
        );
    }

    #[test]
    fn long_translations_reserve_multiple_lines() {
        let mut line = StageLyricLine::from_plain("I'll be waiting");
        line.translation = Some(SharedString::from(
            "我会一直在这里等待，直到最后一个夜晚过去，我仍会等着你",
        ));
        let narrow = estimated_lyric_row_height(&line, 215.0);
        let wide = estimated_lyric_row_height(&line, 580.0);
        assert!(narrow > wide);
        assert!(
            approximate_lyric_lines(line.translation.as_ref().unwrap().as_str(), 215.0, 17.0,) >= 2
        );
    }

    #[test]
    fn lyric_width_uses_the_measured_stage_column() {
        assert_eq!(stage_lyric_content_width(700.0, 1200.0), 580.0);
        // The lyric column reserves 120px of inner horizontal padding.
        assert_eq!(stage_lyric_content_width(200.0, 1200.0), 80.0);
    }

    #[test]
    fn moving_lyric_overscan_keeps_entering_rows_and_culls_far_rows() {
        assert!(lyric_row_intersects_viewport(-110.0, 40.0, 82.0, 600.0));
        assert!(lyric_row_intersects_viewport(670.0, 550.0, 82.0, 600.0));
        assert!(!lyric_row_intersects_viewport(900.0, 850.0, 82.0, 600.0));
        assert!(!lyric_row_intersects_viewport(-400.0, -300.0, 82.0, 600.0));
    }

    #[test]
    fn measured_lyric_rows_keep_a_full_line_spacing_floor() {
        // Short compositor/text-only measurements must not collapse absolute
        // row slots below the baseline for ordinary 28px lyric text.
        let baseline = PLAYBACK_STACK_DEFAULT_ROW_HEIGHT_PX;
        assert_eq!(baseline.max(28.0), baseline);
        let heights = [baseline, baseline.max(116.0), baseline];
        let mut prefix = vec![0.0];
        for height in heights {
            prefix.push(prefix.last().copied().unwrap_or_default() + height);
        }
        let top = playback_stack_row_top(1, 1, &prefix, 320.0);
        let next = playback_stack_row_top(2, 1, &prefix, 320.0);
        assert!(next - top >= 116.0);
    }

    #[test]
    fn prefix_sum_geometry_uses_real_row_heights() {
        // `prefix` is the running sum of the row heights [82, 110, 90].
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
    fn all_rows_move_as_one_continuous_scroll() {
        let prefix = [0.0, 82.0, 206.0, 300.0];
        let now = Instant::now();
        let motion = LyricPlaybackStackHandoff {
            from_scroll_px: prefix[1],
            to_scroll_px: prefix[2],
            first_index: 0,
            last_index: 2,
            started_at: now,
        };
        let anchor_y = 300.0;
        let second = playback_stack_row_top(1, 2, &prefix, anchor_y);
        let third = playback_stack_row_top(2, 2, &prefix, anchor_y);
        for time in [
            now,
            now + motion.duration() / 4,
            now + motion.duration() / 2,
            now + motion.duration(),
        ] {
            let offset = motion.to_scroll_px - motion.scroll_px(time);
            let second_visual = second + offset;
            let third_visual = third + offset;
            // Every row keeps its exact spacing through the entire scroll.
            assert!((third_visual - second_visual - (prefix[2] - prefix[1])).abs() < 0.001);
        }
        assert_eq!(motion.scroll_px(now), prefix[1]);
        assert_eq!(motion.scroll_px(now + motion.duration()), prefix[2]);
    }

    #[test]
    fn interrupted_scroll_uses_sampled_pixel_offset() {
        let now = Instant::now();
        let first = LyricPlaybackStackHandoff {
            from_scroll_px: 82.0,
            to_scroll_px: 206.0,
            first_index: 0,
            last_index: 3,
            started_at: now,
        };
        let half_time = now + first.duration() / 2;
        let sampled = first.scroll_px(half_time);
        assert!(sampled > 82.0 && sampled < 206.0);
        let second = LyricPlaybackStackHandoff {
            from_scroll_px: sampled,
            to_scroll_px: 300.0,
            first_index: 0,
            last_index: 4,
            started_at: half_time,
        };
        assert!((second.scroll_px(half_time) - sampled).abs() < 0.001);
        assert!((second.scroll_px(half_time + second.duration()) - 300.0).abs() < 0.001);
    }

    #[test]
    fn active_lyric_alignment_uses_forty_two_percent_of_local_viewport() {
        let viewport_height = 600.0;
        let prefix = [0.0, 82.0, 164.0];
        let anchor = viewport_height * LYRIC_ANCHOR_RATIO;
        assert_eq!(LYRIC_ANCHOR_RATIO, 0.42);
        assert!((playback_stack_row_top(1, 1, &prefix, anchor) - 252.0).abs() < 0.001);
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
        // The fade and blur fields are continuous between sampled rows,
        // with stronger depth cues near the viewport extremes.
        assert!(top.0 < 0.4);
        assert!(bottom.0 < 0.4);
        assert!(top.1 > 0.0 && top.1 <= LYRIC_VIEWPORT_MAX_BLUR_PX);
        assert!(bottom.1 > 0.0 && bottom.1 <= LYRIC_VIEWPORT_MAX_BLUR_PX);
        assert_eq!(quantize_viewport_blur(0.20), 0.0);
        assert_eq!(quantize_viewport_blur(8.0), LYRIC_VIEWPORT_MAX_BLUR_PX);
    }

    #[test]
    fn plain_active_lyric_is_not_dimmed_when_word_timing_is_missing() {
        let line = StageLyricLine::from_plain("无逐字时间戳");
        assert!(!line.enhanced_complete);
        assert_eq!(lyric_focus_alpha(0.0), 1.0);
        assert!(lyric_focus_alpha(0.0) > 2.0 * lyric_focus_alpha(1.0));
    }

    #[test]
    fn routine_progress_does_not_restart_karaoke_reveal() {
        assert!(!karaoke_seek_discontinuity(1_000, 1_250, false, false));
        assert!(!karaoke_seek_discontinuity(1_000, 2_000, false, false));
        assert!(karaoke_seek_discontinuity(1_000, 4_000, false, false));
        assert!(karaoke_seek_discontinuity(4_000, 4_000, false, true));
        assert!(karaoke_seek_discontinuity(4_000, 4_000, true, false));
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
    fn trackpad_scroll_accumulates_without_skipping_rows_on_tiny_events() {
        let mut remainder = 0.0;
        assert_eq!(take_reading_wheel_steps(&mut remainder, -12.0), 0);
        assert_eq!(take_reading_wheel_steps(&mut remainder, -12.0), 0);
        assert_eq!(take_reading_wheel_steps(&mut remainder, -24.0), -1);
        assert_eq!(remainder, 0.0);
        assert_eq!(take_reading_wheel_steps(&mut remainder, 48.0), 1);
        assert_eq!(take_reading_wheel_steps(&mut remainder, -192.0), -3);
    }

    #[test]
    fn multiline_height_offsets_every_following_row() {
        let prefix = [0.0, 82.0, 82.0 + 156.0, 82.0 + 156.0 + 92.0];
        let anchor = 260.0;
        let first = playback_stack_row_top(0, 0, &prefix, anchor);
        let second = playback_stack_row_top(1, 0, &prefix, anchor);
        let third = playback_stack_row_top(2, 0, &prefix, anchor);
        assert!((second - first - 82.0).abs() < 0.01);
        assert!((third - second - 156.0).abs() < 0.01);
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
        let completed = sustained_time_envelope(&word, 3_000);
        let release = sustained_time_envelope(&word, 3_200);

        assert!(attack > 0.0 && attack < 1.0);
        assert_eq!(sustain, 1.0);
        assert_eq!(completed, 1.0);
        assert!(release > 0.0 && release < 1.0);
        assert_eq!(sustained_time_envelope(&word, 3_400), 0.0);
    }

    #[test]
    fn sustained_animation_samples_only_the_attack_and_release() {
        let word = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(2_000),
            byte_start: 0,
            byte_end: 3,
            text: SharedString::from("啊"),
        };
        assert_eq!(sustained_transition_frame_delay(&word, 999), None);
        assert_eq!(
            sustained_transition_frame_delay(&word, 1_000),
            Some(SUSTAINED_TRANSITION_FRAME_INTERVAL)
        );
        assert_eq!(
            sustained_transition_frame_delay(&word, 1_180),
            Some(SUSTAINED_TRANSITION_FRAME_INTERVAL)
        );
        assert_eq!(sustained_transition_frame_delay(&word, 2_000), None);
        assert_eq!(sustained_transition_frame_delay(&word, 2_850), None);
        assert_eq!(
            sustained_transition_frame_delay(&word, 3_000),
            Some(SUSTAINED_TRANSITION_FRAME_INTERVAL)
        );
        assert_eq!(
            sustained_transition_frame_delay(&word, 3_200),
            Some(SUSTAINED_TRANSITION_FRAME_INTERVAL)
        );
        assert_eq!(sustained_transition_frame_delay(&word, 3_400), None);
    }

    #[test]
    fn sustained_envelope_is_continuous_at_each_phase_boundary() {
        let word = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(2_000),
            byte_start: 0,
            byte_end: 3,
            text: SharedString::from("啊"),
        };
        let (attack, release) = sustained_attack_release_ms(&word).unwrap();
        let attack_end = word.timestamp_ms + attack;
        let word_end = word.timestamp_ms + 2_000;
        let release_end = word_end + release;
        let times = [
            (attack_end - 1, attack_end),
            (word_end - 1, word_end),
            (release_end - 1, release_end),
        ];
        for (before, after) in times {
            let previous = sustained_time_envelope(&word, before);
            let current = sustained_time_envelope(&word, after);
            assert!(
                (current - previous).abs() < 0.002,
                "envelope jumps between {before}ms and {after}ms"
            );
        }
    }

    #[test]
    fn overlapping_syllables_release_before_next_word_without_moving_mask() {
        let original = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(3_000),
            byte_start: 0,
            byte_end: 3,
            text: SharedString::from("啊"),
        };
        let visual = bounded_sustained_word(&original, Some(2_600));
        assert_eq!(visual.duration_ms, Some(1_600));
        let tail_end = sustained_tail_end_timestamp(&visual).unwrap();
        assert!(tail_end > 2_600);
        assert_eq!(sustained_time_envelope(&visual, 2_500), 1.0);
        assert_eq!(sustained_time_envelope(&visual, 2_600), 1.0);
        assert!(sustained_tail_active(&original, &visual, 2_600));
        let at_completion = sustained_visual_emphasis(&original, &visual, 2_600, false);
        let settling = sustained_visual_emphasis(&original, &visual, (2_600 + tail_end) / 2, false);
        assert!(at_completion.scale > settling.scale && settling.scale > 1.0);
        assert!(at_completion.glow_alpha > settling.glow_alpha && settling.glow_alpha > 0.0);
        assert!(!sustained_tail_active(&original, &visual, tail_end));
        assert_eq!(sustained_time_envelope(&visual, tail_end), 0.0);
        // The original timed-word reveal still uses its source duration.
        assert!((word_reveal_progress(&original, 2_500) - 0.5).abs() < 0.001);

        let short = bounded_sustained_word(&original, Some(1_600));
        assert!(!should_emphasize_sustained_word(&short));
        assert!(sustained_visual_emphasis(&original, &short, 1_600, false).scale > 1.0);
        assert_eq!(
            bounded_sustained_word(&original, None).duration_ms,
            Some(3_000)
        );
    }

    #[test]
    fn sustained_scale_uses_full_attack_release_without_early_clipping() {
        let word = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(4_000),
            byte_start: 0,
            byte_end: 3,
            text: SharedString::from("啊"),
        };
        let (attack, release) = sustained_attack_release_ms(&word).unwrap();
        let early = sustained_word_emphasis(&word, 1_000 + attack / 4, true, true);
        let middle = sustained_word_emphasis(&word, 1_000 + attack / 2, true, true);
        let late = sustained_word_emphasis(&word, 1_000 + attack * 3 / 4, true, true);
        let held = sustained_word_emphasis(&word, 3_000, true, true);
        assert!(1.0 < early.scale && early.scale < middle.scale);
        assert!(middle.scale < late.scale && late.scale < held.scale);
        assert!((held.scale - 1.035).abs() < 0.0001);

        let completed = sustained_word_emphasis(&word, 5_000, true, true);
        let settling = sustained_word_emphasis(&word, 5_000 + release / 2, true, true);
        assert!((completed.scale - held.scale).abs() < 0.0001);
        assert!(1.0 < settling.scale && settling.scale < held.scale);
        assert_eq!(
            sustained_word_emphasis(&word, 5_000 + release, true, true).scale,
            1.0
        );
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
        let completed = sustained_word_emphasis(&word, 8_000, true, false);
        let (_, release) = sustained_attack_release_ms(&word).unwrap();
        let settling = sustained_word_emphasis(&word, 8_000 + release / 2, true, false);
        let ended = sustained_word_emphasis(&word, 8_000 + release, true, false);

        assert!(middle.glow_alpha > 0.0);
        assert!(middle.scale > 1.0);
        assert_eq!(near_end.glow_alpha, middle.glow_alpha);
        assert_eq!(completed.scale, middle.scale);
        assert!(settling.glow_alpha > 0.0 && settling.glow_alpha < middle.glow_alpha);
        assert!(settling.scale > 1.0 && settling.scale < middle.scale);
        assert_eq!(ended.glow_alpha, 0.0);
        assert_eq!(ended.scale, 1.0);
    }

    #[test]
    fn sustained_completion_uses_zero_velocity_curve_and_does_not_snap() {
        let word = StageLyricWord {
            timestamp_ms: 1_000,
            duration_ms: Some(2_000),
            byte_start: 0,
            byte_end: 3,
            text: SharedString::from("啊"),
        };
        let (attack_ms, release_ms) = sustained_attack_release_ms(&word).unwrap();
        let attack_end = 1_000 + attack_ms;
        let end_ms = 3_000;
        let final_ms = end_ms + release_ms;

        for (before, after) in [
            (attack_end - 1, attack_end),
            (end_ms - 1, end_ms),
            (final_ms - 1, final_ms),
        ] {
            let first = sustained_word_emphasis(&word, before, true, true);
            let second = sustained_word_emphasis(&word, after, true, true);
            assert!((first.scale - second.scale).abs() < 0.0001);
            assert!((first.glow_alpha - second.glow_alpha).abs() < 0.0001);
        }
        assert!((smootherstep01(0.5) - 0.5).abs() < 0.0001);
        assert!(smootherstep01(0.1) < smoothstep01(0.1));
        assert!(smootherstep01(0.9) > smoothstep01(0.9));
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

        let release = sustained_tail_end_timestamp(&line.words[0]).unwrap();
        assert_eq!(
            next_karaoke_timestamp(&line, 1_100),
            Some(release.min(3_000))
        );
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
