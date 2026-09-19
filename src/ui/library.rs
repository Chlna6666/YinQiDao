use std::{collections::HashMap, ops::Range, sync::Arc};

use gpui::{
    EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, WeakEntity,
    Window, div, hsla, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, uniform_list,
};
use lucide_gpui::icon;

use crate::{
    library::LocalPlaylistSummary,
    model::{LibraryTab, Track, TrackId},
    plugin::abi::PluginRoute,
    ui::image_cache,
};

#[path = "plugin/context_menu.rs"]
mod plugin_context_menu;
#[path = "plugin/provider_playlists.rs"]
mod provider_playlists;

use super::{
    shell::{MusicApp, app_listener},
    theme::{
        self, ACCENT_RED, BORDER_CARD, BORDER_HAIRLINE, TEXT_PRIMARY, TEXT_SECONDARY,
        TEXT_TERTIARY, elegant_gradient_for, format_time, press_transition, themed_icon,
        waveform_animation,
    },
};

pub(super) fn render(
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let query = normalized_query(&app.search);

    let content = match app.library_tab {
        LibraryTab::Songs => songs_view(app, &query, view).into_any_element(),
        LibraryTab::Recent => recent_view(app, &query, view).into_any_element(),
        LibraryTab::Albums => albums_view(&app.tracks, &query, app, view).into_any_element(),
        LibraryTab::Artists => artists_view(&app.tracks, &query, app, view).into_any_element(),
        LibraryTab::Playlists => queue_view(app, view).into_any_element(),
    };

    let virtualized_tab = matches!(
        app.library_tab,
        LibraryTab::Songs | LibraryTab::Recent | LibraryTab::Playlists
    );
    if virtualized_tab {
        div()
            .id(match app.library_tab {
                LibraryTab::Songs => "library-songs-container",
                LibraryTab::Recent => "library-recent-container",
                _ => "library-queue-container",
            })
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .p_8()
            .gap_4()
            .overflow_hidden()
            .child(header(app, view))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_hidden()
                    .child(content),
            )
            .children(plugin_context_menu::render(view))
            .into_any_element()
    } else {
        div()
            .id("library-scroll")
            .size_full()
            .relative()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p_8()
            .gap_6()
            .child(header(app, view))
            .child(content)
            .children(plugin_context_menu::render(view))
            .into_any_element()
    }
}

