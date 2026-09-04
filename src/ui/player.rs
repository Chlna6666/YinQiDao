use std::{sync::Arc, time::Duration};

use anyhow::Result;
use gpui::{
    Context, EncodedImageBytes, Entity, ImageFormat, IntoElement, ObjectFit, Render, SharedString,
    StatefulInteractiveElement as _, Timer, WeakEntity, Window, div, hsla, img, linear_color_stop,
    linear_gradient, prelude::*, px, relative, rgb,
};
use lucide_gpui::icons as lucide_icons;

use crate::{
    artwork::ArtworkPalette,
    audio::AudioEngine,
    lyrics::LyricLine,
    model::{AppPage, LibraryTab, PlaybackState, PlayerSnapshot, RepeatMode, Track},
};

use super::{
    components::{SliderStyle, smooth_slider},
    shell::{DragTarget, MusicApp},
    theme::{
        self, ACCENT_RED, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, TEXT_WHITE,
        elegant_gradient_for, format_remaining_time, format_time, press_transition, themed_icon,
    },
};

// ============================================================================
// 底部常驻播放器 (Apple Music Dock)
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
                    Timer::after(Duration::from_millis(100)).await;
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

        smooth_slider("mini-progress-track", ratio, SliderStyle::mini_progress())
            .w_full()
            .on_mouse_down(gpui::MouseButton::Left, {
                let parent = parent.clone();
                let this = this.clone();
                move |event, window, cx| {
                    cx.stop_propagation();
                    let width = f32::from(window.bounds().size.width).max(1.0);
                    let ratio = (f32::from(event.position.x) / width).clamp(0.0, 1.0);
                    let _ = parent.update(cx, |app, app_cx| {
                        app.begin_drag(DragTarget::Progress, ratio, app_cx);
                    });
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
            .on_mouse_move({
                let parent = parent.clone();
                let this = this.clone();
                move |event, window, cx| {
                    let width = f32::from(window.bounds().size.width).max(1.0);
                    let ratio = (f32::from(event.position.x) / width).clamp(0.0, 1.0);
                    let changed = parent
                        .update(cx, |app, app_cx| {
                            if (app.seeking || event.dragging())
                                && app.drag_target == Some(DragTarget::Progress)
                            {
                                app.update_drag_ratio(DragTarget::Progress, ratio, app_cx)
                            } else {
                                false
                            }
                        })
                        .unwrap_or(false);
                    if changed {
                        let _ = this.update(cx, |_, cx| cx.notify());
                    }
                }
            })
            .on_mouse_up(gpui::MouseButton::Left, {
                let parent = parent.clone();
                let this = this.clone();
                move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = parent.update(cx, |app, app_cx| app.commit_drag(app_cx));
                    let _ = this.update(cx, |_, cx| cx.notify());
                }
            })
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
                    Timer::after(Duration::from_millis(100)).await;
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
            .child(format_time(display_position))
            .child("/")
            .child(format_time(duration_ms))
    }
}

