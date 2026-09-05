use std::{sync::Arc, time::Duration};

use anyhow::Result;
use gpui::{
    Context, EncodedImageBytes, Entity, ImageFormat, IntoElement, ObjectFit, Render,
    StatefulInteractiveElement as _, Timer, WeakEntity, Window, div, hsla, img, linear_color_stop,
    linear_gradient, prelude::*, px, rgb,
};
use lucide_gpui::icons as lucide_icons;

use crate::{
    audio::{AudioEngine, PlayerCommand},
    model::{AppPage, LibraryTab, PlaybackState, RepeatMode},
};

use super::{
    components::{SliderStyle, interactive_slider},
    shell::{DragTarget, MusicApp},
    theme::{
        self, ACCENT_RED, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, TEXT_WHITE,
        elegant_gradient_for, press_transition, themed_icon,
    },
};

const MINI_PROGRESS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const MINI_TIME_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const NOW_PLAYING_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

fn mini_clock_visible(parent: &WeakEntity<MusicApp>, cx: &gpui::App) -> bool {
    parent
        .read_with(cx, |app, _| !app.stage_open)
        .unwrap_or(false)
}

// ============================================================================
// 底部常驻播放器
//
// 播放进度与时间是独立 retained 子视图。它们不能继续使用两个 10Hz notify 循环：
// 在旧版 GPUI targeted repaint 下，这会让静态页面不断进入 retained replay，播放时显著
// 增加输入分发与 frame 提交竞争。进度限制为 4Hz，时间限制为 1Hz；沉浸舞台覆盖 dock 时
// 两个子视图完全停止 notify，只保留轻量 timer wakeup。
// ============================================================================

pub(super) struct PlaybackProgress {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    timer_started: bool,
}

impl PlaybackProgress {
    pub(super) fn new(parent: WeakEntity<MusicApp>, engine: Option<Arc<AudioEngine>>) -> Self {
        Self {
            parent,
            engine,
            timer_started: false,
        }
    }
}

