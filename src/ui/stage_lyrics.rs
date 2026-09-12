use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, BorrowAppContext as _,
    CompositeLayerExt as _, Context, Easing, ElementId, Entity, Global, IntoElement, ListAlignment,
    ListOffset, ListState, Render, SharedString, Timer, Transition, TransitionProperty, WeakEntity,
    Window, div, hsla, list, point, prelude::*, px,
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
const SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(320);
const SCROLL_SETTLE_PX: f32 = 0.30;
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
    byte_start: usize,
    byte_end: usize,
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
            .map(|word| {
                let byte_start = byte_offset;
                byte_offset += word.text.len();
                StageLyricWord {
                    timestamp_ms: word.timestamp_ms,
                    byte_start,
                    byte_end: byte_offset,
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
    epoch: u64,
    from_y: f32,
    started_at: Instant,
}

impl LyricScrollAnimation {
    fn offset_at(self, now: Instant) -> f32 {
        let duration = SCROLL_ANIMATION_DURATION.as_secs_f32();
        if duration <= f32::EPSILON {
            return 0.0;
        }
        let progress = (now
            .saturating_duration_since(self.started_at)
            .as_secs_f32()
            / duration)
            .clamp(0.0, 1.0);
        let remaining = 1.0 - progress;
        self.from_y * remaining * remaining * remaining
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
    active_word_index: Option<usize>,
    hovered_index: Option<usize>,
    motion_epoch: u64,
    reading_until: Option<Instant>,
    reading_epoch: u64,
    scroll_target: Option<usize>,
    scroll_animation: Option<LyricScrollAnimation>,
    scroll_epoch: u64,
    stage_active: bool,
    timer_started: bool,
    timer_epoch: u64,
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
            active_word_index: None,
            hovered_index: None,
            motion_epoch: 0,
            reading_until: None,
            reading_epoch: 0,
            scroll_target: None,
            scroll_animation: None,
            scroll_epoch: 0,
            stage_active: false,
            timer_started: false,
            timer_epoch: 0,
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
        let playback_state_changed = self.playback_state != app.snapshot.state;
        let stage_active_changed = self.stage_active != stage_active;
        let timer_policy_changed =
            engine_changed || source_changed || playback_state_changed || stage_active_changed;

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
            self.active_word_index = None;
            self.hovered_index = None;
            self.motion_epoch = self.motion_epoch.wrapping_add(1);
            self.reading_until = None;
            self.scroll_target = None;
            self.cancel_scroll_animation();
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
            // Keep the hot transport sample locally, but do not invalidate the lyric view merely
            // because another 100 ms of audio elapsed. Rendering changes only at line/word edges.
            self.position_ms = position_ms;
        }
        if playback_state_changed {
            self.playback_state = app.snapshot.state;
            changed = true;
        }
        if stage_active_changed {
            self.stage_active = stage_active;
            if stage_active {
                if !self.is_reading() {
                    self.scroll_target = self.active_index;
                }
            } else {
                self.cancel_scroll_animation();
            }
            changed = true;
        }
        if timer_policy_changed {
            self.timer_epoch = self.timer_epoch.wrapping_add(1);
            self.timer_started = false;
        }

        let active_changed = self.update_active_index();
        let next_word = self.compute_active_word_index();
        let word_changed = position_changed
            && !source_changed
            && !active_changed
            && previous_word != next_word;
        self.active_word_index = next_word;
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

    fn next_transport_delay(&self) -> Duration {
        let (_, position_ms, _) = self
            .engine
            .as_ref()
            .expect("stage lyric transport requires an audio engine")
            .progress();
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
        if !self.transport_should_run() {
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
        let next_word = self.compute_active_word_index();
        let word_changed = !active_changed && previous_word != next_word;
        self.active_word_index = next_word;
        if active_changed || word_changed {
            // Text shaping happens only at semantic lyric boundaries. Vertical movement is handed
            // to one retained compositor translation instead of rerunning List layout per frame.
            cx.notify();
        }
    }

    fn ensure_transport_timer(&mut self, cx: &mut Context<Self>) {
        if self.timer_started || !self.transport_should_run() {
            return;
        }
        self.timer_started = true;
        let epoch = self.timer_epoch;
        cx.spawn(async move |this, cx| -> Result<()> {
            loop {
                let delay = match this.update(cx, |this, _cx| {
                    (this.timer_epoch == epoch && this.transport_should_run())
                        .then(|| this.next_transport_delay())
                }) {
                    Ok(Some(delay)) => delay,
                    _ => break,
                };
                Timer::after(delay).await;
                let keep_running = match this.update(cx, |this, cx| {
                    if this.timer_epoch != epoch {
                        return false;
                    }
                    if !this.transport_should_run() {
                        this.timer_started = false;
                        return false;
                    }
                    this.refresh_transport(cx);
                    true
                }) {
                    Ok(keep_running) => keep_running,
                    Err(_) => break,
                };
                if !keep_running {
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

    fn cancel_scroll_animation(&mut self) {
        self.scroll_epoch = self.scroll_epoch.wrapping_add(1);
        self.scroll_animation = None;
    }

    fn current_scroll_animation_offset(&self, now: Instant) -> f32 {
        self.scroll_animation
            .map_or(0.0, |animation| animation.offset_at(now))
    }

    fn start_scroll_animation(
        &mut self,
        from_y: f32,
        started_at: Instant,
        cx: &mut Context<Self>,
    ) {
        if !from_y.is_finite() || from_y.abs() <= SCROLL_SETTLE_PX {
            self.cancel_scroll_animation();
            return;
        }

        self.scroll_epoch = self.scroll_epoch.wrapping_add(1);
        let epoch = self.scroll_epoch;
        self.scroll_animation = Some(LyricScrollAnimation {
            epoch,
            from_y,
            started_at,
        });

        cx.spawn(async move |this, cx| -> Result<()> {
            Timer::after(SCROLL_ANIMATION_DURATION).await;
            this.update(cx, |this, cx| {
                if this
                    .scroll_animation
                    .is_some_and(|animation| animation.epoch == epoch)
                {
                    this.scroll_animation = None;
                    cx.notify();
                }
            })?;
            Ok(())
        })
        .detach();
    }

    fn begin_reading_mode(&mut self, cx: &mut Context<Self>) {
        self.reading_epoch = self.reading_epoch.wrapping_add(1);
        let epoch = self.reading_epoch;
        self.reading_until = Some(Instant::now() + READING_MODE_DURATION);
        self.active_word_index = None;
        self.scroll_target = None;
        self.hovered_index = None;
        self.cancel_scroll_animation();
        cx.notify();

        cx.spawn(async move |this, cx| -> Result<()> {
            Timer::after(READING_MODE_DURATION).await;
            this.update(cx, |this, cx| {
                if this.reading_epoch != epoch {
                    return;
                }
                this.reading_until = None;
                this.active_word_index = this.compute_active_word_index();
                this.scroll_target = this.active_index;
                cx.notify();
            })?;
            Ok(())
        })
        .detach();
    }

    fn prepare_scroll_animation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.stage_active || self.is_reading() {
            return;
        }
        let Some(target) = self.scroll_target else {
            return;
        };

        let viewport = self.list_state.viewport_bounds();
        if f32::from(viewport.size.height) <= 0.5 {
            // The first active render may precede List's initial prepaint. One local wake is enough
            // to obtain variable-height measurements; the actual transition is compositor-driven.
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
            // Measurement recovery can require one more layout. Do not keep a stale compositor
            // translation while the logical list is being repositioned to discover the row.
            self.cancel_scroll_animation();
            window.request_animation_frame();
            return;
        };

        let anchor_y = f32::from(viewport.origin.y)
            + f32::from(viewport.size.height) * LYRIC_ANCHOR_RATIO;
        let diff = f32::from(line_bounds.center().y) - anchor_y;
        if diff.abs() <= SCROLL_SETTLE_PX {
            self.scroll_target = None;
            return;
        }

        let now = window.animation_time();
        let carry = self.current_scroll_animation_offset(now);
        let before = f32::from(self.list_state.scroll_px_offset_for_scrollbar().y);
        self.list_state.scroll_by(px(diff));
        let after = f32::from(self.list_state.scroll_px_offset_for_scrollbar().y);
        let applied = before - after;
        self.scroll_target = None;
        self.hovered_index = None;

        if applied.abs() <= SCROLL_SETTLE_PX {
            self.cancel_scroll_animation();
            return;
        }

        // scroll_by() jumps layout to its final location. The inverse visual offset keeps the first
        // compositor frame exactly where the previous frame was. If a new lyric arrives while the
        // previous transition is still running, carry its current residual transform into the new
        // start value so retargeting remains continuous.
        self.start_scroll_animation(carry + applied, now, cx);
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
            self.active_word_index = self.compute_active_word_index();
            self.scroll_target = self.active_index;
        }
        self.prepare_scroll_animation(window, cx);

        let active = self.active_index.unwrap_or(0);
        let active_word_index = self.active_word_index;
        let reading_mode = self.is_reading();
        let scroll_animation = self.scroll_animation;
        let scroll_animating = scroll_animation.is_some();
        // Element blur captures an offscreen Scene per blurred row. Keep it out of the one capture
        // used for vertical movement, then restore the depth cue after the compositor settles.
        let depth_blur_active = self.playback_state == PlaybackState::Playing
            && !reading_mode
            && !scroll_animating;
        let text_id = if depth_blur_active {
            "lyric-text-blur"
        } else {
            "lyric-text-direct"
        };
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
                active_word_index,
                reading_mode,
                depth_blur_active,
                text_id,
                hovered_index == Some(index),
                !scroll_animating,
                motion_epoch,
                view.clone(),
                parent.clone(),
            )
        })
        .size_full()
        .pt(px(96.0))
        .pb(px(112.0))
        .pr(px(8.0));

        let lyrics = if let Some(scroll) = scroll_animation {
            let animation = Animation::from_spec(
                AnimationSpec::new(SCROLL_ANIMATION_DURATION).ease(Easing::OutCubic),
            )
            .with_property(AnimationProperty::translation(
                point(px(0.0), px(scroll.from_y)),
                point(px(0.0), px(0.0)),
            ));
            lyrics
                .composite_layer()
                .with_animation(
                    ElementId::NamedInteger(
                        SharedString::new_static("stage-lyrics-scroll"),
                        scroll.epoch,
                    ),
                    animation,
                    |element, _| element,
                )
                .into_any_element()
        } else {
            lyrics.into_any_element()
        };

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
    active_word_index: Option<usize>,
    reading_mode: bool,
    depth_blur_active: bool,
    text_id: &'static str,
    hovered: bool,
    interactive: bool,
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
        .child(stage_primary_lyric(
            line,
            karaoke_active,
            active_word_index,
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
    row.on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
        cx.stop_propagation();
        let _ = local.update(cx, |this, cx| {
            this.reading_epoch = this.reading_epoch.wrapping_add(1);
            this.reading_until = None;
            this.hovered_index = None;
            this.position_ms = timestamp;
            this.active_index = Some(index);
            this.active_word_index = this.compute_active_word_index();
            this.motion_epoch = this.motion_epoch.wrapping_add(1);
            this.scroll_target = Some(index);
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

fn active_enhanced_word_index(line: &StageLyricLine, position_ms: u64) -> Option<usize> {
    if !line.enhanced_complete {
        return None;
    }
    line.words
        .partition_point(|word| word.timestamp_ms <= position_ms)
        .checked_sub(1)
}

fn lyric_word_highlight(fade_out: Option<f32>) -> gpui::HighlightStyle {
    gpui::HighlightStyle {
        fade_out,
        ..gpui::HighlightStyle::default()
    }
}

fn stage_primary_lyric(
    line: &StageLyricLine,
    karaoke_active: bool,
    current_word: Option<usize>,
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

    // Keep one fixed run per source word. Only alpha changes as playback advances, so GPUI's
    // TextLayout geometry key remains stable and the word transition is handled as a paint-only
    // decoration refresh instead of reshaping/re-wrapping the entire active line.
    let mut highlights = Vec::with_capacity(line.words.len());
    for (index, word) in line.words.iter().enumerate() {
        let fade_out = match current_word {
            Some(current) if index < current => Some(0.12),
            Some(current) if index == current => None,
            Some(_) => Some(0.58),
            None if index == 0 => Some(0.10),
            None => Some(0.58),
        };
        highlights.push((
            word.byte_start..word.byte_end,
            lyric_word_highlight(fade_out),
        ));
    }

    div()
        .w_full()
        .min_w(px(0.0))
        .text_size(px(28.0))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(hsla(0.0, 0.0, 1.0, 1.0))
        .child(gpui::StyledText::new(line.text.clone()).with_highlights(highlights))
        .into_any_element()
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
    fn scroll_animation_keeps_continuity_and_settles() {
        let start = Instant::now();
        let animation = LyricScrollAnimation {
            epoch: 1,
            from_y: 120.0,
            started_at: start,
        };
        assert!((animation.offset_at(start) - 120.0).abs() < 0.001);
        let halfway = animation.offset_at(start + SCROLL_ANIMATION_DURATION / 2);
        assert!(halfway > 0.0 && halfway < 120.0);
        assert!(animation.offset_at(start + SCROLL_ANIMATION_DURATION).abs() < 0.001);
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

    #[test]
    fn active_enhanced_word_uses_cached_semantic_boundary() {
        let source = LyricLine {
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
        let line = StageLyricLine::from_source(&source);
        assert_eq!(active_enhanced_word_index(&line, 999), None);
        assert_eq!(active_enhanced_word_index(&line, 1_000), Some(0));
        assert_eq!(active_enhanced_word_index(&line, 1_499), Some(0));
        assert_eq!(active_enhanced_word_index(&line, 1_500), Some(1));
        assert_eq!(line.words[0].byte_start, 0);
        assert_eq!(line.words[0].byte_end, "你好 ".len());
        assert_eq!(line.words[1].byte_start, "你好 ".len());
        assert_eq!(line.words[1].byte_end, line.text.len());
    }
}
