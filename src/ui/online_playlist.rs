use std::{ops::Range, sync::Arc};

use gpui::{App, IntoElement, WeakEntity, div, prelude::*, px, uniform_list};
use lucide_gpui::icon;

use crate::{
    model::AppPage,
    plugin::abi::{PluginRoute, RemoteTrack},
    ui::{
        image_cache,
        shell::{MusicApp, OnlinePlaylistViewData, app_listener},
        theme::{
            self, ACCENT_RED, BORDER_CARD, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, TEXT_WHITE,
            format_time, press_transition, themed_icon,
        },
    },
};

pub(super) fn render<V: 'static>(
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
    cx: &gpui::Context<V>,
) -> gpui::AnyElement {
    let Some(data) = &app.active_online_playlist else {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(TEXT_SECONDARY)
            .child("暂无歌单数据")
            .into_any_element();
    };

    let title = data.title.clone();
    let subtitle = data.subtitle.clone();
    let cover_url = data.cover_url.clone();
    let track_count = data.tracks.len();
    let loading = data.loading;
    let route = data.route.clone();
    let tracks = data.tracks.clone();
    let first_track = tracks.first().cloned();
    let all_route = route.clone();
    let all_title = title.clone();

    let display_subtitle =
        if subtitle.starts_with("共 ") || subtitle == format!("共 {track_count} 首歌曲") {
            String::new()
        } else {
            subtitle
        };

    div()
        .id("online-playlist-page")
        .size_full()
        .overflow_hidden()
        .flex()
        .flex_col()
        .px_8()
        .pt_5()
        .pb_6()
        .gap_3p5()
        // Top compact navigation
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_sm()
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(TEXT_SECONDARY)
                .hover(|s| s.text_color(ACCENT_RED))
                .transition(theme::hover_transition())
                .child(themed_icon(icon!(arrow_left), 16.0, TEXT_SECONDARY.into()))
                .child("返回发现")
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    app_listener(view, |this, _, _, cx| {
                        this.page = AppPage::Home;
                        cx.notify();
                    }),
                ),
        )
        // Integrated compact playlist header (no heavy enclosing card)
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_5()
                .py_1()
                .child(
                    div()
                        .size(px(88.0))
                        .flex_none()
                        .rounded_xl()
                        .overflow_hidden()
                        .border_1()
                        .border_color(BORDER_CARD)
                        .shadow_sm()
                        .child(image_cache::render_remote_cover(
                            cover_url.as_deref(),
                            88.0,
                            88.0,
                            12.0,
                            cx,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .px_2()
                                        .py_0p5()
                                        .rounded_md()
                                        .bg(theme::accent_red_muted())
                                        .border_1()
                                        .border_color(theme::accent_red_border())
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(ACCENT_RED)
                                        .child("歌单"),
                                )
                                .child(
                                    div()
                                        .text_xl()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(TEXT_PRIMARY)
                                        .truncate()
                                        .child(title),
                                ),
                        )
                        .children((!display_subtitle.is_empty()).then(|| {
                            div()
                                .text_xs()
                                .text_color(TEXT_SECONDARY)
                                .truncate()
                                .child(display_subtitle)
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .pt_1()
                                .child(
                                    div()
                                        .id("playlist-play-all-btn")
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .px_4()
                                        .py_1p5()
                                        .rounded_full()
                                        .bg(ACCENT_RED)
                                        .text_color(TEXT_WHITE)
                                        .cursor_pointer()
                                        .hover(|s| s.opacity(0.90))
                                        .transition(press_transition())
                                        .active(|s| s.scale(theme::PRESS_STANDARD))
                                        .child(themed_icon(icon!(play), 13.0, TEXT_WHITE.into()))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .child("播放全部"),
                                        )
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            app_listener(view, move |this, _, _, cx| {
                                                if let Some(first) = first_track.clone() {
                                                    this.play_online_playlist_all(
                                                        all_route.clone(),
                                                        all_title.clone(),
                                                        tracks.clone(),
                                                        first,
                                                        cx,
                                                    );
                                                }
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("playlist-download-all-btn")
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .px_3p5()
                                        .py_1p5()
                                        .rounded_full()
                                        .bg(theme::BG_CARD)
                                        .border_1()
                                        .border_color(BORDER_CARD)
                                        .text_color(TEXT_PRIMARY)
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::bg_hover()))
                                        .transition(press_transition())
                                        .child(themed_icon(
                                            icon!(download),
                                            13.0,
                                            TEXT_SECONDARY.into(),
                                        ))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child("下载全部"),
                                        )
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            app_listener(view, |this, _, _, cx| {
                                                this.status = "已加入离线下载队列".into();
                                                cx.notify();
                                            }),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(TEXT_TERTIARY)
                                        .child(if loading {
                                            "正在加载歌曲列表…".to_string()
                                        } else {
                                            format!("共 {track_count} 首歌曲")
                                        }),
                                ),
                        ),
                ),
        )
        .child(div().flex_1().min_h(px(0.0)).overflow_hidden().child(
            if loading && data.tracks.is_empty() {
                div()
                    .size_full()
                    .rounded_2xl()
                    .bg(theme::BG_CARD)
                    .border_1()
                    .border_color(BORDER_CARD)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(themed_icon(icon!(loader_circle), 28.0, ACCENT_RED.into()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(TEXT_SECONDARY)
                            .child("正在从网易云加载歌曲列表…"),
                    )
                    .into_any_element()
            } else {
                render_songs_table(data, &route, view, app)
            },
        ))
        .into_any_element()
}

