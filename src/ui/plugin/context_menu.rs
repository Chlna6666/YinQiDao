use std::{
    cell::{Cell, RefCell},
    sync::Arc,
};

use anyhow::Result;
use gpui::{
    Context, IntoElement, Pixels, Point, SharedString, Size, WeakEntity, div, hsla, point,
    prelude::*, px, rgb,
};
use gpui_tokio::Tokio;

use crate::{
    library::LocalPlaylistSummary,
    model::Track,
    plugin::{
        commands::{
            self, PluginCommandContext, PluginCommandOpenPage, PluginPlaylistCommandContext,
            PluginTrackCommandContext,
        },
        extensions::{self, PluginCommandSummary, PluginCommandSurface},
    },
    ui::{MusicApp, plugin_navigation, theme},
};

const SHELL_SIDEBAR_WIDTH: f32 = 236.0;
const SHELL_TITLEBAR_HEIGHT: f32 = 38.0;
const SHELL_BOTTOM_PLAYER_HEIGHT: f32 = 72.0;
const MENU_WIDTH: f32 = 300.0;
const MENU_MAX_HEIGHT: f32 = 360.0;
const MENU_MARGIN: f32 = 8.0;
const MENU_HEADER_HEIGHT: f32 = 56.0;
const MENU_ITEM_HEIGHT: f32 = 42.0;

#[derive(Clone)]
struct HostContextMenuState {
    position: Point<Pixels>,
    commands: Arc<[PluginCommandSummary]>,
    context: PluginCommandContext,
    surface_label: &'static str,
    invoking: Option<String>,
}

thread_local! {
    static CONTEXT_MENU: RefCell<Option<HostContextMenuState>> = const { RefCell::new(None) };
    static COMMAND_IN_FLIGHT: Cell<bool> = const { Cell::new(false) };
}

/// Capture a TrackContext command snapshot on an ordinary pointer/controller path.
/// Guest code is not executed while the menu is opened or painted.
pub(super) fn open_track(
    app: &mut MusicApp,
    track: &Track,
    position: Point<Pixels>,
    viewport: Size<Pixels>,
    cx: &mut Context<MusicApp>,
) {
    let artist = track.artist.trim();
    let context = PluginCommandContext {
        surface: PluginCommandSurface::TrackContext,
        page_id: None,
        track: Some(PluginTrackCommandContext {
            title: track.title.clone(),
            artists: if artist.is_empty() {
                Vec::new()
            } else {
                vec![track.artist.clone()]
            },
            album: track.album.clone(),
            duration_ms: Some(track.duration_ms),
            provider_id: Some("local".into()),
            source_id: Some(track.id.to_string()),
        }),
        playlist: None,
    };
    open(
        app,
        context,
        "TrackContext",
        format!("曲目插件操作：{}", track.title),
        position,
        viewport,
        cx,
    );
}

/// Capture a PlaylistContext command snapshot for a real persisted local playlist row.
/// The playlist identity comes from the Host library database, never from the queue UI.
pub(super) fn open_playlist(
    app: &mut MusicApp,
    playlist: &LocalPlaylistSummary,
    position: Point<Pixels>,
    viewport: Size<Pixels>,
    cx: &mut Context<MusicApp>,
) {
    open_playlist_context(
        app,
        &playlist.name,
        "local",
        &playlist.id.to_string(),
        position,
        viewport,
        cx,
    );
}

/// Capture a PlaylistContext command snapshot for a Provider playlist already validated by the
/// application playlist façade. Account identity remains Host-private and is deliberately omitted
/// from the minimized command context.
pub(super) fn open_provider_playlist(
    app: &mut MusicApp,
    name: &str,
    provider_id: &str,
    source_id: &str,
    position: Point<Pixels>,
    viewport: Size<Pixels>,
    cx: &mut Context<MusicApp>,
) {
    open_playlist_context(
        app,
        name,
        provider_id,
        source_id,
        position,
        viewport,
        cx,
    );
}

