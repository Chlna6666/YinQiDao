use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    BorrowAppContext as _, Context, Entity, Global, IntoElement, Render, WeakEntity, Window, div,
    hsla, prelude::*, px,
};
use lucide_gpui::icon;

use crate::{
    audio::{AudioEngine, PlayerCommand},
    model::PlaybackState,
};

use super::{
    components::{SliderStyle, interactive_slider, slider::InteractiveSliderState},
    shell::{DragTarget, MusicApp},
    theme::{ACCENT_RED, format_remaining_time, format_time, themed_icon},
};

const TRANSPORT_MIN_SLEEP_MS: u64 = 8;
const TRANSPORT_MAX_SLEEP_MS: u64 = 1_000;

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
    let visibility = app.stage_controls_visibility.clamp(0.0, 1.0);
    let controls_visible = visibility > 0.005 || app.drag_target.is_some();
    view.update(cx, |view, cx| {
        view.sync_from_app(app, stage_active, controls_visible, cx)
    });
    view
}

pub(super) struct StageControlsView {
    parent: WeakEntity<MusicApp>,
    transport: Entity<StageTransportView>,
    stage_active: bool,
    visibility: f32,
    playback_state: PlaybackState,
    volume: f32,
}

impl StageControlsView {
    fn new(parent: WeakEntity<MusicApp>, transport: Entity<StageTransportView>) -> Self {
        Self {
            parent,
            transport,
            stage_active: false,
            visibility: 1.0,
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
        let visibility = app.stage_controls_visibility.clamp(0.0, 1.0);
        let playback_state = app.snapshot.state;
        let volume = app.displayed_volume_ratio();
        let changed = self.stage_active != stage_active
            || (self.visibility - visibility).abs() > 0.0005
            || self.playback_state != playback_state
            || (self.volume - volume).abs() > 0.0005;

        self.stage_active = stage_active;
        self.visibility = visibility;
        self.playback_state = playback_state;
        self.volume = volume;

        if changed {
            cx.notify();
        }
    }
}

impl Render for StageControlsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let visibility = self.visibility;
        let playing = self.playback_state == PlaybackState::Playing;
        let volume = self.volume;
        let parent = self.parent.clone();

        div()
            .id("stage-bottom-dock")
            .top(px((1.0 - visibility) * 56.0))
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
                        interactive_slider(
                            "stage-volume-track",
                            volume,
                            SliderStyle::stage_volume(),
                            {
                                let parent = parent.clone();
                                move |ratio, cx| {
                                    let _ = parent.update(cx, |app, app_cx| {
                                        app.wake_stage_controls_immediately(app_cx);
                                        app.pending_volume_ratio = None;
                                        app.set_app_volume(ratio, app_cx);
                                    });
                                }
                            },
                            {
                                let parent = parent.clone();
                                move |ratio, cx| {
                                    let _ = parent.update(cx, |app, app_cx| {
                                        app.wake_stage_controls_immediately(app_cx);
                                        if app.drag_target == Some(DragTarget::Volume) {
                                            app.update_drag_ratio(DragTarget::Volume, ratio, app_cx);
                                        } else {
                                            app.begin_drag(DragTarget::Volume, ratio, app_cx);
                                        }
                                        app.send(PlayerCommand::SetVolume(ratio));
                                    });
                                }
                            },
                            {
                                let parent = parent.clone();
                                move |ratio, cx| {
                                    let _ = parent.update(cx, |app, app_cx| {
                                        app.wake_stage_controls_immediately(app_cx);
                                        if app.drag_target == Some(DragTarget::Volume) {
                                            app.update_drag_ratio(DragTarget::Volume, ratio, app_cx);
                                        } else {
                                            app.begin_drag(DragTarget::Volume, ratio, app_cx);
                                        }
                                        app.commit_drag(app_cx);
                                        app.pending_volume_ratio = None;
                                    });
                                }
                            },
                        )
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
}