fn render_songs_table(
    data: &OnlinePlaylistViewData,
    route: &PluginRoute,
    view: &WeakEntity<MusicApp>,
    app: &MusicApp,
) -> gpui::AnyElement {
    let count = data.tracks.len();
    if count == 0 {
        return div()
            .size_full()
            .rounded_2xl()
            .bg(theme::BG_CARD)
            .border_1()
            .border_color(BORDER_CARD)
            .py_12()
            .flex()
            .items_center()
            .justify_center()
            .text_sm()
            .text_color(TEXT_TERTIARY)
            .child(if data.loading {
                "正在加载歌曲列表…"
            } else {
                "此歌单暂无可播放歌曲"
            })
            .into_any_element();
    }

    let tracks: Arc<[RemoteTrack]> = data.tracks.clone().into();
    let track_route = route.clone();
    let playlist_title = data.title.clone();
    let view_clone = view.clone();
    let selected_queue = app.online_playlist_queue.as_ref().filter(|queue| {
        queue.route.plugin_id == route.plugin_id
            && queue.route.provider_id == route.provider_id
            && queue.route.account_id == route.account_id
            && queue.playlist_title == data.title
            && queue.tracks.len() == data.tracks.len()
    });
    let preferred_index = selected_queue.map_or(0, |queue| queue.current_index);
    let current_index = app
        .current_online_source_for_route(route)
        .and_then(|source| {
            tracks
                .iter()
                .enumerate()
                .filter(|(_, track)| track.source == *source)
                .min_by_key(|(index, _)| index.abs_diff(preferred_index))
                .map(|(index, _)| index)
        });
    let liked_source_ids: std::collections::HashSet<String> = app
        .config
        .online_liked_sources
        .iter()
        .filter(|key| {
            key.plugin_id == route.plugin_id
                && key.provider_id == route.provider_id
                && key.account_id == route.account_id
        })
        .map(|key| key.source_id.clone())
        .collect();
    let buffering_index = selected_queue.and_then(|queue| {
        queue.tracks.get(queue.current_index).and_then(|selected| {
            (app.online_track_buffering.as_deref() == Some(selected.source.source_id.as_str()))
                .then_some(queue.current_index)
        })
    });

    div()
        .size_full()
        .flex()
        .flex_col()
        .rounded_2xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(BORDER_CARD)
        .overflow_hidden()
        .child(
            // Table Header
            div()
                .w_full()
                .h(px(40.0))
                .flex_none()
                .flex()
                .items_center()
                .px_4()
                .bg(theme::BG_CANVAS)
                .border_b_1()
                .border_color(BORDER_CARD)
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_TERTIARY)
                .child(div().w(px(48.0)).child("#"))
                .child(div().flex_1().min_w(px(160.0)).child("标题"))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(110.0))
                        .max_w(px(260.0))
                        .child("专辑"),
                )
                .child(div().w(px(60.0)).text_right().child("时长"))
                .child(div().w(px(34.0)).child("喜欢")),
        )
        .child(
            div().flex_1().min_h(px(0.0)).overflow_hidden().child(
                uniform_list(
                    "online-playlist-songs-vlist",
                    count,
                    move |range: Range<usize>, _window, cx| {
                        let mut items = Vec::with_capacity(range.end - range.start);
                        for idx in range {
                            if let Some(track) = tracks.get(idx) {
                                let is_buffering = buffering_index == Some(idx);
                                let is_current = current_index == Some(idx);
                                let is_liked = liked_source_ids.contains(&track.source.source_id);

                                items.push(render_online_song_row(
                                    idx + 1,
                                    track,
                                    &track_route,
                                    &playlist_title,
                                    tracks.clone(),
                                    idx,
                                    &view_clone,
                                    is_buffering,
                                    is_current,
                                    is_liked,
                                    cx,
                                ));
                            }
                        }
                        items
                    },
                )
                .track_scroll(app.online_playlist_scroll.clone())
                .size_full(),
            ),
        )
        .into_any_element()
}

