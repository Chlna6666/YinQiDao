use std::{ops::Range, sync::Arc};

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, CompositeLayerExt as _,
    Context, Easing, ElementId, IntoElement, SharedString, WeakEntity, Window, div, point,
    prelude::*, px, uniform_list,
};
use lucide_gpui::icon;

use super::{
    library,
    shell::{MusicApp, OnlinePlaylistQueue, app_listener},
    theme::{
        self, ACCENT_RED, BORDER_CARD, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, format_time,
        themed_icon,
    },
};

const DRAWER_MAX_WIDTH: f32 = 420.0;
const DRAWER_GUTTER: f32 = 16.0;
const TITLEBAR_HEIGHT: f32 = 38.0;
const MINI_PLAYER_HEIGHT: f32 = 72.0;

fn drawer_width(viewport_width: f32) -> f32 {
    (viewport_width - DRAWER_GUTTER)
        .min(DRAWER_MAX_WIDTH)
        .max(1.0)
}

/// An absolute right-hand sheet, not an AppPage. Geometry stays fixed while
/// the compositor translates the panel and fades its scrim with one epoch.
pub(super) fn render(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
    window: &mut Window,
) -> gpui::AnyElement {
    let width = drawer_width(f32::from(window.viewport_size().width));
    let (from, to, duration) = app.queue_drawer_motion.segment();
    let animating = app.queue_drawer_motion.deadline().is_some();
    let epoch = app.queue_drawer_epoch;
    let parent = cx.entity().downgrade();

    let online_queue = app
        .online_playlist_queue
        .as_ref()
        .filter(|queue| !queue.tracks.is_empty());
    let count = online_queue.map_or(app.config.queue.len(), |queue| queue.tracks.len());
    let title = online_queue
        .map(|queue| queue.playlist_title.clone())
        .unwrap_or_else(|| "播放队列".to_owned());
    let rows = if let Some(queue) = online_queue {
        online_queue_items(queue, &parent)
    } else {
        library::queue_drawer_items(app, &parent)
    };

    let scrim = div()
        .id("queue-drawer-scrim")
        .absolute()
        .inset_0()
        .bg(theme::dark::scrim(0.18))
        .opacity(if animating { 1.0 } else { to })
        .occlude()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|app, _, _, cx| {
                cx.stop_propagation();
                app.begin_queue_drawer_transition(false, cx);
            }),
        );
    let scrim = if animating {
        scrim
            .with_animation(
                ElementId::NamedInteger(SharedString::new_static("queue-scrim"), epoch),
                Animation::from_spec(AnimationSpec::new(duration).ease(Easing::OutCubic))
                    .with_property(AnimationProperty::opacity(from, to)),
                |element, _| element,
            )
            .into_any_element()
    } else {
        scrim.into_any_element()
    };

    let sheet = div()
        .id("queue-drawer-sheet")
        .absolute()
        .right(px(0.0))
        .top(px(0.0))
        .bottom(px(0.0))
        .w(px(width))
        .flex()
        .flex_col()
        .occlude()
        .rounded_2xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(BORDER_CARD)
        .shadow_lg()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .px_5()
                .pt_5()
                .pb_4()
                .border_b_1()
                .border_color(BORDER_CARD)
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(TEXT_PRIMARY)
                                .truncate()
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_SECONDARY)
                                .child(format!("共 {count} 首 · 当前页面保持不变")),
                        ),
                )
                .child(
                    div()
                        .id("queue-drawer-close")
                        .size(px(30.0))
                        .rounded_full()
                        .cursor_pointer()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(theme::BG_CANVAS)
                        .hover(|s| s.bg(theme::bg_hover()))
                        .child(themed_icon(icon!(x), 16.0, TEXT_SECONDARY.into()))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|app, _, _, cx| {
                                cx.stop_propagation();
                                app.begin_queue_drawer_transition(false, cx);
                            }),
                        ),
                ),
        )
        .child(
            div()
                .px_5()
                .py_3()
                .flex_none()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_SECONDARY)
                        .child(if online_queue.is_some() {
                            "当前在线歌单"
                        } else {
                            "待播放曲目"
                        }),
                )
                .children((online_queue.is_none() && count > 0).then(|| {
                    div()
                        .id("queue-drawer-clear")
                        .cursor_pointer()
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .hover(|s| s.text_color(ACCENT_RED))
                        .child("清空")
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            app_listener(&parent, |app, _, _, cx| {
                                cx.stop_propagation();
                                app.clear_queue(cx);
                            }),
                        )
                })),
        )
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .px_2()
                .pb_3()
                .child(rows),
        )
        .composite_layer();

    let sheet = if animating {
        sheet
            .with_animation(
                ElementId::NamedInteger(SharedString::new_static("queue-sheet"), epoch),
                Animation::from_spec(AnimationSpec::new(duration).ease(Easing::OutCubic))
                    .with_property(AnimationProperty::translation(
                        point(px(width * (1.0 - from)), px(0.0)),
                        point(px(width * (1.0 - to)), px(0.0)),
                    )),
                |element, _| element,
            )
            .into_any_element()
    } else {
        sheet.into_any_element()
    };

    div()
        .id("queue-drawer-overlay")
        .absolute()
        .top(px(TITLEBAR_HEIGHT))
        .bottom(px(MINI_PLAYER_HEIGHT))
        .left(px(0.0))
        .right(px(0.0))
        .overflow_hidden()
        .child(scrim)
        .child(sheet)
        .into_any_element()
}

