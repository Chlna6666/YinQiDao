use std::{ops::Range, sync::Arc};

use crate::model::PlaybackState;

use gpui::{
    Animation, AnimationExt as _, AnimationProperty, AnimationSpec, CompositeLayerExt as _,
    Context, Easing, ElementId, IntoElement, SharedString, WeakEntity, Window, div, point,
    prelude::*, px, uniform_list,
};
use lucide_gpui::icon;

use super::{
    image_cache, library, queue_cover,
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

/// Resolve the initial drawer position from actual transport identity first,
/// then the selected source being prepared, and finally persisted list position.
/// Only called for opening/explicit transport changes, never on each render.
pub(super) fn focus_source_index<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    playing_source: Option<&str>,
    buffering_source: Option<&str>,
    fallback_index: usize,
) -> Option<usize> {
    let mut count = 0;
    let mut playing_index = None;
    let mut buffering_index = None;
    for (index, id) in ids.into_iter().enumerate() {
        count += 1;
        if playing_source == Some(id) {
            playing_index = Some(index);
        }
        if buffering_source == Some(id) {
            buffering_index = Some(index);
        }
    }
    playing_index
        .or(buffering_index)
        .or_else(|| (count > 0).then_some(fallback_index.min(count.saturating_sub(1))))
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
    let provider = online_queue.map(|queue| {
        let labels = crate::plugin::management::provider_display_labels();
        labels
            .get(&queue.route.provider_id)
            .cloned()
            .unwrap_or_else(|| queue.route.provider_id.clone())
    });
    let rows = if let Some(queue) = online_queue {
        online_queue_items(app, queue, &parent)
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
        .top(px(TITLEBAR_HEIGHT))
        .bottom(px(MINI_PLAYER_HEIGHT))
        .w(px(width))
        .flex()
        .flex_col()
        .occlude()
        .rounded(px(16.0))
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
                .gap_3()
                .children(
                    online_queue
                        // The former header always used the FIRST song's cover.
                        // An arbitrary first track is not a playlist cover.
                        .and_then(|queue| queue.tracks.get(queue.current_index))
                        .map(|track| {
                            image_cache::render_remote_cover(
                                track.cover_url.as_deref(),
                                44.0,
                                44.0,
                                10.0,
                                cx,
                            )
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
                                .text_lg()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(TEXT_PRIMARY)
                                .truncate()
                                .child(title),
                        )
                        .child(div().text_xs().text_color(TEXT_SECONDARY).child(format!(
                            "共 {count} 首 · {}",
                            provider.as_deref().unwrap_or("本地曲库")
                        ))),
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
        .top(px(0.0))
        .bottom(px(0.0))
        .left(px(0.0))
        .right(px(0.0))
        .child(scrim)
        .child(sheet)
        .into_any_element()
}

fn online_queue_items(
    app: &MusicApp,
    queue: &OnlinePlaylistQueue,
    parent: &WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let tracks = Arc::clone(&queue.tracks);
    let route = queue.route.clone();
    let playlist_title = queue.playlist_title.clone();
    let current_index = queue.current_index;
    let current_source = app.snapshot.current_track.as_ref().and_then(|track| {
        app.online_remote_tracks
            .get(&track.id)
            .map(|(_, remote)| remote.source.clone())
    });
    let has_current_track = app.snapshot.current_track.is_some();
    let playback_state = app.snapshot.state;
    let hovered_cover = app.queue_drawer_hovered_cover;
    let buffering_source = app.online_track_buffering.clone();
    let labels = crate::plugin::management::provider_display_labels();
    let provider = labels
        .get(&route.provider_id)
        .cloned()
        .unwrap_or_else(|| route.provider_id.clone());
    let parent = parent.clone();

    div()
        .size_full()
        .overflow_hidden()
        .child(
            uniform_list(
                "queue-drawer-online-vlist",
                tracks.len(),
                move |range: Range<usize>, _, cx| {
                    let mut rows = Vec::with_capacity(range.len());
                    for index in range {
                        let Some(track) = tracks.get(index) else {
                            continue;
                        };
                        let active = current_source.as_ref().map_or(
                            has_current_track
                                && index == current_index
                                && buffering_source.is_none(),
                            |source| source == &track.source,
                        );
                        let buffering =
                            buffering_source.as_deref() == Some(track.source.source_id.as_str());
                        let hovered = hovered_cover == Some(index);
                        let title = track.title.clone();
                        let artist = if track.artists.is_empty() {
                            "未知艺术家".to_owned()
                        } else {
                            track.artists.join(" / ")
                        };
                        let duration = format_time(track.duration_ms.unwrap_or(0));

                        // Loading the cover is lazy: only visible virtual rows request images.
                        let artwork = image_cache::render_remote_cover_with_sink(
                            track.cover_url.as_deref(),
                            42.0,
                            42.0,
                            9.0,
                            &parent,
                            cx,
                        );
                        let cover = queue_cover::tile(
                            artwork,
                            active,
                            playback_state,
                            buffering,
                            hovered,
                            index as u64,
                        );
                        let hover_parent = parent.clone();
                        let row_parent = parent.clone();
                        let action_route = route.clone();
                        let action_title = playlist_title.clone();
                        let action_tracks = tracks.clone();
                        let row = div()
                            .id(SharedString::from(format!("drawer-online-row-{index}")))
                            .h(px(64.0))
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .rounded(px(10.0))
                            .cursor_pointer()
                            .bg(if active {
                                theme::accent_red_muted()
                            } else {
                                theme::transparent()
                            })
                            .hover(|s| {
                                s.bg(if active {
                                    theme::accent_red_active()
                                } else {
                                    theme::bg_hover()
                                })
                            })
                            .transition(theme::hover_transition())
                            .child(
                                div()
                                    .id(SharedString::from(format!("drawer-online-cover-{index}")))
                                    .size(px(42.0))
                                    .flex_none()
                                    .cursor_pointer()
                                    .on_hover(app_listener(
                                        &hover_parent,
                                        move |app, is_hovered: &bool, _, cx| {
                                            let next = if *is_hovered { Some(index) } else { None };
                                            if (*is_hovered
                                                || app.queue_drawer_hovered_cover == Some(index))
                                                && app.queue_drawer_hovered_cover != next
                                            {
                                                app.queue_drawer_hovered_cover = next;
                                                cx.notify();
                                            }
                                        },
                                    ))
                                    .child(cover),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(if active {
                                                gpui::FontWeight::SEMIBOLD
                                            } else {
                                                gpui::FontWeight::MEDIUM
                                            })
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
                                            .child(format!("{artist} · {provider}")),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(TEXT_TERTIARY)
                                    .child(duration),
                            )
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                app_listener(&row_parent, move |app, _, _, cx| {
                                    if active && !buffering {
                                        app.seek_to_ms(0, cx);
                                        if app.snapshot.state != PlaybackState::Playing {
                                            app.toggle_play(cx);
                                        }
                                    } else {
                                        app.play_online_playlist_track_shared(
                                            action_route.clone(),
                                            action_title.clone(),
                                            action_tracks.clone(),
                                            index,
                                            cx,
                                        );
                                    }
                                }),
                            );
                        rows.push(row.into_any_element());
                    }
                    rows
                },
            )
            .track_scroll(app.queue_drawer_online_scroll.clone())
            .size_full(),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawer_focus_prioritizes_playing_source_and_clamps_restored_position() {
        assert_eq!(
            focus_source_index(
                ["first", "second", "third"],
                Some("second"),
                Some("third"),
                0
            ),
            Some(1)
        );
        assert_eq!(
            focus_source_index(["first", "second", "third"], None, Some("third"), 0),
            Some(2)
        );
        assert_eq!(
            focus_source_index(["first", "second", "third"], Some("gone"), None, 99),
            Some(2)
        );
        assert_eq!(focus_source_index([], None, None, 0), None);
    }

    #[test]
    fn drawer_stays_within_small_and_large_windows() {
        assert_eq!(drawer_width(1200.0), DRAWER_MAX_WIDTH);
        assert!(drawer_width(320.0) <= 320.0);
        assert!(drawer_width(320.0) > 0.0);
    }
}