pub(super) fn mini_player(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> impl IntoElement {
    let snapshot = app.snapshot.clone();
    let is_playing = snapshot.state == PlaybackState::Playing;

    let track = snapshot.current_track.as_ref();
    let track_id = track.map(|t| t.id);
    let title = track.map_or("等待播放", |t| t.title.as_str());
    let artist = track.map_or("点击曲库开启音乐旅程", |t| t.artist.as_str());
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());

    div()
        .id("mini-player-container")
        .w_full()
        .bg(rgb(0xff_ff_ff))
        .border_t_1()
        .border_color(BORDER_HAIRLINE)
        .flex()
        .flex_col()
        .on_mouse_move(
            cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                if (this.seeking || event.dragging())
                    && this.drag_target == Some(DragTarget::Progress)
                {
                    let r = this.mini_progress_ratio(f32::from(event.position.x), window);
                    this.update_drag_ratio(DragTarget::Progress, r, cx);
                } else if (this.volume_dragging || event.dragging())
                    && this.drag_target == Some(DragTarget::Volume)
                {
                    let r = this.mini_volume_ratio(f32::from(event.position.x), window);
                    this.update_drag_ratio(DragTarget::Volume, r, cx);
                }
            }),
        )
        .on_mouse_up(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if this.drag_target.is_some() {
                    this.commit_drag(cx);
                }
            }),
        )
        // 顶部自适应进度条槽 (全宽自适应，悬停微增粗，带白色高亮拖拽圆点)
        .child(playback_progress)
        // 核心控制与信息排布
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_6()
                .py_2()
                .h(px(72.0))
                // 左侧：封面、歌名、歌手与心形收藏
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
                                .hover(|s| s.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|s| s.scale(0.98))
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
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.open_stage(cx);
                                })),
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
                                .hover(|s| s.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|s| s.scale(0.92))
                                .child(themed_icon(
                                    lucide_icons::icon_heart(),
                                    15.0,
                                    hsla(220.0, 0.08, 0.60, 1.0),
                                )),
                        ),
                )
                // 中间：核心播放按键组与时间指示
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
                                // 随机播放按键
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
                                        .hover(|s| s.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|s| s.scale(0.92))
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
                                // 上一曲
                                .child(
                                    div()
                                        .id("mini-previous")
                                        .size(px(32.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|s| s.scale(0.92))
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
                                // 主播放 / 暂停大圆形按键
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
                                        .hover(|s| s.opacity(0.90))
                                        .transition(press_transition())
                                        .active(|s| s.scale(0.94))
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
                                // 下一曲
                                .child(
                                    div()
                                        .id("mini-next")
                                        .size(px(32.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|s| s.scale(0.92))
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
                                // 循环模式
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
                                        .hover(|s| s.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .active(|s| s.scale(0.92))
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
                        // 时间微文本
                        .child(playback_time),
                )
                // 右侧：歌词快捷、待播队列、沉浸窗口与音量推子
                .child(
                    div()
                        .w(px(280.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_3()
                        // 歌词视图快捷入口 (切至沉浸舞台)
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
                                .hover(|s| s.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|s| s.scale(0.92))
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
                        // 待播清单按钮
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
                                .hover(|s| s.bg(theme::bg_hover()))
                                .transition(press_transition())
                                .active(|s| s.scale(0.92))
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
                        // 音量控制单元
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
                                                this.toggle_mute(cx);
                                            }),
                                        ),
                                )
                                // 自适应轻量音量滑槽 (支持点击与拖拽精准调节应用音量，带高亮拖拽圆点)
                                .child(
                                    smooth_slider(
                                        "mini-volume-bar",
                                        app.displayed_volume_ratio(),
                                        SliderStyle::mini_volume(),
                                    )
                                    .w(px(72.0))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        cx.listener(
                                            |this, event: &gpui::MouseDownEvent, window, cx| {
                                                cx.stop_propagation();
                                                let r = this.mini_volume_ratio(
                                                    f32::from(event.position.x),
                                                    window,
                                                );
                                                this.begin_drag(DragTarget::Volume, r, cx);
                                            },
                                        ),
                                    )
                                    .on_mouse_move(cx.listener(
                                        |this, event: &gpui::MouseMoveEvent, window, cx| {
                                            if (this.volume_dragging || event.dragging())
                                                && this.drag_target == Some(DragTarget::Volume)
                                            {
                                                let r = this.mini_volume_ratio(
                                                    f32::from(event.position.x),
                                                    window,
                                                );
                                                this.update_drag_ratio(DragTarget::Volume, r, cx);
                                            }
                                        },
                                    ))
                                    .on_mouse_up(
                                        gpui::MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.commit_drag(cx);
                                        }),
                                    ),
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

// ============================================================================
// 沉浸舞台与动态实时歌词 (Apple Music Time-Synced Lyrics Stage)
// ============================================================================

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let snapshot = app.snapshot.clone();
    let track = snapshot.current_track.clone();
    let track_id = track.as_ref().map(|t| t.id);
    let title = track.as_ref().map_or("未在播放音乐", |t| t.title.as_str());
    let artist = track.as_ref().map_or("请选择音乐", |t| t.artist.as_str());
    let album = track.as_ref().map_or("未知专辑", |t| t.album.as_str());
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());
    let blurred_artwork = track_id.and_then(|id| app.blurred_artworks.get(&id).cloned());
    let palette = track_id.and_then(|id| app.artwork_palettes.get(&id));
    let lyrics_doc = track_id.and_then(|id| app.lyrics.get(&id).cloned());
    let timed_lyrics = lyrics_doc
        .as_ref()
        .map(|d| d.timed_lines())
        .unwrap_or_default();

    div()
        .id("stage-player-root")
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(rgb(0x0e0f16))
        .text_color(TEXT_WHITE)
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.wake_stage_controls_immediately(cx);
            }),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.wake_stage_controls_immediately(cx);
        }))
        // 动态流体 Apple Music 风格氛围背景
        .child(stage_ambient_background(
            track_id,
            artwork.clone(),
            blurred_artwork,
            palette,
            app.config.dynamic_blur,
            app.config.blur_radius,
            snapshot.position_ms,
        ))
        // 前景内容区 (双栏沉浸舞台)
        .child(
            div()
                .id("stage-foreground-content")
                .absolute()
                .inset_0()
                .flex()
                .flex_col()
                .p_8()
                .gap_6()
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.wake_stage_controls_immediately(cx);
                    }),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.wake_stage_controls_immediately(cx);
                }))
                // 顶部状态栏 (随交互自隐匿与平滑唤醒)
                .child(
                    div()
                        .top(px(-(1.0 - app.stage_controls_visibility) * 36.0))
                        .opacity(app.stage_controls_visibility)
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(div()) // 保持左侧清爽留白，无需冗余文本
                        .child(
                            div().flex().items_center().gap_2().child(
                                div()
                                    .id("stage-enrich-btn")
                                    .px_3()
                                    .py_1()
                                    .rounded_full()
                                    .bg(hsla(0.0, 0.0, 1.0, 0.12))
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.20)))
                                    .transition(press_transition())
                                    .child("重新识别元数据与歌词")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.retry_current_enrichment(cx);
                                    })),
                            ),
                        ),
                )
                // 舞台双栏核心内容
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h(px(0.0))
                        .gap_12()
                        .items_center()
                        // 左栏：超高清专辑封面展台与音轨详细参数
                        .child(stage_left_column(
                            track.as_ref(),
                            artwork.clone(),
                            title,
                            artist,
                            album,
                        ))
                        // 右栏：Apple Music 实时同步滚动歌词
                        .child(stage_right_lyrics_column(
                            app,
                            timed_lyrics,
                            snapshot.position_ms,
                            cx,
                        )),
                )
                // 底部浮岛控制器
                .child(stage_bottom_controls(app, &snapshot, cx)),
        )
        .into_any_element()
}