fn open_playlist_context(
    app: &mut MusicApp,
    name: &str,
    provider_id: &str,
    source_id: &str,
    position: Point<Pixels>,
    viewport: Size<Pixels>,
    cx: &mut Context<MusicApp>,
) {
    let context = PluginCommandContext {
        surface: PluginCommandSurface::PlaylistContext,
        page_id: None,
        track: None,
        playlist: Some(PluginPlaylistCommandContext {
            name: name.to_owned(),
            provider_id: Some(provider_id.to_owned()),
            source_id: Some(source_id.to_owned()),
        }),
    };
    open(
        app,
        context,
        "PlaylistContext",
        format!("播放列表插件操作：{name}"),
        position,
        viewport,
        cx,
    );
}

fn open(
    app: &mut MusicApp,
    context: PluginCommandContext,
    surface_label: &'static str,
    status: String,
    position: Point<Pixels>,
    viewport: Size<Pixels>,
    cx: &mut Context<MusicApp>,
) {
    let commands = match extensions::commands(context.surface) {
        Ok(commands) => commands,
        Err(error) => {
            CONTEXT_MENU.with(|state| state.borrow_mut().take());
            app.status = format!("读取插件 {surface_label} 命令失败：{error:#}");
            cx.notify();
            return;
        }
    };
    if commands.is_empty() {
        CONTEXT_MENU.with(|state| state.borrow_mut().take());
        app.status = format!("当前没有插件提供 {surface_label} 命令");
        cx.notify();
        return;
    }

    let local_width = (f32::from(viewport.width) - SHELL_SIDEBAR_WIDTH).max(MENU_WIDTH);
    let local_height = (f32::from(viewport.height)
        - SHELL_TITLEBAR_HEIGHT
        - SHELL_BOTTOM_PLAYER_HEIGHT)
        .max(MENU_HEADER_HEIGHT + MENU_ITEM_HEIGHT);
    let estimated_height = (MENU_HEADER_HEIGHT + commands.len() as f32 * MENU_ITEM_HEIGHT)
        .min(MENU_MAX_HEIGHT);
    let local_x = f32::from(position.x) - SHELL_SIDEBAR_WIDTH;
    let local_y = f32::from(position.y) - SHELL_TITLEBAR_HEIGHT;
    let max_x = (local_width - MENU_WIDTH - MENU_MARGIN).max(MENU_MARGIN);
    let max_y = (local_height - estimated_height - MENU_MARGIN).max(MENU_MARGIN);
    let position = point(
        px(local_x.clamp(MENU_MARGIN, max_x)),
        px(local_y.clamp(MENU_MARGIN, max_y)),
    );

    CONTEXT_MENU.with(|state| {
        *state.borrow_mut() = Some(HostContextMenuState {
            position,
            commands: commands.into(),
            context,
            surface_label,
            invoking: None,
        });
    });
    app.status = status;
    cx.notify();
}

/// Render only the Host-owned immutable snapshot captured by `open`.
/// No registry access, file/network I/O, or guest/WASM execution occurs here.
pub(super) fn render(view: &WeakEntity<MusicApp>) -> Option<gpui::AnyElement> {
    let state = CONTEXT_MENU.with(|slot| slot.borrow().clone())?;
    let position = state.position;
    let surface_label = state.surface_label;
    let invoking = state.invoking.clone();
    let global_busy = COMMAND_IN_FLIGHT.with(Cell::get);

    let mut commands = div().flex().flex_col().gap_1().p_1();
    for command in state.commands.iter() {
        let disabled = global_busy || invoking.is_some();
        let active = invoking.as_deref() == Some(command.qualified_id.as_str());
        let qualified_id = command.qualified_id.clone();
        let command_title = command.title.clone();
        let plugin_id = command.plugin_id.clone();
        let view_invoke = view.clone();
        commands = commands.child(
            div()
                .id(SharedString::from(format!("plugin-context-{qualified_id}")))
                .w_full()
                .min_h(px(MENU_ITEM_HEIGHT))
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .cursor_pointer()
                .opacity(if disabled && !active { 0.48 } else { 1.0 })
                .bg(if active {
                    theme::accent_red_muted()
                } else {
                    hsla(0.0, 0.0, 0.0, 0.0)
                })
                .hover(move |style| {
                    if disabled {
                        style
                    } else {
                        style.bg(theme::bg_hover())
                    }
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::TEXT_PRIMARY)
                                .truncate()
                                .child(command_title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .truncate()
                                .child(plugin_id),
                        ),
                )
                .child_if(active, || {
                    div()
                        .text_xs()
                        .text_color(theme::ACCENT_RED)
                        .child("执行中")
                })
                .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    if disabled {
                        return;
                    }
                    let qualified_id = qualified_id.clone();
                    let _ = view_invoke.update(cx, |app, app_cx| {
                        invoke_command(app, app_cx, qualified_id);
                    });
                }),
        );
    }

    let view_dismiss = view.clone();
    let menu = div()
        .absolute()
        .left(position.x)
        .top(position.y)
        .w(px(MENU_WIDTH))
        .max_h(px(MENU_MAX_HEIGHT))
        .overflow_y_scroll()
        .rounded_xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(theme::BORDER_CARD)
        .shadow_lg()
        .p_2()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .px_2()
                .pt_1()
                .pb_2()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::TEXT_PRIMARY)
                        .child("插件操作"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(format!("{surface_label} · Host 安全执行")),
                ),
        )
        .child(commands);

    Some(
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(gpui::MouseButton::Left, move |_, _, cx| {
                let _ = view_dismiss.update(cx, |app, app_cx| close(app, app_cx));
            })
            .child(menu)
            .into_any_element(),
    )
}