fn header(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let tab_title = match app.library_tab {
        LibraryTab::Songs => "本地音乐",
        LibraryTab::Recent => "最近播放",
        LibraryTab::Albums => "专辑资料库",
        LibraryTab::Artists => "艺术家",
        LibraryTab::Playlists => "播放列表与队列",
    };

    let subtitle = match app.library_tab {
        LibraryTab::Recent => format!("收录最近播放的 {} 首曲目", app.recent_plays.len()),
        _ => format!("共收录 {} 首曲目 · 状态: {}", app.tracks.len(), app.status),
    };

    div()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_2xl()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(TEXT_PRIMARY)
                                .child(tab_title),
                        )
                        .child(div().text_sm().text_color(TEXT_SECONDARY).child(subtitle)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .id("library-search-input")
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3p5()
                                .py_2()
                                .w(px(260.0))
                                .rounded_full()
                                .bg(rgb(0xff_ff_ff))
                                .border_1()
                                .border_color(if app.search_active {
                                    ACCENT_RED
                                } else {
                                    BORDER_CARD
                                })
                                .cursor_pointer()
                                .child(themed_icon(
                                    icon!(search),
                                    15.0,
                                    if app.search_active {
                                        ACCENT_RED.into()
                                    } else {
                                        hsla(220.0, 0.08, 0.50, 1.0)
                                    },
                                ))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if app.search.is_empty() {
                                            TEXT_TERTIARY
                                        } else {
                                            TEXT_PRIMARY
                                        })
                                        .truncate()
                                        .child(if app.search.is_empty() {
                                            "过滤歌曲、艺术家或专辑...".to_string()
                                        } else {
                                            app.search.clone()
                                        }),
                                )
                                .child_if(!app.search.is_empty(), || {
                                    div()
                                        .id("library-search-clear")
                                        .size(px(18.0))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(theme::bg_hover())
                                        .child(themed_icon(
                                            icon!(x),
                                            10.0,
                                            TEXT_SECONDARY.into(),
                                        ))
                                        .on_mouse_down(gpui::MouseButton::Left, app_listener(view, |this, _, _, cx| {
                                            this.search.clear();
                                            this.search_active = false;
                                            cx.notify();
                                        }))
                                })
                                .on_mouse_down(gpui::MouseButton::Left, app_listener(view, |this, _, _, cx| {
                                    this.search_active = true;
                                    cx.notify();
                                }))
                        )
                        .children(if app.library_tab == LibraryTab::Songs {
                            Some(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("library-add-dir-btn")
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_4()
                                            .py_2()
                                            .rounded_full()
                                            .cursor_pointer()
                                            .bg(theme::accent_red_muted())
                                            .text_color(ACCENT_RED)
                                            .hover(|s| s.bg(theme::accent_red_active()))
                                            .transition(press_transition())
                                            .active(|s| s.scale(0.96))
                                            .child(themed_icon(
                                                icon!(folder_plus),
                                                15.0,
                                                ACCENT_RED.into(),
                                            ))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .child("导入目录"),
                                            )
                                            .on_mouse_down(gpui::MouseButton::Left, app_listener(view, |this, _, _, cx| {
                                                this.choose_folder(cx)
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("library-rescan-btn")
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_4()
                                            .py_2()
                                            .rounded_full()
                                            .cursor_pointer()
                                            .bg(theme::BG_CARD)
                                            .border_1()
                                            .border_color(BORDER_CARD)
                                            .text_color(TEXT_PRIMARY)
                                            .hover(|s| s.bg(theme::bg_hover()))
                                            .transition(press_transition())
                                            .active(|s| s.scale(0.96))
                                            .child(themed_icon(
                                                icon!(refresh_cw),
                                                14.0,
                                                TEXT_SECONDARY.into(),
                                            ))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(gpui::FontWeight::MEDIUM)
                                                    .child("重新扫描"),
                                            )
                                            .on_mouse_down(gpui::MouseButton::Left, app_listener(view, |this, _, _, cx| {
                                                this.rescan_library(cx)
                                            })),
                                    )
                                    .into_any_element(),
                            )
                        } else if app.library_tab == LibraryTab::Recent {
                            Some(
                                div()
                                    .id("clear-recent-plays-btn")
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_3p5()
                                    .py_2()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(theme::BG_CARD)
                                    .border_1()
                                    .border_color(BORDER_CARD)
                                    .text_color(TEXT_SECONDARY)
                                    .hover(|s| s.bg(theme::bg_hover()).text_color(TEXT_PRIMARY))
                                    .transition(press_transition())
                                    .active(|s| s.scale(0.96))
                                    .child(themed_icon(icon!(trash_2), 14.0, TEXT_SECONDARY.into()))
                                    .child(div().text_xs().child("清空记录"))
                                    .on_mouse_down(gpui::MouseButton::Left, app_listener(view, |this, _, _, cx| {
                                        this.recent_plays.clear();
                                        cx.notify();
                                    }))
                                    .into_any_element(),
                            )
                        } else {
                            None
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .rounded_full()
                .bg(rgb(0xec_ee_f2))
                .child(segmented_tab_item(
                    "本地歌曲",
                    icon!(music),
                    app.library_tab == LibraryTab::Songs,
                    app_listener(view, |this, _, _, cx| {
                        this.show_library_tab(LibraryTab::Songs, cx)
                    }),
                ))
                .child(segmented_tab_item(
                    "最近播放",
                    icon!(history),
                    app.library_tab == LibraryTab::Recent,
                    app_listener(view, |this, _, _, cx| {
                        this.show_library_tab(LibraryTab::Recent, cx)
                    }),
                ))
                .child(segmented_tab_item(
                    "专辑",
                    icon!(disc_3),
                    app.library_tab == LibraryTab::Albums,
                    app_listener(view, |this, _, _, cx| {
                        this.show_library_tab(LibraryTab::Albums, cx)
                    }),
                ))
                .child(segmented_tab_item(
                    "艺术家",
                    icon!(users_round),
                    app.library_tab == LibraryTab::Artists,
                    app_listener(view, |this, _, _, cx| {
                        this.show_library_tab(LibraryTab::Artists, cx)
                    }),
                ))
                .child(segmented_tab_item(
                    "播放列表/队列",
                    icon!(list_music),
                    app.library_tab == LibraryTab::Playlists,
                    app_listener(view, |this, _, _, cx| {
                        this.show_library_tab(LibraryTab::Playlists, cx)
                    }),
                )),
        )
}

fn segmented_tab_item<F>(
    label: &'static str,
    icon: &'static str,
    active: bool,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .id(SharedString::from(format!("seg-{label}")))
        .flex()
        .items_center()
        .gap_2()
        .px_4()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .bg(if active {
            rgb(0xff_ff_ff).into()
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .hover(|s| s.opacity(if active { 1.0 } else { 0.85 }))
        .transition(press_transition())
        .active(|s| s.scale(0.96))
        .child(themed_icon(
            icon,
            14.0,
            if active {
                ACCENT_RED.into()
            } else {
                hsla(220.0, 0.08, 0.50, 1.0)
            },
        ))
        .child(
            div()
                .text_xs()
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(if active { TEXT_PRIMARY } else { TEXT_SECONDARY })
                .child(label),
        )
        .on_mouse_down(gpui::MouseButton::Left, on_click)
}

fn song_table_header() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(BORDER_HAIRLINE)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(TEXT_TERTIARY)
        .child(div().w(px(36.0)).child("#"))
        .child(div().flex_1().min_w(px(0.0)).child("标题"))
        .child(div().w(px(200.0)).child("艺人"))
        .child(div().w(px(220.0)).child("专辑"))
        .child(div().w(px(80.0)).child("时长"))
        .child(div().w(px(60.0)).child("操作"))
}

fn online_table_header() -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .px_4()
        .py_2()
        .border_b_1()
        .border_color(BORDER_HAIRLINE)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(TEXT_TERTIARY)
        .child(div().w(px(36.0)).child("#"))
        .child(div().flex_1().min_w(px(0.0)).child("单曲标题"))
        .child(div().w(px(200.0)).child("歌手"))
        .child(div().w(px(220.0)).child("专辑"))
        .child(div().w(px(80.0)).child("时长"))
        .child(div().w(px(60.0)).child("操作"))
}