fn stage_left_column(
    track: Option<&Track>,
    artwork: Option<Arc<[u8]>>,
    title: &str,
    artist: &str,
    album: &str,
) -> impl IntoElement {
    let codec = track.map_or("MP3", |t| t.codec.as_str());
    let sample_rate = track.map_or(44100, |t| t.sample_rate);
    let channels = track.map_or(2, |t| t.channels);

    div()
        .w(px(380.0))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_6()
        // 大黑胶/封面立体卡片
        .child(
            div()
                .size(px(280.0))
                .rounded_2xl()
                .overflow_hidden()
                .bg(rgb(0x18_19_24))
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.15))
                .child(if let Some(bytes) = artwork {
                    img(EncodedImageBytes::new(ImageFormat::Png, bytes))
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .into_any_element()
                } else {
                    let id = track.map_or(0, |t| t.id);
                    let (c1, c2) = elegant_gradient_for(id);
                    div()
                        .size_full()
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
                            hsla(0.0, 0.0, 1.0, 0.70),
                        ))
                        .into_any_element()
                }),
        )
        // 歌曲文字
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_WHITE)
                        .text_center()
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .text_base()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.75))
                        .child(artist.to_owned()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.45))
                        .child(album.to_owned()),
                ),
        )
        // Hi-Fi 规格参数徽章
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .rounded_full()
                .bg(hsla(0.0, 0.0, 1.0, 0.08))
                .border_1()
                .border_color(hsla(0.0, 0.0, 1.0, 0.12))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ACCENT_RED)
                        .child(codec.to_uppercase()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.65))
                        .child(format!("{} kHz · {} 声道", sample_rate / 1000, channels)),
                ),
        )
}

