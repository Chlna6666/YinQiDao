use std::sync::Arc;

use gpui::{
    BorrowAppContext as _, Context, EncodedImageBytes, Entity, Global, ImageFormat, IntoElement,
    ObjectFit, Render, SharedString, StatefulInteractiveElement as _, WeakEntity, Window, div, hsla,
    img, linear_color_stop, linear_gradient, prelude::*, px, rgb,
};
use lucide_gpui::icon;

use crate::{
    audio::PlayerCommand,
    model::{AppPage, LibraryTab, PlaybackState, RepeatMode, TrackId},
};

use super::{
    components::{SliderStyle, interactive_slider},
    player_stage::{PlaybackProgress, PlaybackTime},
    shell::{DragTarget, MusicApp},
    theme::{
        self, ACCENT_RED, TEXT_PRIMARY, TEXT_SECONDARY, elegant_gradient_for, press_transition,
        themed_icon,
    },
};

#[derive(Default)]
struct MiniPlayerViewCache {
    view: Option<Entity<MiniPlayerView>>,
}

impl Global for MiniPlayerViewCache {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MiniPlayerRenderKey {
    track_id: Option<TrackId>,
    artwork_ptr: usize,
    artwork_len: usize,
    playback_state: PlaybackState,
    shuffle: bool,
    repeat: RepeatMode,
    desktop_lyrics_visible: bool,
    queue_active: bool,
    slider_volume_bits: u32,
    icon_volume_bits: u32,
}

#[derive(Clone)]
struct MiniTrackRenderData {
    track_id: Option<TrackId>,
    title: SharedString,
    artist: SharedString,
    artwork: Option<Arc<[u8]>>,
}

impl Default for MiniTrackRenderData {
    fn default() -> Self {
        Self {
            track_id: None,
            title: SharedString::new_static("等待播放"),
            artist: SharedString::new_static("点击曲库开启音乐旅程"),
            artwork: None,
        }
    }
}

pub(super) fn view(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
) -> Entity<MiniPlayerView> {
    let parent = cx.entity().downgrade();
    let initial_progress = playback_progress.clone();
    let initial_time = playback_time.clone();
    let view = cx.update_default_global(move |cache: &mut MiniPlayerViewCache, cx| {
        if let Some(view) = &cache.view {
            return view.clone();
        }
        let view = cx.new(move |_| MiniPlayerView {
            parent,
            playback_progress: initial_progress,
            playback_time: initial_time,
            track: MiniTrackRenderData::default(),
            key: MiniPlayerRenderKey {
                track_id: None,
                artwork_ptr: 0,
                artwork_len: 0,
                playback_state: PlaybackState::Stopped,
                shuffle: false,
                repeat: RepeatMode::Off,
                desktop_lyrics_visible: false,
                queue_active: false,
                slider_volume_bits: 1.0_f32.to_bits(),
                icon_volume_bits: 1.0_f32.to_bits(),
            },
            slider_volume: 1.0,
            icon_volume: 1.0,
        });
        cache.view = Some(view.clone());
        view
    });

    let track = app.snapshot.current_track.as_ref();
    let track_id = track.map(|track| track.id);
    let artwork = track_id.and_then(|id| app.artworks.get(&id).cloned());
    let slider_volume = app.displayed_volume_ratio();
    let icon_volume = app.config.volume;
    let key = MiniPlayerRenderKey {
        track_id,
        artwork_ptr: artwork.as_ref().map_or(0, |bytes| bytes.as_ptr() as usize),
        artwork_len: artwork.as_ref().map_or(0, |bytes| bytes.len()),
        playback_state: app.snapshot.state,
        shuffle: app.snapshot.shuffle,
        repeat: app.snapshot.repeat,
        desktop_lyrics_visible: app.config.desktop_lyrics.visible,
        queue_active: app.page == AppPage::Library && app.library_tab == LibraryTab::Playlists,
        slider_volume_bits: slider_volume.to_bits(),
        icon_volume_bits: icon_volume.to_bits(),
    };

    view.update(cx, |view, cx| {
        let track_changed = view.key.track_id != key.track_id
            || view.key.artwork_ptr != key.artwork_ptr
            || view.key.artwork_len != key.artwork_len;
        if track_changed {
            view.track = if let Some(track) = track {
                MiniTrackRenderData {
                    track_id: Some(track.id),
                    title: SharedString::from(track.title.clone()),
                    artist: SharedString::from(track.artist.clone()),
                    artwork: artwork.clone(),
                }
            } else {
                MiniTrackRenderData {
                    artwork: artwork.clone(),
                    ..MiniTrackRenderData::default()
                }
            };
        }
        if view.key != key {
            view.key = key;
            view.slider_volume = slider_volume;
            view.icon_volume = icon_volume;
            cx.notify();
        }
    });

    view
}

pub(super) struct MiniPlayerView {
    parent: WeakEntity<MusicApp>,
    playback_progress: Entity<PlaybackProgress>,
    playback_time: Entity<PlaybackTime>,
    track: MiniTrackRenderData,
    key: MiniPlayerRenderKey,
    slider_volume: f32,
    icon_volume: f32,
}

impl Render for MiniPlayerView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let is_playing = self.key.playback_state == PlaybackState::Playing;
        let parent = self.parent.clone();
        let slider_volume = self.slider_volume;
        let icon_volume = self.icon_volume;

