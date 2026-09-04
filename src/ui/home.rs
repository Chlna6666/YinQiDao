use std::collections::HashSet;

use gpui::{
    EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, WeakEntity, div, hsla,
    img, linear_color_stop, linear_gradient, prelude::*, px, rgb,
};
use lucide_gpui::icons as lucide_icons;

use crate::model::Track;

use super::{
    shell::{MusicApp, app_listener},
    theme::{
        self, ACCENT_RED, BORDER_CARD, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, TEXT_WHITE,
        elegant_gradient_for, format_time, press_transition, themed_icon, waveform_animation,
    },
};

pub(super) fn render(app: &MusicApp, view: &WeakEntity<MusicApp>) -> gpui::AnyElement {
    if app.tracks.is_empty() {
        return div()
            .id("home-scroll-empty")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p_8()
            .gap_8()
            .child(header(app, view))
            .child(empty_state(app, view))
            .into_any_element();
    }

    div()
        .id("home-scroll")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .p_8()
        .gap_8()
        .child(header(app, view))
        .child(stats_overview(app))
        .child(featured_albums_section(app, view))
        .child(recent_tracks_section(app, view))
        .into_any_element()
}

fn header(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let greeting = current_time_greeting();

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
                        .child(greeting),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(TEXT_SECONDARY)
                        .child(if app.scan_in_progress {
                            "正在高速扫描音乐目录元数据…".to_string()
                        } else {
                            "你的专属离线音乐库持续保持最新".to_string()
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .id("home-add-folder")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(theme::accent_red_muted())
                        .text_color(ACCENT_RED)
                        .border_1()
                        .border_color(hsla(348.0, 0.95, 0.56, 0.20))
                        .hover(|s| s.bg(theme::accent_red_active()))
                        .transition(press_transition())
                        .active(|s| s.scale(0.96))
                        .child(themed_icon(
                            lucide_icons::icon_folder_plus(),
                            16.0,
                            ACCENT_RED.into(),
                        ))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("添加音乐目录"),
                        )
                        .on_click(app_listener(view, |this, _, _, cx| this.choose_folder(cx))),
                )
                .child(
                    div()
                        .id("home-rescan-btn")
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .rounded_full()
                        .cursor_pointer()
                        .bg(rgb(0xff_ff_ff))
                        .text_color(TEXT_PRIMARY)
                        .border_1()
                        .border_color(BORDER_CARD)
                        .hover(|s| s.bg(theme::bg_hover()))
                        .transition(press_transition())
                        .active(|s| s.scale(0.96))
                        .child(themed_icon(
                            lucide_icons::icon_refresh_cw(),
                            15.0,
                            hsla(220.0, 0.08, 0.50, 1.0),
                        ))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child("重新扫描"),
                        )
                        .on_click(app_listener(view, |this, _, _, cx| this.rescan(cx))),
                ),
        )
}

fn stats_overview(app: &MusicApp) -> impl IntoElement {
    let track_count = app.tracks.len();
    let album_count = app
        .tracks
        .iter()
        .map(|t| t.album.as_str())
        .collect::<HashSet<_>>()
        .len();
    let artist_count = app
        .tracks
        .iter()
        .map(|t| t.artist.as_str())
        .collect::<HashSet<_>>()
        .len();

    div()
        .flex()
        .items_center()
        .gap_4()
        .child(stat_badge(
            "歌曲总计",
            &format!("{track_count} 首"),
            lucide_icons::icon_music(),
        ))
        .child(stat_badge(
            "已收录专辑",
            &format!("{album_count} 张"),
            lucide_icons::icon_disc_3(),
        ))
        .child(stat_badge(
            "艺术家",
            &format!("{artist_count} 位"),
            lucide_icons::icon_users_round(),
        ))
}

fn stat_badge(label: &'static str, val: &str, icon: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .rounded_xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .size(px(32.0))
                .rounded_lg()
                .bg(theme::accent_red_muted())
                .flex()
                .items_center()
                .justify_center()
                .child(themed_icon(icon, 16.0, ACCENT_RED.into())),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(TEXT_TERTIARY).child(label))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(val.to_owned()),
                ),
        )
}

fn featured_albums_section(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    // 聚合出前 8 个独特专辑进行卡片展示
    let mut albums = Vec::new();
    let mut seen = HashSet::new();
    for track in &app.tracks {
        if seen.insert(track.album.clone()) {
            albums.push(track);
            if albums.len() >= 8 {
                break;
            }
        }
    }

    let mut grid = div().flex().flex_wrap().gap_5();

    for track in albums {
        grid = grid.child(album_card(track, app, view));
    }

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child("精选与专辑推荐"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child("点击卡片畅享整张专辑"),
                ),
        )
        .child(grid)
}