fn online_search_row(
    index: usize,
    track: &crate::plugin::abi::RemoteTrack,
    route: PluginRoute,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let remote_track = track.clone();
    let play_track = track.clone();
    let route_clone = route.clone();
    let cover_url = track.cover_url.as_deref();
    let title = track.title.clone();
    let artists = track.artists.join("/");
    let album = if track.album.is_empty() {
        "—".to_string()
    } else {
        track.album.clone()
    };
    let duration = format_time(track.duration_ms.unwrap_or(0));

    div()
        .w_full()
        .flex()
        .items_center()
        .px_4()
        .py_2()
        .rounded_lg()
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(press_transition())
        .cursor_pointer()
        .child(
            div()
                .w(px(36.0))
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child(format!("{index:02}")),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap_3()
                .child(image_cache::render_remote_cover(
                    cover_url, 36.0, 36.0, 6.0,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(TEXT_PRIMARY)
                                .truncate()
                                .child(title),
                        )
                        .child(
                            div()
                                .px_1()
                                .py_0p5()
                                .rounded(px(3.0))
                                .bg(hsla(348.0, 0.90, 0.95, 1.0))
                                .border_1()
                                .border_color(hsla(348.0, 0.90, 0.60, 0.3))
                                .text_color(ACCENT_RED)
                                .text_xs()
                                .child("SQ"),
                        ),
                ),
        )
        .child(
            div()
                .w(px(200.0))
                .text_xs()
                .text_color(TEXT_SECONDARY)
                .truncate()
                .child(artists),
        )
        .child(
            div()
                .w(px(220.0))
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .truncate()
                .child(album),
        )
        .child(
            div()
                .w(px(80.0))
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child(duration),
        )
        .child(
            div()
                .w(px(60.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .px_2p5()
                        .py_1()
                        .rounded_full()
                        .bg(theme::accent_red_muted())
                        .text_color(ACCENT_RED)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::accent_red_active()))
                        .transition(press_transition())
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("播放"),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            app_listener(view, move |this, _, _, cx| {
                                this.play_online_remote_track(route.clone(), remote_track.clone(), cx);
                            }),
                        ),
                ),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            app_listener(view, move |this, _, _, cx| {
                this.play_online_remote_track(route_clone.clone(), play_track.clone(), cx);
            }),
        )
}