fn stage_right_lyrics_column(
    app: &MusicApp,
    timed_lyrics: &[LyricLine],
    position_ms: u64,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    if timed_lyrics.is_empty() {
        return div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(themed_icon(
                lucide_icons::icon_music(),
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
            )
            .into_any_element();
    }

    // 找到当前句歌词索引
    let current_index = timed_lyrics
        .iter()
        .rposition(|line| line.timestamp_ms <= position_ms)
        .unwrap_or(0);

    let offset_y = app.lyrics_current_offset;

    let mut lyrics_list = div()
        .id("stage-lyrics-inner-list")
        .w_full()
        .flex()
        .flex_col()
        .gap_5()
        .top(px(-offset_y))
        .pt(px(240.0))
        .pb(px(400.0));

    for (index, line) in timed_lyrics.iter().enumerate() {
        let is_current = index == current_index;
        let diff = (index as isize - current_index as isize).abs();
        let ts = line.timestamp_ms;
        let text = line.text.clone();

        // 纯净 Apple Music 风格层次排版（无突兀竖线，天然垂直左对齐）
        let (font_size, font_weight, text_color, scale) = if is_current {
            (
                px(30.0),
                gpui::FontWeight::BOLD,
                hsla(0.0, 0.0, 1.0, 1.0),
                1.02,
            )
        } else if diff == 1 {
            (
                px(22.0),
                gpui::FontWeight::SEMIBOLD,
                hsla(0.0, 0.0, 1.0, 0.58),
                1.00,
            )
        } else if diff == 2 {
            (
                px(18.0),
                gpui::FontWeight::MEDIUM,
                hsla(0.0, 0.0, 1.0, 0.36),
                0.98,
            )
        } else {
            (
                px(16.0),
                gpui::FontWeight::NORMAL,
                hsla(0.0, 0.0, 1.0, 0.18),
                0.96,
            )
        };

        // 处理中英/双语歌词换行排版
        let mut line_content = div()
            .flex()
            .flex_col()
            .gap_1()
            .font_weight(font_weight)
            .scale(scale)
            .transition(theme::hover_transition());

        let lines: Vec<&str> = text.splitn(2, '\n').collect();
        if lines.len() == 2 {
            line_content = line_content
                .child(
                    div()
                        .text_size(font_size)
                        .text_color(text_color)
                        .child(lines[0].to_owned()),
                )
                .child(
                    div()
                        .text_size(px(f32::from(font_size) * 0.72))
                        .text_color(text_color.opacity(if is_current { 0.72 } else { 0.50 }))
                        .child(lines[1].to_owned()),
                );
        } else {
            line_content = line_content.child(
                div()
                    .text_size(font_size)
                    .text_color(text_color)
                    .child(text),
            );
        }

        lyrics_list = lyrics_list.child(
            div()
                .id(SharedString::from(format!("lyric-line-{index}")))
                .cursor_pointer()
                .transition(press_transition())
                .hover(|s| s.opacity(1.0).scale(1.01))
                .active(|s| s.scale(0.98))
                .py_1p5()
                .pl_4()
                .child(line_content)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.seek_to_ms(ts, cx);
                        this.lyrics_target_offset = (index as f32) * 60.0;
                        this.lyrics_user_scrolling_until = None;
                        this.wake_stage_controls_immediately(cx);
                    }),
                ),
        );
    }

    div()
        .id("stage-lyrics-viewport")
        .relative()
        .flex_1()
        .h_full()
        .overflow_hidden()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.wake_stage_controls_immediately(cx);
            }),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.wake_stage_controls_immediately(cx);
        }))
        .on_scroll_wheel(cx.listener(|this, event: &gpui::ScrollWheelEvent, _, cx| {
            let delta_y = match event.delta {
                gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
                gpui::ScrollDelta::Lines(l) => l.y * 32.0,
            };
            this.lyrics_current_offset = (this.lyrics_current_offset - delta_y).max(0.0);
            this.lyrics_target_offset = this.lyrics_current_offset;
            this.lyrics_user_scrolling_until =
                Some(std::time::Instant::now() + Duration::from_secs(3));
            cx.notify();
        }))
        .child(lyrics_list)
        .into_any_element()
}

