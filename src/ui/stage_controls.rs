use std::{sync::Arc, time::Duration};

use anyhow::Result;
use gpui::{
    BorrowAppContext as _, Context, Entity, Global, IntoElement, Render, Timer, WeakEntity, Window,
    div, hsla, prelude::*, px,
};
use lucide_gpui::icon;

use crate::{
    audio::{AudioEngine, PlayerCommand},
    model::PlaybackState,
};

use super::{
    components::{SliderStyle, interactive_slider},
    shell::{DragTarget, MusicApp},
    theme::{ACCENT_RED, format_remaining_time, format_time, themed_icon},
};

const STAGE_TRANSPORT_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct StageControlsViewCache {
    view: Option<Entity<StageControlsView>>,
}

impl Global for StageControlsViewCache {}

pub(super) fn view(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> Entity<StageControlsView> {
    let parent = cx.entity().downgrade();
    let engine = app.engine.clone();
    let view = cx.update_default_global(|cache: &mut StageControlsViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| StageControlsView::new(parent, engine));
        cache.view = Some(view.clone());
        view
    });

    let stage_active = app.stage_open || app.stage_animating;
    view.update(cx, |view, cx| view.sync_from_app(app, stage_active, cx));
    view
}

pub(super) struct StageControlsView {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    stage_active: bool,
    controls_visible: bool,
    timer_started: bool,
}

impl StageControlsView {
    fn new(parent: WeakEntity<MusicApp>, engine: Option<Arc<AudioEngine>>) -> Self {
        Self {
            parent,
            engine,
            stage_active: false,
            controls_visible: true,
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
        let controls_visible = app.stage_controls_visibility > 0.005
            || matches!(app.drag_target, Some(DragTarget::Progress | DragTarget::Volume));
        let changed = engine_changed
            || self.stage_active != stage_active
            || self.controls_visible != controls_visible;
        if engine_changed {
            self.engine = app.engine.clone();
        }
        self.stage_active = stage_active;
        self.controls_visible = controls_visible;
        if changed {
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
                Timer::after(STAGE_TRANSPORT_REFRESH_INTERVAL).await;
                if this
                    .update(cx, |this, cx| {
                        if this.stage_active
                            && this.controls_visible
                            && this.engine.as_ref().is_some_and(|engine| {
                                engine.progress().0 == PlaybackState::Playing
                            })
                        {
                            // Only this transport entity is invalidated. MusicApp, artwork, lyrics
                            // and the fluid background stay retained while the clock advances.
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
            Ok(())
        })
        .detach();
    }
}

impl Render for StageControlsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_transport_timer(cx);

        let Some(parent_entity) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let (transport_state, drag_progress_ratio, volume, visibility) = {
            let app = parent_entity.read(cx);
            (
                app.snapshot.state,
                app.drag_progress_ratio,
                app.displayed_volume_ratio(),
                app.stage_controls_visibility,
            )
        };
        let (_, live_position_ms, duration_ms) = self.engine.as_ref().map_or(
            (PlaybackState::Stopped, 0, 0),
            |engine| engine.progress(),
        );
        let position = drag_progress_ratio.map_or(live_position_ms, |ratio| {
            (duration_ms as f32 * ratio.clamp(0.0, 1.0)).round() as u64
        });
        let progress_ratio = drag_progress_ratio.unwrap_or_else(|| {
            if duration_ms == 0 {
                0.0
            } else {
                (live_position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
            }
        });
        let playing = transport_state == PlaybackState::Playing;
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
                    let _ = parent.update(cx, |app, _cx| {
                        app.stage_controls_hovered = *hovered;
                        if *hovered
                            && app.stage_suppress_wake_until.is_none()
                            && app.stage_controls_visibility >= 0.995
                        {
                            app.stage_last_user_activity = std::time::Instant::now();
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
            .child(
                div()
                    .text_xs()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                    .child(format_time(position)),
            )
            .child(
                interactive_slider(
                    "stage-progress-track",
                    progress_ratio,
                    SliderStyle::stage_progress(),
                    {
                        let parent = parent.clone();
                        move |ratio, cx| {
                            let _ = parent.update(cx, |app, app_cx| {
                                app.wake_stage_controls_immediately(app_cx);
                                app.seek_to_ratio(ratio, app_cx);
                            });
                        }
                    },
                    {
                        let parent = parent.clone();
                        move |ratio, cx| {
                            let _ = parent.update(cx, |app, app_cx| {
                                app.wake_stage_controls_immediately(app_cx);
                                if app.drag_target == Some(DragTarget::Progress) {
                                    app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                                } else {
                                    app.begin_drag(DragTarget::Progress, ratio, app_cx);
                                }
                            });
                        }
                    },
                    {
                        let parent = parent.clone();
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
                        }
                    },
                )
                .flex_1(),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(hsla(0.0, 0.0, 1.0, 0.68))
                    .child(format_remaining_time(position, duration_ms)),
            )
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