fn album_card(track: &Track, app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let track_id = track.id;
    let artwork = app.artworks.get(&track_id).cloned();

    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(150.0))
            .rounded_xl()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track_id);
        div()
            .size(px(150.0))
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
                lucide_icons::icon_disc_3(),
                42.0,
                hsla(0.0, 0.0, 1.0, 0.80),
            ))
            .into_any_element()
    };

    div()
        .id(SharedString::from(format!("home-album-card-{track_id}")))
        .w(px(150.0))
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
                .size(px(150.0))
                .rounded_xl()
                .overflow_hidden()
                .relative()
                .child(cover)
                // 悬停播放指示层
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_xl()
                        .bg(hsla(0.0, 0.0, 0.0, 0.20))
                        .opacity(0.0)
                        .hover(|s| s.opacity(1.0))
                        .transition(press_transition())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(40.0))
                                .rounded_full()
                                .bg(ACCENT_RED)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(themed_icon(
                                    lucide_icons::icon_play(),
                                    20.0,
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
                        .child(track.album.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .truncate()
                        .child(track.artist.clone()),
                ),
        )
        .on_click(app_listener(view, move |this, _, _, cx| {
            this.play_track(track_id, cx)
        }))
}

fn recent_tracks_section(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let mut list = div().flex().flex_col().gap_1p5();

    for track in app.tracks.iter().take(8) {
        list = list.child(track_row(track, app, view));
    }

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child("最近收录歌曲"),
                )
                .child(
                    div()
                        .id("home-see-all-btn")
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ACCENT_RED)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.80))
                        .child("查看全部歌曲 →")
                        .on_click(app_listener(view, |this, _, _, cx| {
                            this.show_library_tab(crate::model::LibraryTab::Songs, cx);
                        })),
                ),
        )
        .child(list)
}

pub(super) fn track_row(
    track: &Track,
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let track_id = track.id;
    let is_current = app
        .snapshot
        .current_track
        .as_ref()
        .is_some_and(|t| t.id == track_id);
    let is_playing = is_current && app.snapshot.state == crate::model::PlaybackState::Playing;
    let artwork = app.artworks.get(&track_id).cloned();

    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(42.0))
            .rounded_md()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track_id);
        div()
            .size(px(42.0))
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
                lucide_icons::icon_music(),
                18.0,
                hsla(0.0, 0.0, 1.0, 0.85),
            ))
            .into_any_element()
    };

    div()
        .id(SharedString::from(format!("home-track-{track_id}")))
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .rounded_xl()
        .cursor_pointer()
        .bg(if is_current {
            theme::accent_red_muted()
        } else {
            rgb(0xff_ff_ff).into()
        })
        .border_1()
        .border_color(if is_current {
            hsla(348.0, 0.95, 0.56, 0.25)
        } else {
            BORDER_CARD.into()
        })
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(press_transition())
        .active(|s| s.scale(0.99))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .flex_1()
                .min_w(px(0.0))
                .child(cover)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(1.0))
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
                .gap_4()
                .child_if(is_playing, || waveform_animation(true))
                .child(
                    div()
                        .id(SharedString::from(format!("home-add-queue-{track_id}")))
                        .size(px(26.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .hover(|s| s.bg(theme::bg_active()))
                        .child(themed_icon(
                            lucide_icons::icon_plus(),
                            14.0,
                            hsla(220.0, 0.08, 0.50, 1.0),
                        ))
                        .on_click(app_listener(view, move |this, _, _, cx| {
                            this.add_to_queue(track_id, cx);
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child(format_time(track.duration_ms)),
                ),
        )
        .on_click(app_listener(view, move |this, _, _, cx| {
            this.play_track(track_id, cx)
        }))
}

fn empty_state(_app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    div()
        .w_full()
        .p_12()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .rounded_2xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .size(px(56.0))
                .rounded_full()
                .bg(theme::accent_red_muted())
                .flex()
                .items_center()
                .justify_center()
                .child(themed_icon(
                    lucide_icons::icon_folder_open(),
                    28.0,
                    ACCENT_RED.into(),
                )),
        )
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(TEXT_PRIMARY)
                .child("尚未添加本地歌曲"),
        )
        .child(
            div()
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .child("选择一个包含音频的文件夹，音栖岛会自动提取元数据并建立本地音乐索引库"),
        )
        .child(
            div()
                .id("empty-add-folder")
                .flex()
                .items_center()
                .gap_2()
                .px_5()
                .py_2p5()
                .rounded_full()
                .cursor_pointer()
                .bg(ACCENT_RED)
                .text_color(TEXT_WHITE)
                .hover(|s| s.opacity(0.90))
                .transition(press_transition())
                .active(|s| s.scale(0.96))
                .child(themed_icon(
                    lucide_icons::icon_folder_plus(),
                    16.0,
                    hsla(0.0, 0.0, 1.0, 1.0),
                ))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("立即选择音乐目录"),
                )
                .on_click(app_listener(view, |this, _, _, cx| this.choose_folder(cx))),
        )
        .child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("全面支持 FLAC、WAV、APE、ALAC、MP3、M4A、AAC、OGG 等高保真无损格式"),
        )
}

fn current_time_greeting() -> &'static str {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 粗略按当地时间换算（以东八区 UTC+8 为主，也可按当前系统时区）：
    let hour = ((now + 8 * 3600) % 86400) / 3600;
    if hour < 12 {
        "早上好，开启今日动听旋律"
    } else if hour < 18 {
        "下午好，来一段惬意音乐时光"
    } else {
        "晚上好，让音乐为你卸下疲惫"
    }
}