fn stage_bottom_controls(
    app: &MusicApp,
    snapshot: &PlayerSnapshot,
    cx: &mut Context<MusicApp>,
) -> impl IntoElement {
    let ratio = app.displayed_progress_ratio();
    let display_pos_ms = app.displayed_position_ms();
    let display_vol = app.displayed_volume_ratio();
    let is_playing = snapshot.state == PlaybackState::Playing;
    let v = app.stage_controls_visibility;
    let y_offset = (1.0 - v) * 56.0;

    div()
        .id("stage-bottom-dock")
        .top(px(y_offset))
        .opacity(v)
        .flex()
        .items_center()
        .justify_between()
        .px_6()
        .py_3()
        .rounded_2xl()
        .bg(hsla(0.0, 0.0, 0.0, 0.40))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.10))
        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
            this.stage_controls_hovered = *hovered;
            if *hovered && this.stage_controls_visibility > 0.05 {
                this.wake_stage_controls(cx);
            }
        }))
        .on_mouse_move(
            cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                this.handle_stage_mouse_move(event.position, cx);
                if (this.seeking || event.dragging())
                    && this.drag_target == Some(DragTarget::Progress)
                {
                    let r = this.stage_progress_ratio(f32::from(event.position.x), window);
                    this.update_drag_ratio(DragTarget::Progress, r, cx);
                } else if (this.volume_dragging || event.dragging())
                    && this.drag_target == Some(DragTarget::Volume)
                {
                    let r = this.stage_volume_ratio(f32::from(event.position.x), window);
                    this.update_drag_ratio(DragTarget::Volume, r, cx);
                }
            }),
        )
        .on_mouse_up(
            gpui::MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                if this.drag_target.is_some() {
                    this.commit_drag(cx);
                }
            }),
        )
        // 时间与全宽进度条
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.65))
                        .child(format_time(display_pos_ms)),
                )
                .child(
                    smooth_slider("stage-progress-track", ratio, SliderStyle::stage_progress())
                        .flex_1()
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                let r =
                                    this.stage_progress_ratio(f32::from(event.position.x), window);
                                this.begin_drag(DragTarget::Progress, r, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            |this, event: &gpui::MouseMoveEvent, window, cx| {
                                if (this.seeking || event.dragging())
                                    && this.drag_target == Some(DragTarget::Progress)
                                {
                                    let r = this
                                        .stage_progress_ratio(f32::from(event.position.x), window);
                                    this.update_drag_ratio(DragTarget::Progress, r, cx);
                                }
                            },
                        ))
                        .on_mouse_up(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.commit_drag(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(hsla(0.0, 0.0, 1.0, 0.65))
                        .child(format_remaining_time(display_pos_ms, snapshot.duration_ms)),
                ),
        )
        // 核心控制区：[随机播放] [上一曲] [播放/暂停] [下一曲] [循环模式] [待播清单]
        .child(
            div()
                .flex()
                .items_center()
                .gap_3p5()
                .px_6()
                // 1. 随机播放
                .child(
                    div()
                        .id("stage-shuffle-btn")
                        .size(px(34.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if app.config.shuffle {
                            hsla(0.0, 0.0, 1.0, 0.20)
                        } else {
                            hsla(0.0, 0.0, 0.0, 0.0)
                        })
                        .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.25)))
                        .active(|s| s.scale(0.92))
                        .child(themed_icon(
                            lucide_icons::icon_shuffle(),
                            16.0,
                            if app.config.shuffle {
                                ACCENT_RED.into()
                            } else {
                                hsla(0.0, 0.0, 1.0, 0.70)
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
                // 2. 上一曲
                .child(
                    div()
                        .id("stage-prev-btn")
                        .size(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.15)))
                        .active(|s| s.scale(0.92))
                        .child(themed_icon(
                            lucide_icons::icon_skip_back(),
                            20.0,
                            hsla(0.0, 0.0, 1.0, 0.85),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.previous(cx);
                            }),
                        ),
                )
                // 3. 播放 / 暂停 (醒目大圆纽)
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
                        .hover(|s| s.opacity(0.88).scale(1.04))
                        .active(|s| s.scale(0.95))
                        .shadow_md()
                        .child(themed_icon(
                            if is_playing {
                                lucide_icons::icon_pause()
                            } else {
                                lucide_icons::icon_play()
                            },
                            22.0,
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
                // 4. 下一曲
                .child(
                    div()
                        .id("stage-next-btn")
                        .size(px(36.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.15)))
                        .active(|s| s.scale(0.92))
                        .child(themed_icon(
                            lucide_icons::icon_skip_forward(),
                            20.0,
                            hsla(0.0, 0.0, 1.0, 0.85),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.next(cx);
                            }),
                        ),
                )
                // 5. 循环模式切换
                .child(
                    div()
                        .id("stage-repeat-btn")
                        .size(px(34.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if snapshot.repeat != RepeatMode::Off {
                            hsla(0.0, 0.0, 1.0, 0.20)
                        } else {
                            hsla(0.0, 0.0, 0.0, 0.0)
                        })
                        .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.25)))
                        .active(|s| s.scale(0.92))
                        .child(themed_icon(
                            match snapshot.repeat {
                                RepeatMode::Off | RepeatMode::All => lucide_icons::icon_repeat(),
                                RepeatMode::One => lucide_icons::icon_repeat_1(),
                            },
                            17.0,
                            if snapshot.repeat != RepeatMode::Off {
                                ACCENT_RED.into()
                            } else {
                                hsla(0.0, 0.0, 1.0, 0.70)
                            },
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.cycle_repeat(cx);
                            }),
                        ),
                )
                // 6. 待播队列清单
                .child(
                    div()
                        .id("stage-queue-btn")
                        .size(px(34.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .cursor_pointer()
                        .hover(|s| s.bg(hsla(0.0, 0.0, 1.0, 0.15)))
                        .active(|s| s.scale(0.92))
                        .child(themed_icon(
                            lucide_icons::icon_list_music(),
                            18.0,
                            hsla(0.0, 0.0, 1.0, 0.70),
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.show_library_tab(LibraryTab::Songs, cx);
                            }),
                        ),
                ),
        )
        // 右侧空间音频与均衡器快捷
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("stage-spatial-btn")
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if app.config.spatial.enabled {
                            ACCENT_RED.into()
                        } else {
                            hsla(0.0, 0.0, 1.0, 0.10)
                        })
                        .text_xs()
                        .child("空间音频")
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_spatial(cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .id("stage-eq-btn")
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(if app.config.eq.enabled {
                            ACCENT_RED.into()
                        } else {
                            hsla(0.0, 0.0, 1.0, 0.10)
                        })
                        .text_xs()
                        .child("EQ")
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_eq(cx);
                            }),
                        ),
                )
                // 交互式音量滑动控制单元 (支持点击与平滑拖拽，带圆点 Thumb)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(hsla(0.0, 0.0, 1.0, 0.08))
                        .child(
                            div()
                                .id("stage-vol-mute-btn")
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.80))
                                .child(themed_icon(
                                    if display_vol <= 0.001 {
                                        lucide_icons::icon_volume_x()
                                    } else if display_vol < 0.5 {
                                        lucide_icons::icon_volume_1()
                                    } else {
                                        lucide_icons::icon_volume_2()
                                    },
                                    16.0,
                                    hsla(0.0, 0.0, 1.0, 0.85),
                                ))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.toggle_mute(cx);
                                    }),
                                ),
                        )
                        // 音量滑槽与白色拖拽圆点 (统一使用 smooth_slider 工业级滑块)
                        .child(
                            smooth_slider(
                                "stage-vol-track",
                                display_vol,
                                SliderStyle::stage_volume(),
                            )
                            .w(px(72.0))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    let r = this
                                        .stage_volume_ratio(f32::from(event.position.x), window);
                                    this.begin_drag(DragTarget::Volume, r, cx);
                                }),
                            )
                            .on_mouse_move(cx.listener(
                                |this, event: &gpui::MouseMoveEvent, window, cx| {
                                    if (this.volume_dragging || event.dragging())
                                        && this.drag_target == Some(DragTarget::Volume)
                                    {
                                        let r = this.stage_volume_ratio(
                                            f32::from(event.position.x),
                                            window,
                                        );
                                        this.update_drag_ratio(DragTarget::Volume, r, cx);
                                    }
                                },
                            ))
                            .on_mouse_up(
                                gpui::MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.commit_drag(cx);
                                }),
                            ),
                        )
                        .child(
                            div()
                                .id("stage-vol-pct")
                                .min_w(px(32.0))
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(hsla(0.0, 0.0, 1.0, 0.80))
                                .child(format!("{}%", (display_vol * 100.0).round() as u32)),
                        ),
                ),
        )
}

