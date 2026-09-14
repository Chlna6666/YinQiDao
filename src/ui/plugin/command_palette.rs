use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};
use gpui::{Context, IntoElement, SharedString, div, hsla, prelude::*, px};
use gpui_tokio::Tokio;

use crate::plugin::{
    commands::{self, PluginCommandContext},
    extensions::{self, PluginCommandSummary, PluginCommandSurface},
};

use super::{shell::MusicApp, theme};

#[derive(Clone, Debug, Default)]
struct CommandPaletteState {
    open: bool,
    loading: bool,
    invoking: bool,
    loaded_generation: Option<u64>,
    commands: Arc<Vec<PluginCommandSummary>>,
    status: String,
}

static COMMAND_PALETTE_STATE: OnceLock<Mutex<CommandPaletteState>> = OnceLock::new();

fn state() -> &'static Mutex<CommandPaletteState> {
    COMMAND_PALETTE_STATE.get_or_init(|| Mutex::new(CommandPaletteState::default()))
}

fn snapshot() -> CommandPaletteState {
    state().lock().map(|state| state.clone()).unwrap_or_default()
}

fn refresh_if_needed(cx: &mut Context<MusicApp>) {
    let generation = extensions::theme_registry_generation();
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.loading || state.loaded_generation == Some(generation) {
            return;
        }
        state.loading = true;
    }

    let task = Tokio::spawn_result(cx, async move {
        tokio::task::spawn_blocking(move || extensions::commands(PluginCommandSurface::CommandPalette))
            .await
            .map_err(|_| anyhow!("插件 Command Palette 快照任务异常退出"))?
    });

    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if let Ok(mut state) = state().lock() {
                state.loading = false;
                state.loaded_generation = Some(generation);
                match result {
                    Ok(commands) => {
                        state.commands = Arc::new(commands);
                        state.status.clear();
                    }
                    Err(error) => {
                        state.commands = Arc::new(Vec::new());
                        state.status = format!("插件命令读取失败：{error:#}");
                    }
                }
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn set_open(open: bool, cx: &mut Context<MusicApp>) {
    if let Ok(mut state) = state().lock() {
        if state.invoking && !open {
            return;
        }
        state.open = open;
    }
    cx.notify();
}

fn invoke_command(qualified_id: String, cx: &mut Context<MusicApp>) {
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.invoking {
            return;
        }
        state.invoking = true;
        state.status = format!("正在执行 {qualified_id}…");
    }
    cx.notify();

    let task = Tokio::spawn_result(cx, async move {
        commands::invoke_command(
            &qualified_id,
            PluginCommandContext {
                surface: PluginCommandSurface::CommandPalette,
                page_id: None,
                track: None,
                playlist: None,
            },
        )
        .await
    });

    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |this, cx| {
            let message = match result {
                Ok(result) => {
                    if let Some(toast) = result.toast {
                        toast
                    } else if let Some(open_page) = result.open_page {
                        format!(
                            "插件命令执行完成；请求打开已验证页面 {}/{}",
                            open_page.plugin_id, open_page.page_id
                        )
                    } else {
                        "插件命令执行完成".into()
                    }
                }
                Err(error) => format!("插件命令执行失败：{error:#}"),
            };

            if let Ok(mut state) = state().lock() {
                state.invoking = false;
                state.open = false;
                state.status = message.clone();
            }
            this.status = message;
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn launcher(cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    div()
        .id("plugin-command-palette-launcher")
        .absolute()
        .right(px(12.0))
        .bottom(px(10.0))
        .px_2()
        .py_1()
        .rounded_lg()
        .cursor_pointer()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::TEXT_SECONDARY)
        .hover(|style| style.bg(theme::accent_red_muted()).text_color(theme::ACCENT_RED))
        .active(|style| style.scale(0.97))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|_, _, _, cx| set_open(true, cx)),
        )
        .child("插件命令")
        .into_any_element()
}

fn palette(current: &CommandPaletteState, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let mut commands = div().flex().flex_col().gap_1().overflow_y_scroll();
    for command in current.commands.iter() {
        let qualified_id = command.qualified_id.clone();
        commands = commands.child(
            div()
                .id(SharedString::from(format!("palette-command-{}", command.qualified_id)))
                .px_3()
                .py_2()
                .rounded_lg()
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .bg(theme::BG_CANVAS)
                .hover(|style| style.bg(theme::accent_red_muted()))
                .active(|style| style.scale(0.99))
                .opacity(if current.invoking { 0.55 } else { 1.0 })
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |_, _, _, cx| invoke_command(qualified_id.clone(), cx)),
                )
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .truncate()
                                .child(command.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .truncate()
                                .child(command.plugin_id.clone()),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child("执行"),
                ),
        );
    }

    let status = (!current.status.is_empty()).then(|| {
        div()
            .mt_2()
            .text_xs()
            .text_color(theme::TEXT_TERTIARY)
            .child(current.status.clone())
    });

    div()
        .id("plugin-command-palette-popover")
        .absolute()
        .right(px(12.0))
        .bottom(px(48.0))
        .w(px(460.0))
        .h(px(340.0))
        .p_3()
        .rounded_xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
        .shadow_lg()
        .flex()
        .flex_col()
        .gap_3()
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child("插件命令面板"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child("仅显示已通过 Host 校验的 CommandPalette contributions"),
                        ),
                )
                .child(
                    div()
                        .id("plugin-command-palette-close")
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(theme::TEXT_SECONDARY)
                        .hover(|style| style.bg(theme::BG_CANVAS))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|_, _, _, cx| set_open(false, cx)),
                        )
                        .child("关闭"),
                ),
        )
        .child(commands)
        .children(status)
        .into_any_element()
}

/// Decorate the mini-player with a Host-owned command palette.
///
/// Paint consumes only the cached Host snapshot. Refresh and command execution are scheduled onto
/// ordinary async/worker paths, so guest/WASM code never runs from GPUI paint/layout/input hot paths.
pub(super) fn decorate(
    _app: &MusicApp,
    cx: &mut Context<MusicApp>,
    player: gpui::AnyElement,
) -> gpui::AnyElement {
    refresh_if_needed(cx);
    let current = snapshot();

    let mut root = div().relative().w_full().child(player);
    if current.commands.is_empty() {
        return root.into_any_element();
    }

    root = root.child(launcher(cx));
    if current.open {
        root = root
            .child(
                div()
                    .id("plugin-command-palette-dismiss")
                    .absolute()
                    .inset_0()
                    .bg(hsla(0.0, 0.0, 0.0, 0.08))
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        cx.listener(|_, _, _, cx| set_open(false, cx)),
                    ),
            )
            .child(palette(&current, cx));
    }
    root.into_any_element()
}