impl Render for PlaybackProgress {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.timer_started {
            self.timer_started = true;
            cx.spawn(async move |this, cx| -> Result<()> {
                loop {
                    Timer::after(MINI_PROGRESS_REFRESH_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            if mini_clock_visible(&this.parent, cx)
                                && this.engine.as_ref().is_some_and(|engine| {
                                    engine.progress().0 == PlaybackState::Playing
                                })
                            {
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

        let (_, position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((PlaybackState::Stopped, 0, 0), |engine| engine.progress());
        let overrides = self
            .parent
            .read_with(cx, |app, _| {
                (
                    app.drag_progress_ratio,
                    app.pending_progress_ratio.map(|(_, ratio)| ratio),
                )
            })
            .unwrap_or((None, None));
        let ratio = overrides.0.or(overrides.1).unwrap_or_else(|| {
            if duration_ms == 0 {
                0.0
            } else {
                (position_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
            }
        });
        let parent = self.parent.clone();
        let this = cx.weak_entity();

        interactive_slider(
            "mini-progress-track",
            ratio,
            SliderStyle::mini_progress(),
            {
                let parent = parent.clone();
                let this = this.clone();
                move |ratio, cx| {
                    let _ = parent.update(cx, |app, app_cx| {
                        if app.drag_target == Some(DragTarget::Progress) {
                            app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                        } else {
                            app.begin_drag(DragTarget::Progress, ratio, app_cx);
                        }
                    });
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            },
            {
                let parent = parent.clone();
                let this = this.clone();
                move |ratio, cx| {
                    let _ = parent.update(cx, |app, app_cx| {
                        if app.drag_target == Some(DragTarget::Progress) {
                            app.update_drag_ratio(DragTarget::Progress, ratio, app_cx);
                        } else {
                            app.begin_drag(DragTarget::Progress, ratio, app_cx);
                        }
                        app.commit_drag(app_cx);
                    });
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            },
        )
        .w_full()
        .into_any_element()
    }
}

pub(super) struct PlaybackTime {
    parent: WeakEntity<MusicApp>,
    engine: Option<Arc<AudioEngine>>,
    timer_started: bool,
}

impl PlaybackTime {
    pub(super) fn new(parent: WeakEntity<MusicApp>, engine: Option<Arc<AudioEngine>>) -> Self {
        Self {
            parent,
            engine,
            timer_started: false,
        }
    }
}

impl Render for PlaybackTime {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.timer_started {
            self.timer_started = true;
            cx.spawn(async move |this, cx| -> Result<()> {
                loop {
                    Timer::after(MINI_TIME_REFRESH_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            if mini_clock_visible(&this.parent, cx)
                                && this.engine.as_ref().is_some_and(|engine| {
                                    engine.progress().0 == PlaybackState::Playing
                                })
                            {
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

        let (_, position_ms, duration_ms) = self
            .engine
            .as_ref()
            .map_or((PlaybackState::Stopped, 0, 0), |engine| engine.progress());
        let (drag_ratio, pending_ratio) = self
            .parent
            .read_with(cx, |app, _| {
                (
                    app.drag_progress_ratio,
                    app.pending_progress_ratio.map(|(_, ratio)| ratio),
                )
            })
            .unwrap_or((None, None));
        let display_position = drag_ratio.or(pending_ratio).map_or(position_ms, |ratio| {
            (duration_ms as f32 * ratio).round() as u64
        });

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

pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> impl IntoElement {
    let snapshot = &app.snapshot;
    let is_playing = snapshot.state == PlaybackState::Playing;
    let track = snapshot.current_track.as_ref();
    let track_id = track.map(|track| track.id);
    let title = track.map_or("等待播放", |track| track.title.as_str());
    let artist = track.map_or("点击曲库开启音乐旅程", |track| {
        track.artist.as_str()
    });
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());
    let app_entity = cx.entity().downgrade();

    div()
        .id("mini-player-container")
        .w_full()
        .bg(rgb(0xff_ff_ff))
        .border_t_1()
        .border_color(BORDER_HAIRLINE)
        .flex()
        .flex_col()
        .child(playback_progress)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_6()
                .py_2()
                .h(px(72.0))
                .child(
                    div()
                        .w(px(280.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .id("mini-player-cover-title-trigger")
                                .flex()
                                .items_center()
                                .gap_3()
                                .flex_1()
                                .min_w(px(0.0))
                                .p_1()
                                .rounded_xl()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|style| style.scale(0.98))
                                .child(mini_cover_element(track_id, artwork))
                                .child(
                                    div()
                                        .flex()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .flex_col()
                                        .gap(px(1.0))
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(TEXT_PRIMARY)
                                                .truncate()
                                                .child(title.to_owned()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(TEXT_SECONDARY)
                                                .truncate()
                                                .child(artist.to_owned()),
                                        ),
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.open_stage(cx))),
                        )
                        .child(
                            div()
                                .id("mini-heart-btn")
                                .size(px(28.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .hover(|style| style.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|style| style.scale(0.92))
                                .child(themed_icon(
                                    lucide_icons::icon_heart(),
                                    15.0,
                                    hsla(220.0, 0.08, 0.60, 1.0),
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_4()
                                .child(
                                    div()
                                        .id("mini-shuffle")
                                        .size(px(28.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .bg(if snapshot.shuffle {
                                            theme::accent_red_muted()
                                        } else {
                                            hsla(0.0, 0.0, 0.0, 0.0)
                                        })
                                        .hover(|style| style.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|style| style.scale(0.92))
                                        .child(themed_icon(
                                            lucide_icons::icon_shuffle(),
                                            15.0,
                                            if snapshot.shuffle {
                                                ACCENT_RED.into()
                                            } else {
                                                hsla(220.0, 0.08, 0.50, 1.0)
                                            },
                                        ))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_shuffle(cx);
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("mini-previous")
                                        .size(px(32.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|style| style.scale(0.92))
                                        .child(themed_icon(
                                            lucide_icons::icon_skip_back(),
                                            18.0,
                                            hsla(220.0, 0.10, 0.35, 1.0),
                                        ))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.previous(cx);
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("mini-play-pause")
                                        .size(px(36.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .bg(if is_playing {
                                            ACCENT_RED
                                        } else {
                                            rgb(0x1d_1d_1f)
                                        })
                                        .hover(|style| style.opacity(0.90))
                                        .transition(press_transition())
                                        .active(|style| style.scale(0.94))
                                        .child(themed_icon(
                                            if is_playing {
                                                lucide_icons::icon_pause()
                                            } else {
                                                lucide_icons::icon_play()
                                            },
                                            18.0,
                                            hsla(0.0, 0.0, 1.0, 1.0),
                                        ))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.toggle_play(cx);
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("mini-next")
                                        .size(px(32.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .hover(|style| style.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|style| style.scale(0.92))
                                        .child(themed_icon(
                                            lucide_icons::icon_skip_forward(),
                                            18.0,
                                            hsla(220.0, 0.10, 0.35, 1.0),
                                        ))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.next(cx);
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("mini-repeat")
                                        .size(px(28.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .bg(if snapshot.repeat != RepeatMode::Off {
                                            theme::accent_red_muted()
                                        } else {
                                            hsla(0.0, 0.0, 0.0, 0.0)
                                        })
                                        .hover(|style| style.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|style| style.scale(0.92))
                                        .child(themed_icon(
                                            match snapshot.repeat {
                                                RepeatMode::Off | RepeatMode::All => {
                                                    lucide_icons::icon_repeat()
                                                }
                                                RepeatMode::One => lucide_icons::icon_repeat_1(),
                                            },
                                            15.0,
                                            if snapshot.repeat != RepeatMode::Off {
                                                ACCENT_RED.into()
                                            } else {
                                                hsla(220.0, 0.08, 0.50, 1.0)
                                            },
                                        ))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.cycle_repeat(cx);
                                            }),
                                        ),
                                ),
                        )
                        .child(playback_time),
                )
                .child(
                    div()
                        .w(px(280.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_3()
                        .child(
                            div()
                                .id("mini-lyrics-btn")
                                .size(px(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .bg(if app.page == AppPage::Player || app.stage_open {
                                    theme::accent_red_muted()
                                } else {
                                    hsla(0.0, 0.0, 0.0, 0.0)
                                })
                                .hover(|style| style.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|style| style.scale(0.92))
                                .child(themed_icon(
                                    lucide_icons::icon_captions(),
                                    16.0,
                                    if app.page == AppPage::Player || app.stage_open {
                                        ACCENT_RED.into()
                                    } else {
                                        hsla(220.0, 0.08, 0.50, 1.0)
                                    },
                                ))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_stage(cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .id("mini-queue-btn")
                                .size(px(30.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .bg(
                                    if app.page == AppPage::Library
                                        && app.library_tab == LibraryTab::Playlists
                                    {
                                        theme::accent_red_muted()
                                    } else {
                                        hsla(0.0, 0.0, 0.0, 0.0)
                                    },
                                )
                                .hover(|style| style.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|style| style.scale(0.92))
                                .child(themed_icon(
                                    lucide_icons::icon_list_music(),
                                    16.0,
                                    if app.page == AppPage::Library
                                        && app.library_tab == LibraryTab::Playlists
                                    {
                                        ACCENT_RED.into()
                                    } else {
                                        hsla(220.0, 0.08, 0.50, 1.0)
                                    },
                                ))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.show_library_tab(LibraryTab::Playlists, cx);
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .id("mini-volume-mute-btn")
                                        .cursor_pointer()
                                        .child(themed_icon(
                                            if app.config.volume < 0.01 {
                                                lucide_icons::icon_volume_x()
                                            } else if app.config.volume < 0.5 {
                                                lucide_icons::icon_volume_1()
                                            } else {
                                                lucide_icons::icon_volume_2()
                                            },
                                            16.0,
                                            hsla(220.0, 0.08, 0.50, 1.0),
                                        ))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.pending_volume_ratio = None;
                                                this.toggle_mute(cx);
                                            }),
                                        ),
                                )
                                .child(
                                    interactive_slider(
                                        "mini-volume-bar",
                                        app.displayed_volume_ratio(),
                                        SliderStyle::mini_volume(),
                                        {
                                            let view = app_entity.clone();
                                            move |ratio, cx| {
                                                let _ = view.update(cx, |this, cx| {
                                                    if this.drag_target == Some(DragTarget::Volume)
                                                    {
                                                        this.update_drag_ratio(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    } else {
                                                        this.begin_drag(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    }
                                                    this.send(PlayerCommand::SetVolume(ratio));
                                                });
                                            }
                                        },
                                        {
                                            let view = app_entity.clone();
                                            move |ratio, cx| {
                                                let _ = view.update(cx, |this, cx| {
                                                    if this.drag_target == Some(DragTarget::Volume)
                                                    {
                                                        this.update_drag_ratio(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    } else {
                                                        this.begin_drag(
                                                            DragTarget::Volume,
                                                            ratio,
                                                            cx,
                                                        );
                                                    }
                                                    this.commit_drag(cx);
                                                });
                                            }
                                        },
                                    )
                                    .w(px(72.0))
                                    .on_scroll_wheel({
                                        let view = app_entity.clone();
                                        move |event: &gpui::ScrollWheelEvent, _window, cx| {
                                            cx.stop_propagation();
                                            let delta = event.delta.pixel_delta(px(48.0)).y;
                                            let _ = view.update(cx, |this, cx| {
                                                if delta < px(0.0) {
                                                    this.adjust_volume(0.04, cx);
                                                } else if delta > px(0.0) {
                                                    this.adjust_volume(-0.04, cx);
                                                }
                                            });
                                        }
                                    }),
                                ),
                        ),
                ),
        )
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
        .child(themed_icon(
            lucide_icons::icon_disc_3(),
            22.0,
            hsla(0.0, 0.0, 1.0, 0.85),
        ))
        .into_any_element()
}

// 保留独立 NowPlaying 窗口 API，旧舞台实现已经由 player_stage.rs 取代。
#[allow(dead_code)]
pub struct NowPlaying {
    engine: Option<Arc<AudioEngine>>,
    dynamic_blur: bool,
    artwork: Option<Arc<[u8]>>,
    timer_started: bool,
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
            timer_started: false,
        }
    }
}

impl Render for NowPlaying {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.timer_started {
            self.timer_started = true;
            cx.spawn(async move |this, cx| -> Result<()> {
                loop {
                    Timer::after(NOW_PLAYING_REFRESH_INTERVAL).await;
                    if this
                        .update(cx, |this, cx| {
                            if this
                                .engine
                                .as_ref()
                                .is_some_and(|engine| engine.progress().0 == PlaybackState::Playing)
                            {
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
                            .child(themed_icon(
                                lucide_icons::icon_disc_3(),
                                96.0,
                                hsla(0.0, 0.0, 1.0, 0.75),
                            ))
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