fn render_online_song_row(
    index: usize,
    track: &RemoteTrack,
    route: &PluginRoute,
    playlist_title: &str,
    playlist_tracks: Arc<[RemoteTrack]>,
    track_index: usize,
    view: &WeakEntity<MusicApp>,
    is_buffering: bool,
    is_current: bool,
    is_liked: bool,
    cx: &mut App,
) -> gpui::AnyElement {
    let track_route = route.clone();
    let song_playlist_title = playlist_title.to_string();
    let song_playlist_tracks = playlist_tracks;
    let cover_url = track.cover_url.as_deref();
    let title = track.title.clone();
    let artists = if track.artists.is_empty() {
        "未知艺术家".to_string()
    } else {
        track.artists.join("/")
    };
    let album = if track.album.is_empty() {
        "未知专辑".to_string()
    } else {
        track.album.clone()
    };
    let duration = format_time(track.duration_ms.unwrap_or(0));
    let num_str = format!("{:02}", index);

    let menu_route = route.clone();
    let menu_track = track.clone();
    let menu_title = track.title.clone();
    let heart_route = route.clone();
    let heart_track = track.clone();
    let heart_view = view.clone();

    let row_bg = if is_current {
        theme::accent_red_muted()
    } else {
        theme::transparent()
    };

    div()
        .w_full()
        .h(px(56.0))
        .flex()
        .items_center()
        .px_4()
        .bg(row_bg)
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(press_transition())
        .cursor_pointer()
        .border_b_1()
        .border_color(theme::BORDER_HAIRLINE)
        // Number / Playing Indicator / Buffering Spinner
        .child(
            div()
                .w(px(48.0))
                .flex()
                .items_center()
                .child(if is_buffering {
                    themed_icon(icon!(loader_circle), 14.0, ACCENT_RED.into()).into_any_element()
                } else if is_current {
                    themed_icon(icon!(play), 13.0, ACCENT_RED.into()).into_any_element()
                } else {
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(if is_current {
                            ACCENT_RED
                        } else {
                            TEXT_TERTIARY
                        })
                        .child(num_str)
                        .into_any_element()
                }),
        )
        // Title + Cover + Artist
        .child(
            div()
                .flex_1()
                .min_w(px(200.0))
                .flex()
                .items_center()
                .gap_3()
                .child(image_cache::render_remote_cover_with_sink(
                    cover_url, 40.0, 40.0, 6.0, view, cx,
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .min_w(px(0.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(if is_current {
                                            ACCENT_RED
                                        } else {
                                            TEXT_PRIMARY
                                        })
                                        .truncate()
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .px(px(3.5))
                                        .py(px(0.5))
                                        .rounded(px(theme::RADIUS_SM))
                                        .bg(theme::accent_red_soft())
                                        .border_1()
                                        .border_color(theme::accent_red_soft_border())
                                        .text_color(ACCENT_RED)
                                        .text_size(px(theme::FONT_CAPTION_MIN))
                                        .line_height(px(11.0))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child("SQ"),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_SECONDARY)
                                .truncate()
                                .child(artists),
                        ),
                ),
        )
        // Album
        .child(
            div()
                .flex_1()
                .min_w(px(110.0))
                .max_w(px(260.0))
                .text_xs()
                .text_color(TEXT_SECONDARY)
                .truncate()
                .child(album),
        )
        // Duration
        .child(
            div()
                .w(px(60.0))
                .text_right()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child(duration),
        )
        .child(
            div()
                .id(SharedString::from(format!("online-playlist-liked-{index}")))
                .w(px(34.0))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|style| style.bg(theme::bg_hover()))
                .child(themed_icon(
                    icon!(heart),
                    15.0,
                    if is_liked {
                        ACCENT_RED.into()
                    } else {
                        TEXT_TERTIARY.into()
                    },
                ))
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    let _ = heart_view.update(cx, |app, cx| {
                        app.toggle_online_track_liked(heart_route.clone(), heart_track.clone(), cx);
                    });
                }),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            app_listener(view, move |this, _, _, cx| {
                // A newer selection supersedes/cancels the previous online prepare task in
                // MusicApp. Never turn buffering into a global input lock for the playlist.
                this.play_online_playlist_track_shared(
                    track_route.clone(),
                    song_playlist_title.clone(),
                    song_playlist_tracks.clone(),
                    track_index,
                    cx,
                );
            }),
        )
        .on_mouse_down(
            gpui::MouseButton::Right,
            app_listener(view, move |this, event: &gpui::MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.open_context_menu(
                    event.position,
                    menu_title.clone(),
                    crate::ui::components::modal::ContextMenuTarget::OnlineTrack {
                        route: menu_route.clone(),
                        track: menu_track.clone(),
                    },
                    cx,
                );
            }),
        )
        .into_any_element()
}