fn songs_view(
    app: &MusicApp,
    query: &str,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let matching_indices = matching_track_indices(&app.tracks, query);
    let count = matching_indices
        .as_ref()
        .map_or(app.tracks.len(), |indices| indices.len());

    let has_search = !query.is_empty();
    let has_online_results = !app.online_search_results.is_empty();
    let online_loading = app.online_search_loading;

    if count == 0 && !has_search {
        return empty_filter_state().into_any_element();
    }

    if count == 0 && !has_online_results && !online_loading {
        return div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .size(px(48.0))
                    .rounded_full()
                    .bg(theme::BG_CANVAS)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(themed_icon(icon!(search), 22.0, TEXT_TERTIARY.into())),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(TEXT_PRIMARY)
                    .child("本地与网易云均未找到匹配歌曲"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(TEXT_TERTIARY)
                    .child("尝试更换关键词重新搜索"),
            )
            .into_any_element();
    }

    let matching_indices_for_rows = matching_indices.clone();
    let view_clone = view.clone();

    let route = app
        .online_search_route
        .clone()
        .or_else(|| app.online_route.clone())
        .unwrap_or_else(|| PluginRoute {
            plugin_id: "netease".to_string(),
            provider_id: "netease-music".to_string(),
            account_id: String::new(),
            priority: 0,
            is_default: true,
        });

    if has_search {
        let mut search_view = div()
            .id("library-search-results")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_6();

        // 1. Local results section (if any)
        if count > 0 {
            let mut local_list = div().flex().flex_col().gap_1();
            let limit = count.min(15);
            for idx in 0..limit {
                let track_index = matching_indices_for_rows
                    .as_ref()
                    .map_or(idx, |indices| indices[idx]);
                if let Some(track) = app.tracks.get(track_index) {
                    let track_id = track.id;
                    let is_current = app
                        .snapshot
                        .current_track
                        .as_ref()
                        .is_some_and(|t| t.id == track_id);
                    let is_playing = is_current
                        && app.snapshot.state == crate::model::PlaybackState::Playing;
                    let artwork = app.artworks.get(&track_id).cloned();
                    local_list = local_list.child(song_table_row(
                        idx + 1,
                        track,
                        is_current,
                        is_playing,
                        artwork,
                        view,
                    ));
                }
            }

            search_view = search_view.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(TEXT_PRIMARY)
                            .child(format!("本地曲库匹配结果 ({count})")),
                    )
                    .child(song_table_header())
                    .child(local_list),
            );
        }

        // 2. Online search results section
        let mut online_list = div().flex().flex_col().gap_1();

        if online_loading && !has_online_results {
            online_list = online_list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_4()
                    .rounded_xl()
                    .bg(theme::BG_CANVAS)
                    .border_1()
                    .border_color(BORDER_CARD)
                    .child(themed_icon(icon!(refresh_cw), 16.0, ACCENT_RED.into()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(TEXT_SECONDARY)
                            .child("正在网易云音乐曲库中全力搜索…"),
                    ),
            );
        } else {
            for (idx, remote_track) in app.online_search_results.iter().enumerate() {
                online_list = online_list.child(online_search_row(
                    idx + 1,
                    remote_track,
                    route.clone(),
                    view,
                ));
            }
        }

        search_view = search_view.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(TEXT_PRIMARY)
                                        .child("网易云音乐 在线曲库"),
                                )
                                .child(
                                    div()
                                        .px_2()
                                        .py_0p5()
                                        .rounded_full()
                                        .bg(theme::accent_red_muted())
                                        .text_color(ACCENT_RED)
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(format!("{} 首", app.online_search_results.len())),
                                ),
                        )
                        .child_if(online_loading, || {
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(TEXT_TERTIARY)
                                .child(themed_icon(icon!(refresh_cw), 12.0, TEXT_TERTIARY.into()))
                                .child("正在更新搜索结果…")
                        }),
                )
                .child(online_table_header())
                .child(online_list),
        );

        return search_view.into_any_element();
    }

    div()
        .size_full()
        .flex()
        .flex_col()
        .gap_2()
        .overflow_hidden()
        .child(song_table_header())
        .child(
            div().flex_1().min_h(px(0.0)).overflow_hidden().child(
                uniform_list(
                    "library-song-table-vlist",
                    count,
                    move |range: Range<usize>, _window, cx| {
                        view_clone
                            .update(cx, |this, _cx| {
                                let mut items = Vec::with_capacity(range.end - range.start);
                                for idx in range {
                                    let track_index = matching_indices_for_rows
                                        .as_ref()
                                        .map_or(idx, |indices| indices[idx]);
                                    if let Some(track) = this.tracks.get(track_index) {
                                        let track_id = track.id;
                                        let is_current = this
                                            .snapshot
                                            .current_track
                                            .as_ref()
                                            .is_some_and(|t| t.id == track_id);
                                        let is_playing = is_current
                                            && this.snapshot.state
                                                == crate::model::PlaybackState::Playing;
                                        let artwork = this.artworks.get(&track_id).cloned();
                                        items.push(song_table_row(
                                            idx + 1,
                                            track,
                                            is_current,
                                            is_playing,
                                            artwork,
                                            &view_clone,
                                        ));
                                    }
                                }
                                items
                            })
                            .unwrap_or_default()
                    },
                )
                .track_scroll(app.library_scroll_handle.clone())
                .size_full(),
            ),
        )
        .into_any_element()
}

fn recent_view(
    app: &MusicApp,
    query: &str,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let mut seen_keys = std::collections::HashSet::new();
    let mut recent_tracks: Vec<Track> = Vec::new();
    for id in &app.recent_plays {
        let track = app
            .tracks
            .iter()
            .find(|t| t.id == *id)
            .cloned()
            .or_else(|| app.online_track_cache.get(id).cloned());
        if let Some(track) = track {
            let key = (
                track.title.trim().to_lowercase(),
                track.artist.trim().to_lowercase(),
            );
            if seen_keys.insert(key) {
                recent_tracks.push(track);
            }
        }
    }

    let filtered_tracks: Vec<Track> = if query.is_empty() {
        recent_tracks
    } else {
        recent_tracks
            .into_iter()
            .filter(|track| track_matches_query(track, query))
            .collect()
    };

    let count = filtered_tracks.len();

    if count == 0 {
        return div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .size(px(48.0))
                    .rounded_full()
                    .bg(theme::BG_CANVAS)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(themed_icon(icon!(history), 22.0, TEXT_TERTIARY.into())),
            )
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(TEXT_PRIMARY)
                    .child(if query.is_empty() {
                        "暂无最近播放记录"
                    } else {
                        "未找到匹配的播放历史"
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(TEXT_TERTIARY)
                    .child(if query.is_empty() {
                        "播放歌曲后将自动沉淀足迹，方便随时重温"
                    } else {
                        "尝试更换关键词重新搜索"
                    }),
            )
            .into_any_element();
    }

    let tracks_arc = std::sync::Arc::new(filtered_tracks);
    let view_clone = view.clone();

    div()
        .size_full()
        .flex()
        .flex_col()
        .gap_2()
        .overflow_hidden()
        .child(song_table_header())
        .child(
            div().flex_1().min_h(px(0.0)).overflow_hidden().child(
                uniform_list(
                    "library-recent-table-vlist",
                    count,
                    move |range: Range<usize>, _window, cx| {
                        let tracks = tracks_arc.clone();
                        view_clone
                            .update(cx, |this, _cx| {
                                let mut items = Vec::with_capacity(range.end - range.start);
                                for idx in range {
                                    if let Some(track) = tracks.get(idx) {
                                        let track_id = track.id;
                                        let is_current = this
                                            .snapshot
                                            .current_track
                                            .as_ref()
                                            .is_some_and(|t| t.id == track_id);
                                        let is_playing = is_current
                                            && this.snapshot.state
                                                == crate::model::PlaybackState::Playing;
                                        let artwork = this.artworks.get(&track_id).cloned();
                                        items.push(song_table_row(
                                            idx + 1,
                                            track,
                                            is_current,
                                            is_playing,
                                            artwork,
                                            &view_clone,
                                        ));
                                    }
                                }
                                items
                            })
                            .unwrap_or_default()
                    },
                )
                .size_full(),
            ),
        )
        .into_any_element()
}