fn fluid_palette_for(id: i64) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    let base_hue = ((id.unsigned_abs() * 47) % 360) as f32;
    let c1 = hsla(base_hue, 0.70, 0.45, 0.75);
    let c2 = hsla((base_hue + 75.0) % 360.0, 0.75, 0.40, 0.70);
    let c3 = hsla((base_hue + 160.0) % 360.0, 0.80, 0.50, 0.65);
    let c4 = hsla((base_hue + 230.0) % 360.0, 0.65, 0.35, 0.75);
    (c1, c2, c3, c4)
}

fn rgb_to_hsla(rgb: [u8; 3], alpha: f32) -> gpui::Hsla {
    let r = rgb[0] as f32 / 255.0;
    let g = rgb[1] as f32 / 255.0;
    let b = rgb[2] as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;

    if (max - min).abs() < 1e-4 {
        return hsla(0.0, 0.0, l, alpha);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < 1e-4 {
        let mut h = (g - b) / d;
        if g < b {
            h += 6.0;
        }
        h / 6.0
    } else if (max - g).abs() < 1e-4 {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };

    hsla(h, (s * 1.30).clamp(0.25, 0.95), l.clamp(0.22, 0.65), alpha)
}

fn stage_ambient_background(
    track_id: Option<i64>,
    artwork: Option<Arc<[u8]>>,
    blurred_artwork: Option<Arc<[u8]>>,
    palette: Option<&ArtworkPalette>,
    dynamic_blur: bool,
    blur_radius: f32,
    position_ms: u64,
) -> impl IntoElement {
    let t = (position_ms as f32) / 1000.0;
    let id = track_id.unwrap_or(0);

    // 提取 Apple Music 风格 3 阶高动态流体色卡 (主活动色、辅光色、深层阴影色) 与暗场基底
    let (c1, c2, c3, dark_bg, mask_alpha) = if let Some(pal) = palette {
        let hex = ((pal.dark_ambient_rgb[0] as u32) << 16)
            | ((pal.dark_ambient_rgb[1] as u32) << 8)
            | (pal.dark_ambient_rgb[2] as u32);
        let dom = rgb_to_hsla(pal.dominant_rgb, 0.70);
        let sec = rgb_to_hsla(pal.secondary_rgb, 0.60);
        let amb = hsla(
            ((pal.dominant_rgb[0] as f32 * 1.6 + pal.secondary_rgb[1] as f32) % 360.0) / 360.0,
            0.75,
            0.50,
            0.55,
        );
        (dom, sec, amb, rgb(hex), pal.mask_alpha)
    } else {
        let (f1, f2, f3, _f4) = fluid_palette_for(id);
        (f1, f2, f3, rgb(0x10_11_1a), 0.62)
    };

    let blur_bytes = blurred_artwork.or(artwork);
    let diffusion = (blur_radius / 16.0).clamp(0.6, 2.5);

    // Apple Music 多 Blob 互质自旋与有机漂移数学模型 (Coprime Harmonic Motions)
    let (angle1, angle2, angle3, d1_x, d1_y, scale1, d2_x, d2_y, scale2, d3_x, d3_y, scale3) =
        if dynamic_blur {
            let a1 = (t * 8.2 + 35.0) % 360.0;
            let a2 = ((t * -6.1) + 190.0) % 360.0;
            let a3 = (t * 4.5 + 280.0) % 360.0;

            let d1_x = 6.0 * (t * 0.137).sin();
            let d1_y = 5.0 * (t * 0.093).cos();
            let scale1 = 1.30 + 0.06 * diffusion + 0.04 * (t * 0.115).sin();

            let d2_x = -7.0 * (t * 0.083).cos();
            let d2_y = -6.0 * (t * 0.127).sin();
            let scale2 = 1.28 + 0.05 * diffusion + 0.04 * (t * 0.103).cos();

            let d3_x = 4.5 * (t * 0.071).cos();
            let d3_y = -4.0 * (t * 0.097).sin();
            let scale3 = 1.22 + 0.04 * diffusion + 0.03 * (t * 0.088).sin();

            (
                a1, a2, a3, d1_x, d1_y, scale1, d2_x, d2_y, scale2, d3_x, d3_y, scale3,
            )
        } else {
            (
                45.0,
                215.0,
                310.0,
                0.0,
                0.0,
                1.25 * diffusion,
                0.0,
                0.0,
                1.25 * diffusion,
                0.0,
                0.0,
                1.20 * diffusion,
            )
        };

    let mut fluid_stage = div().absolute().inset_0().overflow_hidden().bg(dark_bg);

    // 底层：GPU 硬件双线性拉伸的高斯微缩封面自然底图 (赋予真实专辑质感底色)
    if let Some(bytes) = blur_bytes {
        let breathe = if dynamic_blur {
            1.16 + 0.03 * (t * 0.06).sin()
        } else {
            1.15
        };
        fluid_stage = fluid_stage.child(
            div().absolute().inset_0().scale(breathe).child(
                img(EncodedImageBytes::new(ImageFormat::Png, bytes))
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .opacity(0.46),
            ),
        );
    }

    // Blob 1: 主活动色流体层 (Primary Accent Wave)
    let blob1 = div()
        .absolute()
        .left(relative(-0.15 + d1_x / 100.0))
        .top(relative(-0.15 + d1_y / 100.0))
        .w(relative(1.30))
        .h(relative(1.30))
        .scale(scale1)
        .bg(linear_gradient(
            angle1,
            linear_color_stop(c1.opacity(0.55), 0.0),
            linear_color_stop(c1.opacity(0.0), 0.85),
        ));

    // Blob 2: 辅光色自旋交融层 (Secondary Glow Swirl)
    let blob2 = div()
        .absolute()
        .right(relative(-0.15 + d2_x / 100.0))
        .bottom(relative(-0.15 + d2_y / 100.0))
        .w(relative(1.35))
        .h(relative(1.35))
        .scale(scale2)
        .bg(linear_gradient(
            angle2,
            linear_color_stop(c2.opacity(0.48), 0.0),
            linear_color_stop(c2.opacity(0.0), 0.80),
        ));

    // Blob 3: 深层阴影与环境干涉层 (Ambient Mood & Depth)
    let blob3 = div()
        .absolute()
        .left(relative(-0.10 + d3_x / 100.0))
        .bottom(relative(-0.10 + d3_y / 100.0))
        .w(relative(1.25))
        .h(relative(1.25))
        .scale(scale3)
        .bg(linear_gradient(
            angle3,
            linear_color_stop(c3.opacity(0.38), 0.0),
            linear_color_stop(c3.opacity(0.0), 0.75),
        ));

    fluid_stage
        .child(blob1)
        .child(blob2)
        .child(blob3)
        // 顶层 Apple Music 级自适应暗场渐变遮罩 (保证前景白色歌词与控制组件符合 WCAG AAA 对比度)
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.0,
            linear_color_stop(
                hsla(0.0, 0.0, 0.02, (mask_alpha * 0.75).clamp(0.35, 0.85)),
                0.0,
            ),
            linear_color_stop(
                hsla(0.0, 0.0, 0.01, (mask_alpha + 0.08).clamp(0.45, 0.92)),
                1.0,
            ),
        )))
}

// ============================================================================
// NowPlaying 独立窗口结构
// ============================================================================

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
                    Timer::after(Duration::from_millis(120)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
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
            .map_or_else(PlayerSnapshot::default, |e| e.snapshot());
        let track = snapshot.current_track.as_ref();
        let track_id = track.map(|t| t.id);
        let title = track.map_or("等待播放", |t| t.title.as_str());
        let artist = track.map_or("从音栖岛歌库选择歌曲", |t| t.artist.as_str());
        let album = track.map_or("暂无专辑", |t| t.album.as_str());

        div()
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(rgb(0x0e0f16))
            .text_color(TEXT_WHITE)
            .child(stage_ambient_background(
                track_id,
                self.artwork.clone(),
                self.artwork.clone(),
                None,
                self.dynamic_blur,
                16.0,
                snapshot.position_ms,
            ))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .flex_col()
                    .p_8()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_base()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child("音栖岛 · 沉浸播放"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(hsla(0.0, 0.0, 1.0, 0.5))
                                    .child("全屏模式"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h(px(0.0))
                            .gap_12()
                            .items_center()
                            .child(stage_left_column(
                                track,
                                self.artwork.clone(),
                                title,
                                artist,
                                album,
                            )),
                    ),
            )
    }
}
