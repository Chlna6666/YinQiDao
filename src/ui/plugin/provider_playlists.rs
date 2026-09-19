use std::{cell::RefCell, ops::Range, sync::Arc};

use anyhow::Result;
use gpui::{
    Context, IntoElement, SharedString, WeakEntity, div, prelude::*, px, rgb, uniform_list,
};
use gpui_tokio::Tokio;
use lucide_gpui::icon;

use crate::{
    plugin::{
        abi::{PlaylistDescriptor, PluginRoute, RoutingPolicy},
        extensions, frontend,
        playlists::PluginPlaylistFanout,
    },
    ui::{MusicApp, theme},
};

use super::plugin_context_menu;

const ROW_HEIGHT: f32 = 56.0;
const MAX_VISIBLE_ROWS: usize = 2;

#[derive(Clone)]
struct ProviderPlaylistRow {
    route: PluginRoute,
    playlist: PlaylistDescriptor,
}

#[derive(Clone)]
struct ProviderPlaylistSnapshot {
    generation: u64,
    rows: Arc<[ProviderPlaylistRow]>,
    route_count: usize,
    failure_count: usize,
    task_error_count: usize,
    client_ready: bool,
}

#[derive(Default)]
struct ProviderPlaylistState {
    loading: bool,
    loading_generation: Option<u64>,
    request_id: u64,
    snapshot: Option<ProviderPlaylistSnapshot>,
    error: Option<String>,
}

thread_local! {
    static STATE: RefCell<ProviderPlaylistState> = RefCell::new(ProviderPlaylistState::default());
}