fn song_table_row(
    index: usize,
    track: &Track,
    is_current: bool,
    is_playing: bool,
    artwork: Option<std::sync::Arc<[u8]>>,
    view: &gpui::WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let track_id = track.id;

    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(38.0))
            .rounded_md()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track_id);
        div()
            .size(px(38.0))
            .rounded_md()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(c1, 0.0),
                linear_color_stop(c2, 1.0),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(
                icon!(music),
                16.0,
                hsla(0.0, 0.0, 1.0, 0.85),
            ))
            .into_any_element()
    };

    let view_add = view.clone();
    let view_play = view.clone();
    let view_context = view.clone();
    let track_context = track.clone();

    div()
        .id(SharedString::from(format!("library-track-{track_id}")))
        .w_full()
        .h(px(52.0))
        .flex()
        .items_center()
        .px_4()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .bg(if is_current {
            theme::accent_red_muted()
        } else {
            hsla(0.0, 0.0, 0.0, 0.0)
        })
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(press_transition())
        .active(|s| s.scale(0.995))
        .child(
            div()
                .w(px(36.0))
                .flex()
                .items_center()
                .child_if(is_playing, || waveform_animation(true))
                .child_if(!is_playing, || {
                    div()
                        .text_xs()
                        .text_color(if is_current {
                            ACCENT_RED
                        } else {
                            TEXT_TERTIARY
                        })
                        .child(format!("{index:02}"))
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap_3()
                .child(cover)
                .child(
                    div()
                        .text_sm()
                        .font_weight(if is_current {
                            gpui::FontWeight::BOLD
                        } else {
                            gpui::FontWeight::MEDIUM
                        })
                        .text_color(if is_current { ACCENT_RED } else { TEXT_PRIMARY })
                        .truncate()
                        .child(track.title.clone()),
                ),
        )
        .child(
            div()
                .w(px(200.0))
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .truncate()
                .child(track.artist.clone()),
        )
        .child(
            div()
                .w(px(220.0))
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .truncate()
                .child(track.album.clone()),
        )
        .child(
            div()
                .w(px(80.0))
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child(format_time(track.duration_ms)),
        )
        .child(
            div().w(px(60.0)).flex().items_center().child(
                div()
                    .id(SharedString::from(format!("lib-add-q-{track_id}")))
                    .size(px(26.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .hover(|s| s.bg(theme::bg_active()))
                    .child(themed_icon(
                        icon!(plus),
                        14.0,
                        hsla(220.0, 0.08, 0.50, 1.0),
                    ))
                    .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                        let _ = view_add.update(cx, |this, cx| {
                            this.add_to_queue(track_id, cx);
                        });
                    }),
            ),
        )
        .on_mouse_down(gpui::MouseButton::Right, move |event, window, cx| {
            cx.stop_propagation();
            let position = event.position;
            let viewport = window.viewport_size();
            let track = track_context.clone();
            let _ = view_context.update(cx, |this, app_cx| {
                plugin_context_menu::open_track(this, &track, position, viewport, app_cx);
            });
        })
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
            let _ = view_play.update(cx, |this, cx| this.play_track(track_id, cx));
        })
        .into_any_element()
}

