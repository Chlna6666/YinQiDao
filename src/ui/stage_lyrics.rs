use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, BorrowAppContext as _,
    Context, Easing, ElementId, Entity, FillMode, Global, HorizontalRevealEdge,
    IntoElement, ListAlignment, ListOffset, ListState, Render, SharedString,
    Transition, TransitionProperty, WeakEntity, Window, div, hsla, list, point, prelude::*, px,
};
use lucide_gpui::icon;

use crate::{
    audio::AudioEngine,
    lyrics::LyricLine,
    model::{PlaybackState, TrackId},
};

use super::{shell::MusicApp, theme::themed_icon};

const READING_MODE_DURATION: Duration = Duration::from_secs(3);
const LIST_OVERDRAW_PX: f32 = 360.0;
const LYRIC_ANCHOR_RATIO: f32 = 0.43;
const LYRIC_LIST_PADDING_TOP: f32 = 8.0;
const LYRIC_LIST_PADDING_BOTTOM: f32 = 10.0;
const LYRIC_VIEWPORT_FADE_TOP_PX: f32 = 128.0;
const LYRIC_VIEWPORT_FADE_BOTTOM_PX: f32 = 150.0;
const LYRIC_HANDOFF_DURATION: Duration = Duration::from_millis(410);
const LYRIC_ROW_MOTION_DURATION: Duration = Duration::from_millis(270);
const LYRIC_ROW_STAGGER_MS: u64 = 18;
const LYRIC_ROW_MAX_STAGGER_ROWS: usize = 6;
const LYRIC_DEPTH_TRANSITION_DURATION: Duration = Duration::from_millis(190);
const LYRIC_OPACITY_TRANSITION_DURATION: Duration = Duration::from_millis(230);
const SCROLL_SETTLE_PX: f32 = 0.30;
const TRANSPORT_MIN_SLEEP: u64 = 8;
// Blur/opacity now use the Nova GPU driver. Keep enough neighboring rows in the same hand-off so
// edge depth never snaps while translation is still cascading.
const LYRIC_BLUR_TRANSITION_RADIUS: usize = 2;
const LYRIC_OPACITY_TRANSITION_RADIUS: usize = 9;

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

#[derive(Clone, Copy)]
struct LyricScrollAnimation {
    from_y: f32,
    started_at: Instant,
}

impl LyricScrollAnimation {
    fn progress_at(self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started_at);
        lyric_row_motion_spec(Duration::ZERO)
            .sample_elapsed(elapsed)
            .eased_progress
    }

    fn offset_at(self, now: Instant) -> f32 {
        self.from_y * (1.0 - self.progress_at(now))
    }
}

pub(super) fn view(app: &MusicApp, cx: &mut Context<MusicApp>) -> Entity<StageLyricsView> {
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
    focus_from_index: Option<usize>,
    focus_started_at: Option<Instant>,
    active_word_index: Option<usize>,
    hovered_index: Option<usize>,
    karaoke_epoch: u64,
    transport_generation: u64,
    reading_until: Option<Instant>,
    scroll_target: Option<usize>,
    scroll_animation: Option<LyricScrollAnimation>,
    motion_epoch: u64,
    // Initial stage/source materialization is aligned offscreen first. It must never reuse the
    // normal line-change FLIP animation, otherwise the first active lyric visibly starts near the
    // top edge and then flies into the focus slot.
    anchor_bootstrap_pending: bool,
    stage_active: bool,
    scrubbing: bool,
}

impl StageLyricsView {
    fn new(parent: WeakEntity<MusicApp>, engine: Option<Arc<AudioEngine>>) -> Self {
        let transport_generation = engine
            .as_ref()
            .map_or(0, |engine| engine.transport_generation());
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
            focus_from_index: None,
            focus_started_at: None,
            active_word_index: None,
            hovered_index: None,
            karaoke_epoch: 0,
            transport_generation,
            reading_until: None,
            scroll_target: None,
            scroll_animation: None,
            motion_epoch: 0,
            anchor_bootstrap_pending: false,
            stage_active: false,
            scrubbing: false,
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
        let source = track_id
            .and_then(|id| app.lyrics.get(&id))
            .map_or(&[][..], |document| document.timed_lines());
        let source_ptr = source.as_ptr() as usize;
        let source_len = source.len();
        let source_changed = self.track_id != track_id
            || self.source_ptr != source_ptr
            || self.source_len != source_len;
        let playback_state_changed = self.playback_state != app.snapshot.state;
        let stage_active_changed = self.stage_active != stage_active;
        let scrubbing = app.drag_progress_ratio.is_some();
        let scrubbing_changed = self.scrubbing != scrubbing;

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
            self.focus_from_index = None;
            self.focus_started_at = None;
            self.active_word_index = None;
            self.hovered_index = None;
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            self.reading_until = None;
            self.scroll_target = None;
            self.cancel_scroll_animation();
            self.anchor_bootstrap_pending = stage_active;
            changed = true;
        }
        if transport_changed && !source_changed {
            // Seek/restore commands update AudioEngine's optimistic position immediately. A separate
            // generation lets the compositor timeline restart even when the target stays inside the
            // same authored word and therefore does not change active_word_index.
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            changed = true;
        }