        div()
            .id("mini-player-container")
            .w_full()
            .relative()
            .bg(rgb(0xff_ff_ff))
            .flex()
            .flex_col()
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
                                    .child(mini_cover_element(
                                        self.track.track_id,
                                        self.track.artwork.clone(),
                                    ))
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
                                                    .child(self.track.title.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(TEXT_SECONDARY)
                                                    .truncate()
                                                    .child(self.track.artist.clone()),
                                            ),
                                    )
                                    .on_mouse_down(gpui::MouseButton::Left, {
                                        let parent = parent.clone();
                                        move |_, _, cx| {
                                            let _ = parent.update(cx, |app, app_cx| {
                                                app.open_stage(app_cx);
                                            });
                                        }
                                    }),
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
                                        icon!(heart),
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
                                    .child(control_button(
                                        "mini-shuffle",
                                        icon!(shuffle),
                                        true,
                                        self.key.shuffle,
                                        {
                                            let parent = parent.clone();
                                            move |cx| {
                                                let _ = parent.update(cx, |app, app_cx| {
                                                    app.toggle_shuffle(app_cx);
                                                });
                                            }
                                        },
                                    ))
                                    .child(control_button(
                                        "mini-previous",
                                        icon!(skip_back),
                                        false,
                                        false,
                                        {
                                            let parent = parent.clone();
                                            move |cx| {
                                                let _ = parent.update(cx, |app, app_cx| {
                                                    app.previous(app_cx);
                                                });
                                            }
                                        },
                                    ))
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
                                                if is_playing { icon!(pause) } else { icon!(play) },
                                                18.0,
                                                hsla(0.0, 0.0, 1.0, 1.0),
                                            ))
                                            .on_mouse_down(gpui::MouseButton::Left, {
                                                let parent = parent.clone();
                                                move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    let _ = parent.update(cx, |app, app_cx| {
                                                        app.toggle_play(app_cx);
                                                    });
                                                }
                                            }),
                                    )
                                    .child(control_button(
                                        "mini-next",
                                        icon!(skip_forward),
                                        false,
                                        false,
                                        {
                                            let parent = parent.clone();
                                            move |cx| {
                                                let _ = parent.update(cx, |app, app_cx| {
                                                    app.next(app_cx);
                                                });
                                            }
                                        },
                                    ))
                                    .child(repeat_button(self.key.repeat, {
                                        let parent = parent.clone();
                                        move |cx| {
                                            let _ = parent.update(cx, |app, app_cx| {
                                                app.cycle_repeat(app_cx);
                                            });
                                        }
                                    })),
                            )
                            .child(self.playback_time.clone()),
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
                                    .id("mini-desktop-lyrics-btn")
                                    .size(px(30.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(if self.key.desktop_lyrics_visible {
                                        theme::accent_red_muted()
                                    } else {
                                        hsla(0.0, 0.0, 0.0, 0.0)
                                    })
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(if self.key.desktop_lyrics_visible {
                                        ACCENT_RED
                                    } else {
                                        rgb(0x78_7f_8c)
                                    })
                                    .hover(|style| style.bg(theme::bg_hover()))
                                    .transition(press_transition())
                                    .active(|style| style.scale(0.92))
                                    .on_mouse_down(gpui::MouseButton::Left, {
                                        let parent = parent.clone();
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            let _ = parent.update(cx, |app, app_cx| {
                                                app.toggle_desktop_lyrics_visible(app_cx);
                                            });
                                        }
                                    })
                                    .child("词"),
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
                                    .bg(if self.key.queue_active {
                                        theme::accent_red_muted()
                                    } else {
                                        hsla(0.0, 0.0, 0.0, 0.0)
                                    })
                                    .hover(|style| style.bg(theme::bg_hover()))
                                    .transition(press_transition())
                                    .active(|style| style.scale(0.92))
                                    .child(themed_icon(
                                        icon!(list_music),
                                        16.0,
                                        if self.key.queue_active {
                                            ACCENT_RED.into()
                                        } else {
                                            hsla(220.0, 0.08, 0.50, 1.0)
                                        },
                                    ))
                                    .on_mouse_down(gpui::MouseButton::Left, {
                                        let parent = parent.clone();
                                        move |_, _, cx| {
                                            cx.stop_propagation();
                                            let _ = parent.update(cx, |app, app_cx| {
                                                app.show_library_tab(LibraryTab::Playlists, app_cx);
                                            });
                                        }
                                    }),
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
                                                if icon_volume < 0.01 {
                                                    icon!(volume_x)
                                                } else if icon_volume < 0.5 {
                                                    icon!(volume_1)
                                                } else {
                                                    icon!(volume_2)
                                                },
                                                16.0,
                                                hsla(220.0, 0.08, 0.50, 1.0),
                                            ))
                                            .on_mouse_down(gpui::MouseButton::Left, {
                                                let parent = parent.clone();
                                                move |_, _, cx| {
                                                    cx.stop_propagation();
                                                    let _ = parent.update(cx, |app, app_cx| {
                                                        app.pending_volume_ratio = None;
                                                        app.toggle_mute(app_cx);
                                                    });
                                                }
                                            }),
                                    )
                                    .child(
                                        interactive_slider(
                                            "mini-volume-bar",
                                            slider_volume,
                                            SliderStyle::mini_volume(),
                                            {
                                                let parent = parent.clone();
                                                move |ratio, cx| {
                                                    let _ = parent.update(cx, |app, app_cx| {
                                                        app.pending_volume_ratio = None;
                                                        app.set_app_volume(ratio, app_cx);
                                                    });
                                                }
                                            },
                                            {
                                                let parent = parent.clone();
                                                move |ratio, cx| {
                                                    let _ = parent.update(cx, |app, app_cx| {
                                                        if app.drag_target == Some(DragTarget::Volume) {
                                                            app.update_drag_ratio(
                                                                DragTarget::Volume,
                                                                ratio,
                                                                app_cx,
                                                            );
                                                        } else {
                                                            app.begin_drag(
                                                                DragTarget::Volume,
                                                                ratio,
                                                                app_cx,
                                                            );
                                                        }
                                                        app.send(PlayerCommand::SetVolume(ratio));
                                                    });
                                                }
                                            },
                                            {
                                                let parent = parent.clone();
                                                move |ratio, cx| {
                                                    let _ = parent.update(cx, |app, app_cx| {
                                                        if app.drag_target == Some(DragTarget::Volume) {
                                                            app.update_drag_ratio(
                                                                DragTarget::Volume,
                                                                ratio,
                                                                app_cx,
                                                            );
                                                        } else {
                                                            app.begin_drag(
                                                                DragTarget::Volume,
                                                                ratio,
                                                                app_cx,
                                                            );
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
                                            move |event: &gpui::ScrollWheelEvent, _window, cx| {
                                                cx.stop_propagation();
                                                let delta = event.delta.pixel_delta(px(48.0)).y;
                                                let _ = parent.update(cx, |app, app_cx| {
                                                    if delta < px(0.0) {
                                                        app.adjust_volume(0.04, app_cx);
                                                    } else if delta > px(0.0) {
                                                        app.adjust_volume(-0.04, app_cx);
                                                    }
                                                });
                                            }
                                        }),
                                    ),
                            ),
                    ),
            )
            .child(self.playback_progress.clone())
    }
}