fn albums_view(
    tracks: &[Track],
    query: &str,
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let mut albums: Vec<(String, String, TrackId, usize)> = Vec::new();
    let mut album_indices: HashMap<String, usize> = HashMap::new();
    for track in tracks {
        if !track_matches_query(track, query) {
            continue;
        }
        let display_name = if track.album.trim().is_empty() {
            "未知专辑"
        } else {
            track.album.as_str()
        };
        if let Some(&index) = album_indices.get(display_name) {
            albums[index].3 += 1;
        } else {
            album_indices.insert(display_name.to_owned(), albums.len());
            albums.push((display_name.to_owned(), track.artist.clone(), track.id, 1));
        }
    }

    if albums.is_empty() {
        return empty_filter_state().into_any_element();
    }

    let mut grid = div().flex().flex_wrap().gap_6();

    for (name, artist, track_id, count) in albums {
        let artwork = app.artworks.get(&track_id).cloned();
        grid = grid.child(album_poster_card(
            name, artist, track_id, count, artwork, view,
        ));
    }

    grid.into_any_element()
}

fn album_poster_card(
    name: String,
    artist: String,
    track_id: TrackId,
    count: usize,
    artwork: Option<std::sync::Arc<[u8]>>,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(170.0))
            .rounded_xl()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track_id);
        div()
            .size(px(170.0))
            .rounded_xl()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(c1, 0.0),
                linear_color_stop(c2, 1.0),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(
                icon!(disc_3),
                48.0,
                hsla(0.0, 0.0, 1.0, 0.80),
            ))
            .into_any_element()
    };

    div()
        .id(SharedString::from(format!("lib-album-card-{track_id}")))
        .w(px(170.0))
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .cursor_pointer()
        .hover(|s| s.scale(1.02))
        .transition(press_transition())
        .active(|s| s.scale(0.97))
        .child(
            div()
                .size(px(170.0))
                .rounded_xl()
                .overflow_hidden()
                .relative()
                .child(cover)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_xl()
                        .bg(hsla(0.0, 0.0, 0.0, 0.25))
                        .opacity(0.0)
                        .hover(|s| s.opacity(1.0))
                        .transition(press_transition())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(44.0))
                                .rounded_full()
                                .bg(ACCENT_RED)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(themed_icon(
                                    icon!(play),
                                    22.0,
                                    hsla(0.0, 0.0, 1.0, 1.0),
                                )),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .truncate()
                        .child(format!("{artist} · {count} 首")),
                ),
        )
        .on_mouse_down(gpui::MouseButton::Left, app_listener(view, move |this, _, _, cx| {
            this.play_track(track_id, cx)
        }))
}

fn artists_view(
    tracks: &[Track],
    query: &str,
    _app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let mut artists: Vec<(String, TrackId, usize)> = Vec::new();
    let mut artist_indices: HashMap<String, usize> = HashMap::new();
    for track in tracks {
        if !track_matches_query(track, query) {
            continue;
        }
        let display_name = if track.artist.trim().is_empty() {
            "未知艺术家"
        } else {
            track.artist.as_str()
        };
        if let Some(&index) = artist_indices.get(display_name) {
            artists[index].2 += 1;
        } else {
            artist_indices.insert(display_name.to_owned(), artists.len());
            artists.push((display_name.to_owned(), track.id, 1));
        }
    }

    if artists.is_empty() {
        return empty_filter_state().into_any_element();
    }

    let mut grid = div().flex().flex_wrap().gap_8();

    for (name, track_id, count) in artists {
        grid = grid.child(artist_circle_card(name, track_id, count, view));
    }

    grid.into_any_element()
}

fn artist_circle_card(
    name: String,
    track_id: TrackId,
    count: usize,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let (c1, c2) = elegant_gradient_for(track_id);

    div()
        .id(SharedString::from(format!("lib-artist-card-{track_id}")))
        .w(px(140.0))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap_2p5()
        .cursor_pointer()
        .hover(|s| s.scale(1.03))
        .transition(press_transition())
        .active(|s| s.scale(0.97))
        .child(
            div()
                .size(px(130.0))
                .rounded_full()
                .p_1()
                .bg(rgb(0xff_ff_ff))
                .border_1()
                .border_color(BORDER_CARD)
                .child(
                    div()
                        .size_full()
                        .rounded_full()
                        .bg(linear_gradient(
                            140.0,
                            linear_color_stop(c1, 0.0),
                            linear_color_stop(c2, 1.0),
                        ))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(themed_icon(
                            icon!(users_round),
                            44.0,
                            hsla(0.0, 0.0, 1.0, 0.85),
                        )),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .child(format!("{count} 首作品")),
                ),
        )
        .on_mouse_down(gpui::MouseButton::Left, app_listener(view, move |this, _, _, cx| {
            this.play_track(track_id, cx)
        }))
}