        let live_position_ms = self
            .engine
            .as_ref()
            .map_or(app.snapshot.position_ms, |engine| engine.progress().1);
        let position_ms = app.drag_progress_ratio.map_or(live_position_ms, |ratio| {
            (app.snapshot.duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64
        });
        let previous_word = self.active_word_index;
        let position_changed = self.position_ms != position_ms;
        if position_changed {
            self.position_ms = position_ms;
        }
        if playback_state_changed {
            self.playback_state = app.snapshot.state;
            // Karaoke owns an independent epoch: pausing/resuming must not retrigger the active-line
            // focus scale animation. Only the word sweep restarts from the exact transport sample.
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            changed = true;
        }
        if scrubbing_changed {
            self.scrubbing = scrubbing;
            // During a drag the mask is sampled directly. Releasing creates a fresh retained
            // animation from the released transport position, so no stale timeline can catch up.
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
            changed = true;
        }
        if stage_active_changed {
            self.stage_active = stage_active;
            // Stage removal drops retained scene state. Do not keep an old focus source around:
            // otherwise reopening can recreate an already-finished hand-off from a stale line.
            self.focus_from_index = None;
            self.focus_started_at = None;
            if stage_active {
                // Re-entering Stage must not reuse an old retained word timeline that may have kept
                // aging while its scene subtree was absent. Align the current line invisibly first;
                // only subsequent semantic line changes get the Apple-style cascade.
                self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
                self.anchor_bootstrap_pending = self.active_index.is_some();
                if !self.is_reading() {
                    self.scroll_target = self.active_index;
                }
            } else {
                self.anchor_bootstrap_pending = false;
                self.cancel_scroll_animation();
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
        if active_changed || word_changed {
            changed = true;
        }

        if source_changed && let Some(active) = self.active_index {
            self.list_state.scroll_to(ListOffset {
                item_ix: active.saturating_sub(4),
                offset_in_item: px(0.0),
            });
            self.scroll_target = Some(active);
            self.anchor_bootstrap_pending = stage_active;
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

        let previous = self.active_index;
        if previous.is_none() && self.stage_active {
            // First materialization is positioning, not a lyric hand-off.
            self.focus_from_index = None;
            self.anchor_bootstrap_pending = active.is_some();
        } else {
            self.focus_from_index = previous;
        }
        self.focus_started_at = None;
        self.active_index = active;
        self.hovered_index = None;
        self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        if !self.is_reading() {
            self.scroll_target = active;
        }
        true
    }

    fn compute_active_word_index(&self) -> Option<usize> {
        if self.is_reading() {
            return None;
        }
        let line = self.active_index.and_then(|index| self.lines.get(index))?;
        active_enhanced_word_index(line, self.position_ms)
    }

    #[inline]
    fn transport_should_run(&self) -> bool {
        self.stage_active
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
        self.reading_until
            .is_some_and(|until| until > Instant::now())
    }

    fn cancel_scroll_animation(&mut self) {
        self.scroll_animation = None;
    }

    fn current_scroll_animation_offset(&self, now: Instant) -> f32 {
        self.scroll_animation
            .map_or(0.0, |animation| animation.offset_at(now))
    }

    fn start_scroll_animation(&mut self, from_y: f32, started_at: Instant) {
        if !from_y.is_finite() || from_y.abs() <= SCROLL_SETTLE_PX {
            self.cancel_scroll_animation();
            return;
        }

        self.motion_epoch = self.motion_epoch.wrapping_add(1);
        self.scroll_animation = Some(LyricScrollAnimation {
            from_y,
            started_at,
        });
    }

    fn begin_reading_mode(&mut self, cx: &mut Context<Self>) {
        self.reading_until = Some(Instant::now() + READING_MODE_DURATION);
        // Reading mode deliberately flattens the depth field. Drop any in-flight automatic
        // hand-off so returning to playback never replays a stale old->new focus animation.
        self.focus_from_index = None;
        self.focus_started_at = None;
        self.active_word_index = None;
        self.scroll_target = None;
        self.hovered_index = None;
        self.cancel_scroll_animation();
        cx.notify();
    }

    fn expire_deadlines(&mut self, now: Instant) {
        if self.reading_until.is_some_and(|until| until <= now) {
            self.reading_until = None;
            self.focus_from_index = None;
            self.focus_started_at = None;
            self.active_word_index = self.compute_active_word_index();
            self.scroll_target = self.active_index;
            self.karaoke_epoch = self.karaoke_epoch.wrapping_add(1);
        }
        if self
            .focus_started_at
            .is_some_and(|started_at| started_at + LYRIC_HANDOFF_DURATION <= now)
        {
            self.focus_from_index = None;
            self.focus_started_at = None;
        }
        if self
            .scroll_animation
            .is_some_and(|animation| animation.started_at + LYRIC_HANDOFF_DURATION <= now)
        {
            self.scroll_animation = None;
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
        if let Some(animation) = self.scroll_animation {
            let deadline = animation.started_at + LYRIC_HANDOFF_DURATION;
            if deadline > now {
                window.request_invalidation_at(deadline, cx);
            }
        }
        if let Some(started_at) = self.focus_started_at {
            let deadline = started_at + LYRIC_HANDOFF_DURATION;
            if deadline > now {
                window.request_invalidation_at(deadline, cx);
            }
        }
    }

    fn prepare_scroll_animation(&mut self, window: &mut Window) {
        if !self.stage_active || self.is_reading() {
            return;
        }
        let Some(target) = self.scroll_target else {
            return;
        };

        let viewport = self.list_state.viewport_bounds();
        if f32::from(viewport.size.height) <= 0.5 || window.is_minimized() {
            return;
        }

        let Some(line_bounds) = self.list_state.bounds_for_item(target) else {
            let adjacent_handoff = self
                .focus_from_index
                .is_some_and(|previous| previous.abs_diff(target) <= 2);

            if self.anchor_bootstrap_pending {
                self.list_state.scroll_to(ListOffset {
                    item_ix: target.saturating_sub(4),
                    offset_in_item: px(0.0),
                });
                self.cancel_scroll_animation();
            } else if !adjacent_handoff {
                // Large seeks may legitimately jump the virtual list close to the destination.
                self.list_state.scroll_to(ListOffset {
                    item_ix: target.saturating_sub(3),
                    offset_in_item: px(0.0),
                });
                self.cancel_scroll_animation();
            }

            // Normal playback must never call scroll_to_reveal_item here. That mutates logical
            // scroll immediately and causes the whole lyric field to jump before the hand-off
            // animation starts. Bootstrap stays invisible until this target has real geometry.
            if !window.is_minimized() && f32::from(viewport.size.height) > 1.0 {
                window.request_animation_frame();
            }
            return;
        };

        let viewport_top = f32::from(viewport.origin.y);
        let viewport_height = f32::from(viewport.size.height);
        let usable_height =
            (viewport_height - LYRIC_LIST_PADDING_TOP - LYRIC_LIST_PADDING_BOTTOM).max(1.0);
        let anchor_y =
            viewport_top + LYRIC_LIST_PADDING_TOP + usable_height * LYRIC_ANCHOR_RATIO;

        // GPUI ListState::bounds_for_item currently reports coordinates from the list bounds but
        // omits style padding.top, while prepaint_items actually starts every row after padding.top.
        // Compensate here so the active lyric's *painted* center lands on the visual anchor.
        let painted_line_center =
            f32::from(line_bounds.center().y) + LYRIC_LIST_PADDING_TOP;
        let diff = painted_line_center - anchor_y;
        let now = window.animation_time();

        if self.anchor_bootstrap_pending {
            if diff.abs() > SCROLL_SETTLE_PX {
                self.list_state.scroll_by(px(diff));
            }
            self.scroll_target = None;
            self.hovered_index = None;
            self.focus_from_index = None;
            self.focus_started_at = None;
            self.anchor_bootstrap_pending = false;
            self.cancel_scroll_animation();
            return;
        }
        if diff.abs() <= SCROLL_SETTLE_PX {
            self.scroll_target = None;
            if self.focus_from_index.is_some() && self.focus_started_at.is_none() {
                self.focus_started_at = Some(now);
                self.motion_epoch = self.motion_epoch.wrapping_add(1);
                window.request_animation_frame();
            }
            return;
        }

        let carry = self.current_scroll_animation_offset(now);
        let before = f32::from(self.list_state.scroll_px_offset_for_scrollbar().y);
        self.list_state.scroll_by(px(diff));
        let after = f32::from(self.list_state.scroll_px_offset_for_scrollbar().y);
        let applied = before - after;
        self.scroll_target = None;
        self.hovered_index = None;

        if applied.abs() <= SCROLL_SETTLE_PX {
            self.cancel_scroll_animation();
            if self.focus_from_index.is_some() && self.focus_started_at.is_none() {
                self.focus_started_at = Some(now);
                self.motion_epoch = self.motion_epoch.wrapping_add(1);
                window.request_animation_frame();
            }
            return;
        }

        // Focus/depth and list motion begin from the same platform-frame timestamp. Starting the
        // focus earlier (before ListState had target bounds) produced the video-visible state where
        // the next line became clear while the list was still parked at the old anchor.
        let started_at = self.focus_started_at.unwrap_or(now);
        self.focus_started_at = Some(started_at);
        self.start_scroll_animation(carry + applied, started_at);
    }
}

impl Render for StageLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let frame_now = window.animation_time();
        self.expire_deadlines(frame_now);
        self.refresh_transport();

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
                // Generic pointer wake is owned by stage-drawer-root. Lyric-row actions below
                // explicitly wake after stop_propagation, so interactive seeking remains intact.
                .child(themed_icon(icon!(music), 36.0, hsla(0.0, 0.0, 1.0, 0.25)))
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

        self.prepare_scroll_animation(window);
        self.schedule_deadlines(window, cx);

        let active = self.active_index.unwrap_or(0);
        let active_word_index = self.active_word_index;
        let position_ms = self.position_ms;
        let reading_mode = self.is_reading();
        let karaoke_running = self.stage_active
            && self.playback_state == PlaybackState::Playing
            && !self.scrubbing
            && !reading_mode;
        let scroll_animation = self.scroll_animation;
        let scroll_animating = scroll_animation.is_some();
        let scroll_from_y = scroll_animation.map_or(0.0, |scroll| scroll.from_y);
        let motion_epoch = self.motion_epoch;
        let focus_started = self.focus_started_at.is_some();
        let focus_animating = focus_started;
        // Automatic line hand-off keeps the depth field active. Disabling blur for the whole
        // automatic scroll used to make every line equally sharp during the transition, producing
        // visible "flat" frame from the immersive comparison. Only explicit reading mode removes
        // depth so manual browsing stays crisp.
        let depth_blur_active = !reading_mode;
        let text_id = "lyric-text";
        let karaoke_epoch = self.karaoke_epoch;
        let hovered_index = self.hovered_index;
        let focus_from_index = self.focus_from_index;
        let viewport_bounds = self.list_state.viewport_bounds();
        let lines = self.lines.clone();

        // Snapshot ListState geometry before constructing/rendering the List element. The list
        // renderer mutably borrows its internal RefCell while invoking item callbacks, so querying
        // bounds_for_item() from inside that callback would re-borrow the same RefCell and panic.
        // Previous-frame measured geometry is exactly what FLIP/fade needs as the visual source.
        const EDGE_GEOMETRY_RADIUS: usize = 16;
        let edge_snapshot_start = active.saturating_sub(EDGE_GEOMETRY_RADIUS);
        let edge_snapshot_end = active
            .saturating_add(EDGE_GEOMETRY_RADIUS + 1)
            .min(lines.len());
        let row_edge_progress: Arc<[Option<(f32, f32)>]> = if reading_mode {
            Arc::from([])
        } else {
            (edge_snapshot_start..edge_snapshot_end)
                .map(|index| {
                    self.list_state.bounds_for_item(index).map(|bounds| {
                        // ListState item bounds omit style padding.top while paint includes it.
                        let target_center_y =
                            f32::from(bounds.center().y) + LYRIC_LIST_PADDING_TOP;
                        let previous_center_y = target_center_y + scroll_from_y;
                        (
                            lyric_viewport_edge_progress(target_center_y, viewport_bounds),
                            lyric_viewport_edge_progress(previous_center_y, viewport_bounds),
                        )
                    })
                })
                .collect::<Vec<_>>()
                .into()
        };

        let view = cx.entity().downgrade();
        let parent = self.parent.clone();

        let lyrics = list(self.list_state.clone(), move |index, _window, _cx| {
            let (edge_progress, previous_edge_progress) = if reading_mode {
                (0.0, 0.0)
            } else {
                index
                    .checked_sub(edge_snapshot_start)
                    .and_then(|offset| row_edge_progress.get(offset))
                    .and_then(|progress| *progress)
                    .unwrap_or_else(|| {
                        // A newly materialized overdraw row has no previous-frame bounds yet. Use
                        // a conservative one-frame fallback; once measured, the next render uses
                        // exact viewport geometry.
                        let distance = index.abs_diff(active) as f32;
                        let fallback = smoothstep01((distance - 3.0) / 5.0);
                        (fallback, fallback)
                    })
            };

            render_lyric_row(
                &lines[index],
                index,
                active,
                focus_from_index,
                focus_started,
                scroll_animating,
                scroll_from_y,
                edge_progress,
                previous_edge_progress,
                motion_epoch,
                active_word_index,
                position_ms,
                reading_mode,
                karaoke_running,
                depth_blur_active,
                text_id,
                hovered_index == Some(index),
                !(scroll_animating || focus_animating),
                karaoke_epoch,
                view.clone(),
                parent.clone(),
            )
        })
        .size_full()
        .pt(px(LYRIC_LIST_PADDING_TOP))
        .pb(px(LYRIC_LIST_PADDING_BOTTOM))
        .pr(px(8.0))
        .opacity(if self.anchor_bootstrap_pending { 0.0 } else { 1.0 });

        let lyrics = lyrics.into_any_element();

        div()
            .id("stage-lyrics-view")
            .relative()
            .flex_1()
            .h_full()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, _: &gpui::ScrollWheelEvent, _, cx| {
                this.begin_reading_mode(cx);
                let _ = this
                    .parent
                    .update(cx, |app, cx| app.wake_stage_controls(cx));
            }))
            .child(lyrics)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_lyric_row(
    line: &StageLyricLine,
    index: usize,
    active: usize,
    focus_from_index: Option<usize>,
    focus_started: bool,
    scroll_animating: bool,
    scroll_from_y: f32,
    edge_progress: f32,
    previous_edge_progress: f32,
    motion_epoch: u64,
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
        reading_mode,
        depth_blur_active,
    );
    let timestamp = line.timestamp_ms;
    let karaoke_active = index == active && !reading_mode;

    let previous_active = focus_from_index.filter(|previous| *previous != active);
    let previous_profile = previous_active.map(|previous| {
        lyric_visual_profile(
            index,
            previous,
            previous_edge_progress,
            reading_mode,
            depth_blur_active,
        )
    });
    let animate_blur =
        lyric_blur_transition_bound(index, active, previous_active, reading_mode);
    let animate_opacity =
        lyric_opacity_transition_bound(index, active, previous_active, reading_mode);

    // Active may advance before the virtual list has target geometry. Keep the old style until the
    // hand-off actually begins. Once it begins, the stable lyric-text element changes to the new
    // alpha/blur target and GPUI/Nova owns both transitions on the GPU.
    let (resolved_alpha, resolved_blur) = if previous_active.is_some() && !focus_started {
        previous_profile.unwrap_or((target_alpha, target_blur))
    } else {
        (target_alpha, target_blur)
    };
    let resolved_alpha = if hovered { 1.0 } else { resolved_alpha };
    let resolved_blur = if hovered { 0.0 } else { resolved_blur };

    let mut text = lyric_text_layer(
        line,
        karaoke_active,
        active_word_index,
        position_ms,
        karaoke_running,
        karaoke_epoch,
        resolved_blur,
        text_id,
        index,
    )
    .opacity(resolved_alpha);

    if focus_started && !hovered {
        let delay = lyric_row_stagger_delay(index, active, previous_active, scroll_from_y);
        if animate_blur {
            text = text.transition(lyric_depth_transition(delay));
        } else if animate_opacity {
            text = text.transition(lyric_opacity_transition(delay));
        }
    }
    let text = text.into_any_element();

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
        return apply_lyric_row_motion(
            row,
            index,
            active,
            focus_from_index,
            scroll_animating,
            scroll_from_y,
            motion_epoch,
        );
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
            this.scroll_target = Some(index);
            cx.notify();
        });
        let _ = parent.update(cx, |app, cx| {
            app.seek_to_ms(timestamp, cx);
            app.wake_stage_controls_immediately(cx);
        });
    });

    apply_lyric_row_motion(
        row,
        index,
        active,
        focus_from_index,
        scroll_animating,
        scroll_from_y,
        motion_epoch,
    )
}