/// Render a Host-owned Provider playlist snapshot. This function never calls a guest, performs
/// network/file I/O, or reads the plugin registry under a lock. The generation read is atomic and
/// is used only to invalidate snapshots after update/disable/uninstall.
pub(super) fn render(view: &WeakEntity<MusicApp>) -> gpui::AnyElement {
    let generation = extensions::theme_registry_generation();
    let (loading, snapshot, error) = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let snapshot_stale = state
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.generation != generation);
        let request_stale = state.loading
            && state
                .loading_generation
                .is_some_and(|loading_generation| loading_generation != generation);
        if snapshot_stale || request_stale {
            state.snapshot = None;
            state.loading = false;
            state.loading_generation = None;
            state.request_id = state.request_id.wrapping_add(1);
            state.error = Some("插件已更新，请重新同步 Provider 播放列表".into());
        }
        (state.loading, state.snapshot.clone(), state.error.clone())
    });

    let rows = snapshot
        .as_ref()
        .map(|snapshot| snapshot.rows.clone())
        .unwrap_or_else(|| Arc::<[ProviderPlaylistRow]>::from([]));
    let row_count = rows.len();
    let route_count = snapshot.as_ref().map_or(0, |snapshot| snapshot.route_count);
    let failure_count = snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.failure_count + snapshot.task_error_count);
    let client_ready = snapshot
        .as_ref()
        .is_none_or(|snapshot| snapshot.client_ready);

    let summary = if let Some(error) = error.as_deref() {
        if row_count > 0 {
            format!("上次同步失败，继续显示缓存 · {error}")
        } else {
            error.to_owned()
        }
    } else if !client_ready {
        "Provider runtime 尚未就绪；当前没有可执行的 Component adapter".into()
    } else if snapshot.is_none() {
        "点击同步读取当前已认证、支持 Playlists 的 Provider".into()
    } else if row_count == 0 {
        if route_count == 0 {
            "当前没有通过会话与健康检查的 Playlists Provider route".into()
        } else {
            "当前认证 Provider 没有返回播放列表".into()
        }
    } else if failure_count > 0 {
        format!("{route_count} 个 route · {failure_count} 个调用失败；其余结果已保留")
    } else {
        format!("{route_count} 个 Provider route · Host 安全快照")
    };

    let view_sync = view.clone();
    let sync_button = div()
        .id("provider-playlists-sync")
        .px_3()
        .py_1p5()
        .rounded_full()
        .cursor_pointer()
        .opacity(if loading { 0.55 } else { 1.0 })
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(theme::BORDER_CARD)
        .text_xs()
        .text_color(if loading {
            theme::TEXT_TERTIARY
        } else {
            theme::TEXT_SECONDARY
        })
        .hover(move |style| {
            if loading {
                style
            } else {
                style.text_color(theme::ACCENT_RED)
            }
        })
        .child(if loading { "同步中…" } else { "同步" })
        .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            if loading {
                return;
            }
            let _ = view_sync.update(cx, request_sync);
        });

    if rows.is_empty() {
        return div()
            .flex_none()
            .h(px(88.0))
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .px_4()
            .rounded_xl()
            .bg(rgb(0xff_ff_ff))
            .border_1()
            .border_color(theme::BORDER_CARD)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .size(px(34.0))
                            .rounded_lg()
                            .bg(theme::accent_red_muted())
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(theme::themed_icon(
                                icon!(list_music),
                                16.0,
                                theme::ACCENT_RED.into(),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w(px(0.0))
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme::TEXT_PRIMARY)
                                    .child("Provider 播放列表"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::TEXT_TERTIARY)
                                    .truncate()
                                    .child(summary),
                            ),
                    ),
            )
            .child(sync_button)
            .into_any_element();
    }

    let visible_rows = row_count.min(MAX_VISIBLE_ROWS) as f32;
    let list_height = (visible_rows * ROW_HEIGHT).max(ROW_HEIGHT);
    let rows_for_list = rows.clone();
    let view_rows = view.clone();

    div()
        .flex_none()
        .h(px(46.0 + list_height))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child(format!("Provider 播放列表 · {row_count} 个")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .truncate()
                                .child(summary),
                        ),
                )
                .child(sync_button),
        )
        .child(
            div()
                .h(px(list_height))
                .overflow_hidden()
                .child(
                    uniform_list(
                        "library-provider-playlists-vlist",
                        row_count,
                        move |range: Range<usize>, _window, _cx| {
                            let mut items = Vec::with_capacity(range.end - range.start);
                            for index in range {
                                if let Some(row) = rows_for_list.get(index) {
                                    items.push(provider_playlist_row(
                                        index,
                                        row,
                                        &view_rows,
                                        generation,
                                    ));
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

fn provider_playlist_row(
    index: usize,
    row: &ProviderPlaylistRow,
    view: &WeakEntity<MusicApp>,
    generation: u64,
) -> gpui::AnyElement {
    let playlist = row.playlist.clone();
    let plugin_id = row.route.plugin_id.clone();
    let view_context = view.clone();
    let count = playlist
        .track_count
        .map(|count| format!("{count} 首"))
        .unwrap_or_else(|| "曲目数未知".into());
    let editability = if playlist.editable { "可编辑" } else { "只读" };

    div()
        .id(SharedString::from(format!("provider-playlist-row-{index}")))
        .h(px(52.0))
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_4()
        .rounded_xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(theme::BORDER_CARD)
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
                        .child(theme::themed_icon(
                            icon!(list_music),
                            15.0,
                            theme::ACCENT_RED.into(),
                        )),
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
                                .text_color(theme::TEXT_PRIMARY)
                                .truncate()
                                .child(playlist.name.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .truncate()
                                .child(format!(
                                    "{} · {} · {count} · {editability}",
                                    playlist.provider_id, plugin_id
                                )),
                        ),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child("PlaylistContext"),
        )
        .on_mouse_down(gpui::MouseButton::Right, move |event, window, cx| {
            cx.stop_propagation();
            if extensions::theme_registry_generation() != generation {
                let _ = view_context.update(cx, |app, app_cx| {
                    app.status = "Provider 播放列表已失效：插件运行代际已变化，请重新同步".into();
                    app_cx.notify();
                });
                return;
            }
            let position = event.position;
            let viewport = window.viewport_size();
            let playlist = playlist.clone();
            let _ = view_context.update(cx, |app, app_cx| {
                plugin_context_menu::open_provider_playlist(
                    app,
                    &playlist.name,
                    &playlist.provider_id,
                    &playlist.source_id,
                    position,
                    viewport,
                    app_cx,
                );
            });
        })
        .into_any_element()
}

fn request_sync(app: &mut MusicApp, cx: &mut Context<MusicApp>) {
    let generation = extensions::theme_registry_generation();
    let request_id = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.loading {
            return None;
        }
        state.request_id = state.request_id.wrapping_add(1);
        state.loading = true;
        state.loading_generation = Some(generation);
        state.error = None;
        Some(state.request_id)
    });
    let Some(request_id) = request_id else {
        return;
    };

    let Some(frontend) = frontend::global() else {
        STATE.with(|slot| {
            let mut state = slot.borrow_mut();
            if state.request_id == request_id {
                state.loading = false;
                state.loading_generation = None;
                state.error = Some("插件 Provider frontend 尚未初始化".into());
            }
        });
        app.status = "同步 Provider 播放列表失败：插件 Provider frontend 尚未初始化".into();
        cx.notify();
        return;
    };

    app.status = "正在同步 Provider 播放列表…".into();
    cx.notify();

    let task = Tokio::spawn_result(cx, async move {
        frontend.playlists(&RoutingPolicy::default()).await
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |app, app_cx| {
            complete_sync(app, app_cx, request_id, generation, result);
        })?;
        Ok(())
    })
    .detach();
}

fn complete_sync(
    app: &mut MusicApp,
    cx: &mut Context<MusicApp>,
    request_id: u64,
    expected_generation: u64,
    result: Result<PluginPlaylistFanout>,
) {
    let current_generation = extensions::theme_registry_generation();
    let status = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.request_id != request_id {
            return None;
        }
        state.loading = false;
        state.loading_generation = None;

        if current_generation != expected_generation {
            state.snapshot = None;
            state.error = Some("插件在同步期间已更新，请重新同步".into());
            return Some("Provider 播放列表同步结果已丢弃：插件运行代际已变化".into());
        }

        match result {
            Ok(fanout) => {
                let route_count = fanout.plan.eligible_routes.len();
                let failure_count = fanout.failures.len();
                let task_error_count = fanout.task_errors.len();
                let client_ready = fanout.client_ready;
                let mut rows = Vec::new();
                for batch in fanout.batches {
                    for playlist in batch.playlists {
                        rows.push(ProviderPlaylistRow {
                            route: batch.route.clone(),
                            playlist,
                        });
                    }
                }
                let row_count = rows.len();
                state.snapshot = Some(ProviderPlaylistSnapshot {
                    generation: expected_generation,
                    rows: rows.into(),
                    route_count,
                    failure_count,
                    task_error_count,
                    client_ready,
                });
                state.error = None;

                if !client_ready {
                    Some("Provider 播放列表未同步：Component runtime 尚未就绪".into())
                } else if failure_count + task_error_count > 0 {
                    Some(format!(
                        "Provider 播放列表已同步 {row_count} 个，{} 个调用失败",
                        failure_count + task_error_count
                    ))
                } else {
                    Some(format!("Provider 播放列表已同步：{row_count} 个"))
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                state.error = Some(message.clone());
                Some(format!("同步 Provider 播放列表失败：{message}"))
            }
        }
    });

    if let Some(status) = status {
        app.status = status;
        cx.notify();
    }
}