fn queue_view(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let playlists = app
        .library
        .as_ref()
        .map(|library| library.playlist_summaries())
        .unwrap_or_else(|| Arc::<[LocalPlaylistSummary]>::from([]));
    let queue_ids = &app.config.queue;
    let queue_ids_for_rows = queue_ids.clone();
    let track_indices_for_rows = Arc::new(
        app.tracks
            .iter()
            .enumerate()
            .map(|(index, track)| (track.id, index))
            .collect::<HashMap<TrackId, usize>>(),
    );
    let view_for_rows = view.clone();

    let queue_body = if queue_ids.is_empty() {
        div()
            .flex_1()
            .min_h(px(0.0))
            .p_8()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .rounded_2xl()
            .bg(rgb(0xff_ff_ff))
            .border_1()
            .border_color(BORDER_CARD)
            .child(themed_icon(
                icon!(list_music),
                32.0,
                TEXT_TERTIARY.into(),
            ))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(TEXT_PRIMARY)
                    .child("待播队列为空"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(TEXT_SECONDARY)
                    .child("在任意歌曲右侧点击“+”，即可将其加入待播队列"),
            )
            .into_any_element()
    } else {
        div()
            .flex_1()
            .min_h(px(0.0))
            .overflow_hidden()
            .child(
                uniform_list(
                    "library-queue-vlist",
                    queue_ids.len(),
                    move |range: Range<usize>, _window, cx| {
                        view_for_rows
                            .update(cx, |this, _cx| {
                                let mut items = Vec::with_capacity(range.end - range.start);
                                for queue_index in range {
                                    let Some(&track_id) = queue_ids_for_rows.get(queue_index) else {
                                        continue;
                                    };
                                    let track = track_indices_for_rows
                                        .get(&track_id)
                                        .and_then(|&track_index| {
                                            this.tracks
                                                .get(track_index)
                                                .filter(|track| track.id == track_id)
                                        })
                                        .cloned()
                                        .or_else(|| this.online_track_cache.get(&track_id).cloned());
                                    let Some(track) = track else {
                                        continue;
                                    };
                                    items.push(
                                        div()
                                            .h(px(64.0))
                                            .child(queue_item_row(
                                                &track,
                                                this,
                                                &view_for_rows,
                                                queue_index,
                                            ))
                                            .into_any_element(),
                                    );
                                }
                                items
                            })
                            .unwrap_or_default()
                    },
                )
                .size_full(),
            )
            .into_any_element()
    };

    div()
        .size_full()
        .flex()
        .flex_col()
        .gap_4()
        .overflow_hidden()
        .child(local_playlists_section(playlists, view))
        .child(provider_playlists::render(view))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(format!("待播队列中共有 {} 首歌曲", queue_ids.len())),
                )
                .child_if(!queue_ids.is_empty(), || {
                    div()
                        .id("queue-clear-btn")
                        .px_3()
                        .py_1p5()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(rgb(0xff_ff_ff))
                        .border_1()
                        .border_color(BORDER_CARD)
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .hover(|s| s.text_color(ACCENT_RED))
                        .transition(press_transition())
                        .child("清空队列")
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            app_listener(view, |this, _, _, cx| this.clear_queue(cx)),
                        )
                }),
        )
        .child(queue_body)
        .into_any_element()
}

fn local_playlists_section(
    playlists: Arc<[LocalPlaylistSummary]>,
    view: &WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    if playlists.is_empty() {
        return div()
            .flex_none()
            .h(px(76.0))
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .rounded_xl()
            .bg(rgb(0xff_ff_ff))
            .border_1()
            .border_color(BORDER_CARD)
            .child(themed_icon(
                icon!(list_music),
                20.0,
                TEXT_TERTIARY.into(),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(TEXT_PRIMARY)
                            .child("本地播放列表"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(TEXT_TERTIARY)
                            .child("当前数据库中没有播放列表；待播队列不会被当作 PlaylistContext"),
                    ),
            )
            .into_any_element();
    }

    let row_height = 56.0;
    let visible_rows = playlists.len().min(3) as f32;
    let list_height = (visible_rows * row_height).max(row_height);
    let rows = playlists.clone();
    let view_rows = view.clone();

    div()
        .flex_none()
        .h(px(38.0 + list_height))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(format!("本地播放列表 · {} 个", playlists.len())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child("右键打开插件 PlaylistContext"),
                ),
        )
        .child(
            div()
                .h(px(list_height))
                .overflow_hidden()
                .child(
                    uniform_list(
                        "library-local-playlists-vlist",
                        playlists.len(),
                        move |range: Range<usize>, _window, _cx| {
                            let mut items = Vec::with_capacity(range.end - range.start);
                            for index in range {
                                if let Some(playlist) = rows.get(index) {
                                    items.push(local_playlist_row(playlist, &view_rows));
                                }
                            }
                            items
                        },
                    )
                    .size_full(),
                ),
        )
        .into_any_element()
}