#[allow(clippy::too_many_arguments)]
fn lyric_text_layer(
    line: &StageLyricLine,
    karaoke_active: bool,
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
            karaoke_active,
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

#[inline]
fn lyric_viewport_edge_progress(
    row_center_y: f32,
    viewport: gpui::Bounds<gpui::Pixels>,
) -> f32 {
    let top = f32::from(viewport.origin.y);
    let height = f32::from(viewport.size.height).max(1.0);
    let bottom = top + height;
    let y = row_center_y.clamp(top, bottom);

    let top_visibility =
        smoothstep01((y - top) / LYRIC_VIEWPORT_FADE_TOP_PX.max(1.0));
    let bottom_visibility =
        smoothstep01((bottom - y) / LYRIC_VIEWPORT_FADE_BOTTOM_PX.max(1.0));

    1.0 - top_visibility.min(bottom_visibility)
}

fn lyric_visual_profile(
    index: usize,
    active: usize,
    edge_progress: f32,
    reading_mode: bool,
    depth_blur_active: bool,
) -> (f32, f32) {
    if reading_mode {
        return (1.0, 0.0);
    }

    let distance = index.abs_diff(active) as f32;
    let (focus_alpha, focus_blur) = lyric_focus_falloff(distance);
    let edge = smoothstep01(edge_progress);

    // Physical viewport edge owns the final fade. Near the actual clip boundary the glyphs become
    // almost transparent instead of merely blurred, so no bright half-line appears at top/bottom.
    let edge_alpha = 1.0 + (0.035 - 1.0) * edge;
    let alpha = (focus_alpha * edge_alpha).clamp(0.012, 1.0);

    let blur = if depth_blur_active {
        // Keep element-blur captures local to the focus neighborhood. Past roughly three rows the
        // text is already dim enough that opacity alone produces the edge-depth cue, while dropping
        // the Gaussian pass avoids a stack of offscreen blur layers during every hand-off.
        let blur_gate = 1.0 - smoothstep01((distance - 2.0) / 1.8);
        let edge_blur = 0.75 * edge;
        ((focus_blur + edge_blur) * blur_gate).min(2.35)
    } else {
        0.0
    };

    (alpha, blur)
}

#[inline]
fn lyric_focus_falloff(distance: f32) -> (f32, f32) {
    let d = distance.max(0.0);

    // Start fading immediately after the focused row. The previous smoothstep curve left row ±1 at
    // almost 90% opacity, which is why several rows still read as one equally-bright block.
    // This rational curve remains continuous but gives a clearly separated depth stack:
    // d=1 ≈ 0.66 alpha, d=2 ≈ 0.40, d=3 ≈ 0.29.
    let attenuation = 1.0 / (1.0 + 0.70 * d * d);
    let alpha = 0.18 + 0.82 * attenuation;

    let blur_progress = 1.0 - 1.0 / (1.0 + 0.55 * d * d);
    let blur_gate = 1.0 - smoothstep01((d - 2.0) / 1.8);
    let blur = 2.05 * blur_progress * blur_gate;

    (alpha.clamp(0.0, 1.0), blur.max(0.0))
}

// Keep the standalone focus profile helper for reading-mode and regression tests. It uses the same
// continuous curve as the compositor profile but without viewport-edge attenuation.
fn lyric_focus_profile(
    distance: usize,
    reading_mode: bool,
    depth_blur_active: bool,
) -> (f32, f32) {
    if reading_mode {
        return (1.0, 0.0);
    }

    let (alpha, blur) = lyric_focus_falloff(distance as f32);
    (alpha, if depth_blur_active { blur } else { 0.0 })
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

fn word_reveal_remaining(word: &StageLyricWord, position_ms: u64) -> Option<Duration> {
    let duration_ms = word.duration_ms.filter(|duration| *duration > 0)?;
    let end = word.timestamp_ms.saturating_add(duration_ms);
    (position_ms < end).then(|| Duration::from_millis(end.saturating_sub(position_ms)))
}

fn karaoke_word(
    word: &StageLyricWord,
    index: usize,
    current_word: Option<usize>,
    position_ms: u64,
    animate: bool,
    karaoke_epoch: u64,
) -> gpui::AnyElement {
    const DIM_ALPHA: f32 = 0.28;
    const DONE_ALPHA: f32 = 0.97;

    let Some(current_word) = current_word else {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(hsla(0.0, 0.0, 1.0, DIM_ALPHA))
            .child(word.text.clone())
            .into_any_element();
    };
    if index < current_word {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(hsla(0.0, 0.0, 1.0, DONE_ALPHA))
            .child(word.text.clone())
            .into_any_element();
    }
    if index > current_word {
        return div()
            .flex_none()
            .whitespace_nowrap()
            .text_color(hsla(0.0, 0.0, 1.0, DIM_ALPHA))
            .child(word.text.clone())
            .into_any_element();
    }

    let progress = word_reveal_progress(word, position_ms);
    let base = div()
        .whitespace_nowrap()
        .text_color(hsla(0.0, 0.0, 1.0, DIM_ALPHA))
        .child(word.text.clone());
    let overlay = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .h_full()
        .w_full()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(hsla(0.0, 0.0, 1.0, 1.0))
        .child(word.text.clone());

    let overlay = if animate
        && progress < 1.0
        && let Some(remaining) = word_reveal_remaining(word, position_ms)
        && !remaining.is_zero()
    {
        let key = karaoke_epoch
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(word.timestamp_ms.rotate_left(17))
            .wrapping_add(index as u64);
        overlay
            .with_animation(
                ElementId::NamedInteger(SharedString::new_static("lyric-word-sweep"), key),
                Animation::new(remaining).with_property(AnimationProperty::horizontal_reveal(
                    HorizontalRevealEdge::Left,
                    progress,
                    1.0,
                )),
                |element, _| element,
            )
            .into_any_element()
    } else if progress < 1.0 {
        overlay
            .with_sampled_animation(
                AnimationProperty::horizontal_reveal(HorizontalRevealEdge::Left, 0.0, 1.0),
                progress,
            )
            .into_any_element()
    } else {
        overlay.into_any_element()
    };

    div()
        .relative()
        .flex_none()
        .whitespace_nowrap()
        .child(base)
        .child(overlay)
        .into_any_element()
}

fn stage_primary_lyric(
    line: &StageLyricLine,
    karaoke_active: bool,
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

    // Keep exactly the same word-fragment layout whether this line is focused or not. Previously a
    // line changed from one shaped text run to many flex fragments at the active boundary, causing
    // rewrap/reflow on the same frame as the scroll hand-off.
    let mut row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_wrap()
        .items_center()
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::SEMIBOLD);
    for (index, word) in line.words.iter().enumerate() {
        row = if karaoke_active {
            row.child(karaoke_word(
                word,
                index,
                current_word,
                position_ms,
                animate,
                karaoke_epoch,
            ))
        } else {
            row.child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                    .child(word.text.clone()),
            )
        };
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


fn lyric_depth_transition(delay: Duration) -> Transition {
    Transition::new(LYRIC_DEPTH_TRANSITION_DURATION)
        .delay(delay)
        .fill_mode(FillMode::Both)
        .ease(Easing::OutCubic)
        .properties([
            TransitionProperty::Opacity,
            TransitionProperty::Blur,
        ])
}

fn lyric_opacity_transition(delay: Duration) -> Transition {
    Transition::new(LYRIC_OPACITY_TRANSITION_DURATION)
        .delay(delay)
        .fill_mode(FillMode::Both)
        .ease(Easing::OutCubic)
        .properties([TransitionProperty::Opacity])
}

fn lyric_row_motion_spec(delay: Duration) -> AnimationSpec {
    AnimationSpec::new(LYRIC_ROW_MOTION_DURATION)
        .delay(delay)
        // Delay is part of the visual choreography: lower rows must remain at their old FLIP
        // position until their own start time. Forwards does not apply the first keyframe during
        // delay and made all rows appear at the final logical position before delayed motion.
        .fill_mode(FillMode::Both)
        .ease(Easing::OutCubic)
}

#[inline]
fn lyric_row_stagger_delay(
    index: usize,
    active: usize,
    previous_active: Option<usize>,
    from_y: f32,
) -> Duration {
    let leader = previous_active.unwrap_or(active);
    let trailing_distance = if from_y >= 0.0 {
        // Normal forward playback: old active starts immediately, the promoted new active follows,
        // then every lower row starts on its own compositor timestamp.
        index.saturating_sub(leader)
    } else {
        // Reverse seek mirrors the same cascade direction.
        leader.saturating_sub(index)
    }
    .min(LYRIC_ROW_MAX_STAGGER_ROWS);

    Duration::from_millis(trailing_distance as u64 * LYRIC_ROW_STAGGER_MS)
}

fn apply_lyric_row_motion(
    row: gpui::Stateful<gpui::Div>,
    index: usize,
    active: usize,
    previous_active: Option<usize>,
    animating: bool,
    from_y: f32,
    motion_epoch: u64,
) -> gpui::AnyElement {
    if !animating || from_y.abs() <= SCROLL_SETTLE_PX {
        return row.into_any_element();
    }

    let delay = lyric_row_stagger_delay(index, active, previous_active, from_y);
    let key = motion_epoch
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(index as u64);
    row.with_animation(
        ElementId::NamedInteger(
            SharedString::new_static("stage-lyric-row-motion"),
            key,
        ),
        Animation::from_spec(lyric_row_motion_spec(delay)).with_property(
            AnimationProperty::translation(
                point(px(0.0), px(from_y)),
                point(px(0.0), px(0.0)),
            ),
        ),
        |element, _| element,
    )
    .into_any_element()
}

fn lyric_blur_transition_bound(
    index: usize,
    active: usize,
    focus_from_index: Option<usize>,
    reading_mode: bool,
) -> bool {
    if reading_mode {
        return false;
    }

    index.abs_diff(active) <= LYRIC_BLUR_TRANSITION_RADIUS
        || focus_from_index
            .is_some_and(|previous| index.abs_diff(previous) <= LYRIC_BLUR_TRANSITION_RADIUS)
}

fn lyric_opacity_transition_bound(
    index: usize,
    active: usize,
    focus_from_index: Option<usize>,
    reading_mode: bool,
) -> bool {
    if reading_mode {
        return false;
    }

    index.abs_diff(active) <= LYRIC_OPACITY_TRANSITION_RADIUS
        || focus_from_index
            .is_some_and(|previous| index.abs_diff(previous) <= LYRIC_OPACITY_TRANSITION_RADIUS)
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
    fn lyric_depth_profile_is_continuous_and_monotonic() {
        let active = lyric_focus_profile(0, false, true);
        let near = lyric_focus_profile(1, false, true);
        let middle = lyric_focus_profile(3, false, true);
        let far = lyric_focus_profile(6, false, true);

        assert_eq!(active, (1.0, 0.0));
        assert!(active.0 > near.0 && near.0 > middle.0 && middle.0 >= far.0);
        assert!(active.1 < near.1);
        assert!(near.1 <= middle.1 || middle.1 == 0.0);
        assert!(far.1 <= middle.1);
        assert_eq!(lyric_focus_profile(2, true, true), (1.0, 0.0));
        assert_eq!(lyric_focus_profile(2, false, false).1, 0.0);
    }

    #[test]
    fn focus_band_visibly_fades_each_adjacent_row() {
        let active = lyric_visual_profile(10, 10, 0.0, false, true);
        let row1 = lyric_visual_profile(11, 10, 0.0, false, true);
        let row2 = lyric_visual_profile(12, 10, 0.0, false, true);
        let row3 = lyric_visual_profile(13, 10, 0.0, false, true);
        let row4 = lyric_visual_profile(14, 10, 0.0, false, true);

        assert!(active.0 > 0.99);
        assert!(row1.0 < 0.70);
        assert!(row2.0 < 0.45);
        assert!(row3.0 < 0.33);
        assert!(row4.0 < 0.28);
        assert!(active.0 > row1.0);
        assert!(row1.0 > row2.0);
        assert!(row2.0 > row3.0);
        assert!(row3.0 > row4.0);
        assert!(active.1 < row1.1);
        assert!(row1.1 < row2.1);
        assert!(row2.1 < row3.1);
    }

    #[test]
    fn viewport_edge_progress_tracks_actual_top_and_bottom_distance() {
        let viewport = gpui::Bounds::new(
            gpui::point(px(0.0), px(100.0)),
            gpui::size(px(500.0), px(600.0)),
        );

        let top_edge = lyric_viewport_edge_progress(100.0, viewport);
        let top_inside = lyric_viewport_edge_progress(230.0, viewport);
        let center = lyric_viewport_edge_progress(400.0, viewport);
        let bottom_inside = lyric_viewport_edge_progress(545.0, viewport);
        let bottom_edge = lyric_viewport_edge_progress(700.0, viewport);

        assert!(top_edge > top_inside);
        assert!(top_inside > center);
        assert!(bottom_edge > bottom_inside);
        assert!(bottom_inside > center);
        assert_eq!(center, 0.0);
    }

    #[test]
    fn visual_profile_becomes_transparent_and_blurred_toward_edges() {
        let active = lyric_visual_profile(10, 10, 0.0, false, true);
        let top_mid = lyric_visual_profile(6, 10, 0.45, false, true);
        let top_edge = lyric_visual_profile(2, 10, 0.95, false, true);
        let bottom_mid = lyric_visual_profile(15, 10, 0.45, false, true);
        let bottom_edge = lyric_visual_profile(19, 10, 0.95, false, true);

        assert_eq!(active, (1.0, 0.0));
        assert!(active.0 > top_mid.0 && top_mid.0 > top_edge.0);
        assert!(active.0 > bottom_mid.0 && bottom_mid.0 > bottom_edge.0);
        assert!(top_mid.1 >= 0.0 && top_edge.1 >= 0.0);
        assert!(bottom_mid.1 >= 0.0 && bottom_edge.1 >= 0.0);
    }

    #[test]
    fn delayed_row_motion_holds_the_old_position_before_start() {
        let spec = lyric_row_motion_spec(Duration::from_millis(LYRIC_ROW_STAGGER_MS * 2));
        let before = spec.sample_elapsed(Duration::from_millis(LYRIC_ROW_STAGGER_MS));
        assert!(before.applies);
        assert_eq!(before.eased_progress, 0.0);

        let after = spec.sample_elapsed(Duration::from_millis(
            LYRIC_ROW_STAGGER_MS * 2 + LYRIC_ROW_MOTION_DURATION.as_millis() as u64,
        ));
        assert!(after.applies);
        assert_eq!(after.eased_progress, 1.0);
    }

    #[test]
    fn row_scroll_stagger_promotes_from_previous_active() {
        let previous = Some(9);
        assert_eq!(
            lyric_row_stagger_delay(9, 10, previous, 80.0),
            Duration::ZERO
        );
        assert_eq!(
            lyric_row_stagger_delay(10, 10, previous, 80.0),
            Duration::from_millis(LYRIC_ROW_STAGGER_MS)
        );
        assert_eq!(
            lyric_row_stagger_delay(12, 10, previous, 80.0),
            Duration::from_millis(LYRIC_ROW_STAGGER_MS * 3)
        );
        assert_eq!(
            lyric_row_stagger_delay(20, 10, previous, 80.0),
            Duration::from_millis(
                LYRIC_ROW_STAGGER_MS * LYRIC_ROW_MAX_STAGGER_ROWS as u64
            )
        );
    }

    #[test]
    fn lyric_transition_cost_is_limited_to_nearby_blur_rows() {
        assert!(lyric_blur_transition_bound(12, 12, Some(11), false));
        assert!(lyric_blur_transition_bound(10, 12, Some(11), false));
        assert!(!lyric_blur_transition_bound(8, 12, Some(11), false));

        // Opacity is compositor-cheap and may cover the full visible focus neighborhood.
        assert!(lyric_opacity_transition_bound(8, 12, Some(11), false));
        assert!(lyric_opacity_transition_bound(4, 12, Some(11), false));
        assert!(!lyric_opacity_transition_bound(0, 12, Some(11), false));

        assert!(!lyric_blur_transition_bound(12, 12, Some(11), true));
        assert!(!lyric_opacity_transition_bound(12, 12, Some(11), true));
    }

    #[test]
    fn scroll_animation_leading_row_matches_renderer_timing() {
        let start = Instant::now();
        let animation = LyricScrollAnimation {
            from_y: 120.0,
            started_at: start,
        };
        assert!((animation.offset_at(start) - 120.0).abs() < 0.001);
        let halfway = animation.offset_at(start + LYRIC_ROW_MOTION_DURATION / 2);
        assert!(halfway > 0.0 && halfway < 120.0);
        assert!(animation
            .offset_at(start + LYRIC_ROW_MOTION_DURATION)
            .abs()
            < 0.001);
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
        assert_eq!(
            word_reveal_remaining(word, 1_000),
            Some(Duration::from_millis(300))
        );
        assert_eq!(
            word_reveal_remaining(word, 1_250),
            Some(Duration::from_millis(50))
        );
        assert_eq!(word_reveal_remaining(word, 1_300), None);
    }
}