fn invoke_command(app: &mut MusicApp, cx: &mut Context<MusicApp>, qualified_id: String) {
    if COMMAND_IN_FLIGHT.with(Cell::get) {
        return;
    }
    let context = CONTEXT_MENU.with(|slot| {
        let mut slot = slot.borrow_mut();
        let state = slot.as_mut()?;
        if state.invoking.is_some()
            || !state
                .commands
                .iter()
                .any(|command| command.qualified_id == qualified_id)
        {
            return None;
        }
        state.invoking = Some(qualified_id.clone());
        Some(state.context.clone())
    });
    let Some(context) = context else {
        return;
    };
    COMMAND_IN_FLIGHT.with(|busy| busy.set(true));

    app.status = format!("正在执行插件命令：{qualified_id}");
    cx.notify();

    let task_id = qualified_id.clone();
    let task = Tokio::spawn_result(cx, async move {
        commands::invoke_command(&task_id, context).await
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |app, app_cx| {
            COMMAND_IN_FLIGHT.with(|busy| busy.set(false));
            CONTEXT_MENU.with(|slot| slot.borrow_mut().take());
            match result {
                Ok(result) => apply_result(app, app_cx, &qualified_id, result),
                Err(error) => {
                    app.status = format!("插件命令执行失败：{error:#}");
                    app_cx.notify();
                }
            }
        })?;
        Ok(())
    })
    .detach();
}

fn apply_result(
    app: &mut MusicApp,
    cx: &mut Context<MusicApp>,
    qualified_id: &str,
    result: commands::PluginCommandResult,
) {
    let mut denied_page = None;
    if let Some(open_page) = result.open_page {
        if let Some(target) = navigation_target(&open_page) {
            plugin_navigation::navigate(app, cx, &target);
        } else {
            denied_page = Some(open_page.qualified_id);
        }
    }

    app.status = if let Some(toast) = result.toast {
        toast
    } else if let Some(qualified_page) = denied_page {
        format!("插件命令已执行，但目标页面当前不可见：{qualified_page}")
    } else {
        format!("插件命令已执行：{qualified_id}")
    };
    cx.notify();
}

fn navigation_target(
    open_page: &PluginCommandOpenPage,
) -> Option<plugin_navigation::PluginNavigationRoute> {
    plugin_navigation::sidebar_routes()
        .unwrap_or_default()
        .into_iter()
        .chain(plugin_navigation::settings_routes().unwrap_or_default())
        .find(|target| {
            target.summary.plugin_id == open_page.plugin_id
                && target.summary.page_id == open_page.page_id
        })
}

fn close(app: &mut MusicApp, cx: &mut Context<MusicApp>) {
    let removed = CONTEXT_MENU.with(|slot| slot.borrow_mut().take().is_some());
    if removed {
        app.status = "已关闭插件操作菜单".into();
        cx.notify();
    }
}