fn local_playlist_row(
    playlist: &LocalPlaylistSummary,
    view: &WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let playlist = playlist.clone();
    let playlist_id = playlist.id;
    let view_context = view.clone();

    div()
        .id(SharedString::from(format!("local-playlist-{playlist_id}")))
        .h(px(52.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .rounded_xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .hover(|style| style.bg(theme::bg_hover()))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .min_w(px(0.0))
                .child(
                    div()
                        .size(px(32.0))
                        .rounded_lg()
                        .bg(theme::accent_red_muted())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(themed_icon(icon!(list_music), 15.0, ACCENT_RED.into())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(TEXT_PRIMARY)
                                .truncate()
                                .child(playlist.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_TERTIARY)
                                .child(format!("{} 首本地曲目", playlist.track_count)),
                        ),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("PlaylistContext"),
        )
        .on_mouse_down(gpui::MouseButton::Right, move |event, window, cx| {
            cx.stop_propagation();
            let position = event.position;
            let viewport = window.viewport_size();
            let playlist = playlist.clone();
            let _ = view_context.update(cx, |this, app_cx| {
                plugin_context_menu::open_playlist(
                    this,
                    &playlist,
                    position,
                    viewport,
                    app_cx,
                );
            });
        })
        .into_any_element()
}

fn queue_item_row(
    track: &Track,
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
    queue_index: usize,
) -> impl IntoElement {
    let track_id = track.id;
    let is_current = app
        .snapshot
        .current_track
        .as_ref()
        .is_some_and(|t| t.id == track_id);

    div()
        .id(SharedString::from(format!(
            "queue-row-{queue_index}-{track_id}"
        )))
        .w_full()
        .h(px(58.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        .py_2p5()
        .rounded_xl()
        .bg(if is_current {
            theme::accent_red_muted()
        } else {
            rgb(0xff_ff_ff).into()
        })
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(theme::press_transition())
        .cursor_pointer()
        .border_1()
        .border_color(BORDER_CARD)
        .on_mouse_down(
            gpui::MouseButton::Left,
            app_listener(view, move |this, event: &gpui::MouseDownEvent, _, cx| {
                if event.click_count >= 2 {
                    if track_id < 0 {
                        if let Some((route, remote)) =
                            this.online_remote_tracks.get(&track_id).cloned()
                        {
                            this.play_online_remote_track(route, remote, cx);
                        } else if let Some(t) = this.online_track_cache.get(&track_id).cloned() {
                            this.play_prepared_remote_track(t, cx);
                        }
                    } else {
                        this.play_track(track_id, cx);
                    }
                }
            }),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .flex_1()
                .min_w(px(0.0))
                .child(div().size(px(8.0)).rounded_full().bg(if is_current {
                    ACCENT_RED.into()
                } else {
                    hsla(220.0, 0.08, 0.70, 1.0)
                }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(if is_current {
                                    gpui::FontWeight::BOLD
                                } else {
                                    gpui::FontWeight::MEDIUM
                                })
                                .text_color(if is_current { ACCENT_RED } else { TEXT_PRIMARY })
                                .truncate()
                                .child(track.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_SECONDARY)
                                .truncate()
                                .child(format!("{} · {}", track.artist, track.album)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child(format_time(track.duration_ms)),
                )
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "queue-remove-{queue_index}-{track_id}"
                        )))
                        .size(px(26.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .hover(|s| s.bg(theme::bg_active()))
                        .child(themed_icon(
                            icon!(x),
                            14.0,
                            TEXT_TERTIARY.into(),
                        ))
                        .on_mouse_down(gpui::MouseButton::Left, app_listener(view, move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.remove_from_queue(track_id, cx);
                        })),
                ),
        )
}

fn empty_filter_state() -> impl IntoElement {
    div()
        .w_full()
        .p_12()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .rounded_2xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(TEXT_PRIMARY)
                .child("没有匹配的内容"),
        )
        .child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("尝试更改过滤条件或添加新的音乐文件夹"),
        )
}

fn normalized_query(search: &str) -> String {
    search.trim().to_lowercase()
}

fn track_matches_query(track: &Track, query: &str) -> bool {
    query.is_empty()
        || track.title.to_lowercase().contains(query)
        || track.artist.to_lowercase().contains(query)
        || track.album.to_lowercase().contains(query)
}

fn matching_track_indices(tracks: &[Track], query: &str) -> Option<Arc<[usize]>> {
    if query.is_empty() {
        return None;
    }

    Some(
        tracks
            .iter()
            .enumerate()
            .filter_map(|(index, track)| track_matches_query(track, query).then_some(index))
            .collect::<Vec<_>>()
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn track(title: &str, artist: &str, album: &str) -> Track {
        Track::new(crate::model::TrackData {
            id: 1,
            path: PathBuf::from("song.mp3"),
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            year: None,
            genre: None,
            duration_ms: 0,
            codec: "MP3".into(),
            sample_rate: 44_100,
            channels: 2,
            artwork_key: None,
        })
    }

    #[test]
    fn empty_query_uses_original_track_order_without_indices() {
        let tracks = vec![track("One", "Artist", "Album")];

        assert!(matching_track_indices(&tracks, "").is_none());
    }

    #[test]
    fn matching_indices_are_case_insensitive_and_keep_source_indices() {
        let tracks = vec![
            track("First", "Artist", "Album"),
            track("Second", "Singer", "Record"),
            track("Third", "Artist", "Collection"),
        ];

        let query = normalized_query("sInGeR");
        assert_eq!(
            matching_track_indices(&tracks, &query)
                .expect("non-empty query")
                .as_ref(),
            &[1]
        );
    }
}