fn control_button(
    id: &'static str,
    icon_name: &'static str,
    compact: bool,
    active: bool,
    action: impl Fn(&mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(if compact { px(28.0) } else { px(32.0) })
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(if active {
            theme::accent_red_muted()
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .hover(|style| style.bg(theme::bg_hover()))
        .transition(press_transition())
        .active(|style| style.scale(0.92))
        .child(themed_icon(
            icon_name,
            if compact { 15.0 } else { 18.0 },
            if active {
                ACCENT_RED.into()
            } else if compact {
                hsla(220.0, 0.08, 0.50, 1.0)
            } else {
                hsla(220.0, 0.10, 0.35, 1.0)
            },
        ))
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            action(cx);
        })
}

fn repeat_button(
    repeat: RepeatMode,
    action: impl Fn(&mut gpui::App) + 'static,
) -> impl IntoElement {
    let active = repeat != RepeatMode::Off;
    div()
        .id("mini-repeat")
        .size(px(28.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(if active {
            theme::accent_red_muted()
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .hover(|style| style.bg(theme::bg_hover()))
        .transition(press_transition())
        .active(|style| style.scale(0.92))
        .child(themed_icon(
            match repeat {
                RepeatMode::Off | RepeatMode::All => icon!(repeat),
                RepeatMode::One => icon!(repeat_1),
            },
            15.0,
            if active {
                ACCENT_RED.into()
            } else {
                hsla(220.0, 0.08, 0.50, 1.0)
            },
        ))
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            action(cx);
        })
}

fn mini_cover_element(track_id: Option<TrackId>, artwork: Option<Arc<[u8]>>) -> impl IntoElement {
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
        .child(themed_icon(icon!(disc_3), 22.0, hsla(0.0, 0.0, 1.0, 0.85)))
        .into_any_element()
}