fn online_queue_items(
    queue: &OnlinePlaylistQueue,
    parent: &WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let tracks = Arc::clone(&queue.tracks);
    let route = queue.route.clone();
    let playlist_title = queue.playlist_title.clone();
    let current_index = queue.current_index;
    let parent = parent.clone();

    div()
        .size_full()
        .overflow_hidden()
        .child(
            uniform_list(
                "queue-drawer-online-vlist",
                tracks.len(),
                move |range: Range<usize>, _, _cx| {
                    let mut rows = Vec::with_capacity(range.len());
                    for index in range {
                        let Some(track) = tracks.get(index) else {
                            continue;
                        };
                        let active = index == current_index;
                        let route = route.clone();
                        let source_tracks = tracks.clone();
                        let playlist_title = playlist_title.clone();
                        let title = track.title.clone();
                        let artist = if track.artists.is_empty() {
                            "未知艺术家".to_owned()
                        } else {
                            track.artists.join(" / ")
                        };
                        let duration = format_time(track.duration_ms.unwrap_or(0));
                        rows.push(
                            div()
                                .id(SharedString::from(format!("drawer-online-row-{index}")))
                                .h(px(62.0))
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_3()
                                .px_3()
                                .rounded_xl()
                                .cursor_pointer()
                                .bg(if active {
                                    theme::accent_red_muted()
                                } else {
                                    theme::transparent()
                                })
                                .hover(|s| s.bg(theme::bg_hover()))
                                .child(
                                    div()
                                        .w(px(24.0))
                                        .text_xs()
                                        .text_color(if active { ACCENT_RED } else { TEXT_TERTIARY })
                                        .child(if active {
                                            "▶".to_owned()
                                        } else {
                                            format!("{}", index + 1)
                                        }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .text_color(if active {
                                                    ACCENT_RED
                                                } else {
                                                    TEXT_PRIMARY
                                                })
                                                .truncate()
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(TEXT_SECONDARY)
                                                .truncate()
                                                .child(artist),
                                        ),
                                )
                                .child(div().text_xs().text_color(TEXT_TERTIARY).child(duration))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    app_listener(&parent, move |app, _, _, cx| {
                                        app.play_online_playlist_track_shared(
                                            route.clone(),
                                            playlist_title.clone(),
                                            source_tracks.clone(),
                                            index,
                                            cx,
                                        );
                                    }),
                                )
                                .into_any_element(),
                        );
                    }
                    rows
                },
            )
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawer_stays_within_small_and_large_windows() {
        assert_eq!(drawer_width(1200.0), DRAWER_MAX_WIDTH);
        assert!(drawer_width(320.0) <= 320.0);
        assert!(drawer_width(320.0) > 0.0);
    }
}
