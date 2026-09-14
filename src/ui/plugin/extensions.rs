use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use gpui_tokio::Tokio;

use crate::plugin::{
    commands::{self, PluginCommandContext},
    extensions::{
        self, PluginCommandSummary, PluginCommandSurface, PluginHomeSectionSummary,
        PluginThemeSummary,
    },
};

use super::{plugin_theme, shell::MusicApp, theme};

#[derive(Clone, Debug, Default)]
struct PluginExtensionsSnapshot {
    loading: bool,
    operation_in_flight: bool,
    loaded: bool,
    commands: Arc<Vec<PluginCommandSummary>>,
    home_sections: Arc<Vec<PluginHomeSectionSummary>>,
    themes: Arc<Vec<PluginThemeSummary>>,
    status: String,
}

static PLUGIN_EXTENSIONS_STATE: OnceLock<Mutex<PluginExtensionsSnapshot>> = OnceLock::new();

fn state() -> &'static Mutex<PluginExtensionsSnapshot> {
    PLUGIN_EXTENSIONS_STATE.get_or_init(|| Mutex::new(PluginExtensionsSnapshot::default()))
}

fn snapshot() -> PluginExtensionsSnapshot {
    state().lock().map(|state| state.clone()).unwrap_or_default()
}

fn refresh(cx: &mut Context<MusicApp>) {
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.loading || state.operation_in_flight {
            return;
        }
        state.loading = true;
    }

    let task = Tokio::spawn_result(cx, async move {
        tokio::task::spawn_blocking(|| -> Result<_> {
            let mut commands = Vec::new();
            for surface in [
                PluginCommandSurface::CommandPalette,
                PluginCommandSurface::TrackContext,
                PluginCommandSurface::PlaylistContext,
                PluginCommandSurface::PageLocal,
            ] {
                for command in extensions::commands(surface)? {
                    if !commands
                        .iter()
                        .any(|existing: &PluginCommandSummary| existing.qualified_id == command.qualified_id)
                    {
                        commands.push(command);
                    }
                }
            }
            commands.sort_by(|left, right| {
                left.title
                    .cmp(&right.title)
                    .then_with(|| left.qualified_id.cmp(&right.qualified_id))
            });
            Ok((commands, extensions::home_sections()?, extensions::themes()?))
        })
        .await
        .map_err(|_| anyhow!("插件扩展快照任务异常退出"))?
    });

    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if let Ok(mut state) = state().lock() {
                state.loading = false;
                state.loaded = true;
                match result {
                    Ok((commands, home_sections, themes)) => {
                        state.commands = Arc::new(commands);
                        state.home_sections = Arc::new(home_sections);
                        state.themes = Arc::new(themes);
                        state.status = "插件扩展快照已同步".into();
                    }
                    Err(error) => state.status = format!("插件扩展快照读取失败：{error:#}"),
                }
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn invoke_palette_command(qualified_id: String, cx: &mut Context<MusicApp>) {
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.operation_in_flight {
            return;
        }
        state.operation_in_flight = true;
        state.status = format!("正在执行 Command {qualified_id}…");
    }

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
        this.update(cx, |_this, cx| {
            if let Ok(mut state) = state().lock() {
                state.operation_in_flight = false;
                state.status = match result {
                    Ok(result) => {
                        if let Some(toast) = result.toast {
                            toast
                        } else if let Some(open_page) = result.open_page {
                            format!(
                                "Command 执行完成；插件请求打开已验证页面 {}/{}",
                                open_page.plugin_id, open_page.page_id
                            )
                        } else {
                            "Command 执行完成".into()
                        }
                    }
                    Err(error) => format!("Command 执行失败：{error:#}"),
                };
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn load_home_section(qualified_id: String, cx: &mut Context<MusicApp>) {
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.operation_in_flight {
            return;
        }
        state.operation_in_flight = true;
        state.status = format!("正在加载 Home Section {qualified_id}…");
    }

    let task = Tokio::spawn_result(cx, async move {
        extensions::load_home_section(&qualified_id).await
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if let Ok(mut state) = state().lock() {
                state.operation_in_flight = false;
                state.status = match result {
                    Ok(snapshot) => format!(
                        "Home Section 页面已通过 Host 加载：{}/{} · rev {}",
                        snapshot.plugin_id, snapshot.page_id, snapshot.revision
                    ),
                    Err(error) => format!("Home Section 页面加载失败：{error:#}"),
                };
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn activate_theme(qualified_id: String, cx: &mut Context<MusicApp>) {
    let expected_generation = extensions::theme_registry_generation();
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.operation_in_flight {
            return;
        }
        state.operation_in_flight = true;
        state.status = format!("正在应用 Theme {qualified_id}…");
    }

    let task = Tokio::spawn_result(cx, async move {
        let snapshot = tokio::task::spawn_blocking(move || extensions::load_theme(&qualified_id))
            .await
            .map_err(|_| anyhow!("插件 Theme 加载任务异常退出"))??;
        Ok((expected_generation, snapshot))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if let Ok(mut state) = state().lock() {
                state.operation_in_flight = false;
                state.status = match result {
                    Ok((generation, snapshot)) => {
                        match plugin_theme::activate_snapshot(&snapshot, generation) {
                            Ok(selection) => format!(
                                "Theme {} 已应用到插件页面；Host 主界面主题保持不变",
                                selection.display_name
                            ),
                            Err(error) => format!("Theme 应用失败：{error}"),
                        }
                    }
                    Err(error) => format!("Theme 应用失败：{error:#}"),
                };
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn restore_host_theme(cx: &mut Context<MusicApp>) {
    plugin_theme::clear_active_theme();
    if let Ok(mut state) = state().lock() {
        state.status = "已恢复 Host Theme；插件页面不再使用插件 Theme".into();
    }
    cx.notify();
}

pub(super) fn render(_app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let current = snapshot();
    if !current.loaded && !current.loading && !current.operation_in_flight {
        refresh(cx);
    }
    let current = snapshot();
    let active_theme = plugin_theme::active_selection();

    let mut command_list = div().flex().flex_col().gap_2();
    if current.commands.is_empty() {
        command_list = command_list.child(empty_state("暂无 Command contribution"));
    } else {
        for command in current.commands.iter() {
            let can_invoke = command
                .surfaces
                .contains(&PluginCommandSurface::CommandPalette);
            let qualified = command.qualified_id.clone();
            let surfaces = command
                .surfaces
                .iter()
                .map(|surface| format!("{surface:?}"))
                .collect::<Vec<_>>()
                .join(" · ");
            command_list = command_list.child(
                div()
                    .id(SharedString::from(format!("plugin-command-{}", command.qualified_id)))
                    .p_3()
                    .rounded_lg()
                    .bg(theme::BG_CANVAS)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
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
                                    .child(command.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::TEXT_TERTIARY)
                                    .truncate()
                                    .child(format!("{} · {surfaces}", command.plugin_id)),
                            ),
                    )
                    .child_if(can_invoke, || {
                        div()
                            .id(SharedString::from(format!("invoke-{qualified}")))
                            .px_3()
                            .py_1p5()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(theme::accent_red_muted())
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::ACCENT_RED)
                            .hover(|style| style.opacity(0.86))
                            .active(|style| style.scale(0.98))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |_, _, _, cx| {
                                    invoke_palette_command(qualified.clone(), cx)
                                }),
                            )
                            .child("执行")
                    }),
            );
        }
    }

    let mut home = div().flex().flex_col().gap_2();
    if current.home_sections.is_empty() {
        home = home.child(empty_state("暂无 Home section contribution"));
    } else {
        for section in current.home_sections.iter() {
            let qualified = section.qualified_id.clone();
            home = home.child(
                div()
                    .id(SharedString::from(format!("plugin-home-{}", section.qualified_id)))
                    .p_3()
                    .rounded_lg()
                    .bg(theme::BG_CANVAS)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
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
                                    .child(section.title.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::TEXT_TERTIARY)
                                    .truncate()
                                    .child(format!(
                                        "{} · page {} · order {}",
                                        section.plugin_id, section.page_id, section.order
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("load-{qualified}")))
                            .px_3()
                            .py_1p5()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(theme::accent_red_muted())
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::ACCENT_RED)
                            .hover(|style| style.opacity(0.86))
                            .active(|style| style.scale(0.98))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |_, _, _, cx| {
                                    load_home_section(qualified.clone(), cx)
                                }),
                            )
                            .child("加载页面"),
                    ),
            );
        }
    }

    let mut themes = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .p_3()
                .rounded_lg()
                .bg(if active_theme.is_none() {
                    theme::accent_red_muted()
                } else {
                    theme::BG_CANVAS
                })
                .border_1()
                .border_color(theme::BORDER_CARD)
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
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child("Host Theme"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child("使用 YinQiDao 默认页面颜色；不会读取插件 Theme asset"),
                        ),
                )
                .child(
                    div()
                        .id("plugin-theme-host")
                        .px_3()
                        .py_1p5()
                        .rounded_lg()
                        .cursor_pointer()
                        .bg(theme::accent_red_muted())
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::ACCENT_RED)
                        .hover(|style| style.opacity(0.86))
                        .active(|style| style.scale(0.98))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|_, _, _, cx| restore_host_theme(cx)),
                        )
                        .child(if active_theme.is_none() { "当前" } else { "恢复" }),
                ),
        );

    if current.themes.is_empty() {
        themes = themes.child(empty_state("暂无 Theme contribution"));
    } else {
        for contributed_theme in current.themes.iter() {
            let qualified = contributed_theme.qualified_id.clone();
            let is_active = active_theme
                .as_ref()
                .is_some_and(|selection| selection.qualified_id == contributed_theme.qualified_id);
            themes = themes.child(
                div()
                    .id(SharedString::from(format!(
                        "plugin-theme-{}",
                        contributed_theme.qualified_id
                    )))
                    .p_3()
                    .rounded_lg()
                    .bg(if is_active {
                        theme::accent_red_muted()
                    } else {
                        theme::BG_CARD
                    })
                    .border_1()
                    .border_color(theme::BORDER_CARD)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
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
                                    .child(contributed_theme.display_name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::TEXT_TERTIARY)
                                    .truncate()
                                    .child(contributed_theme.qualified_id.clone()),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("apply-{qualified}")))
                            .px_3()
                            .py_1p5()
                            .rounded_lg()
                            .cursor_pointer()
                            .bg(theme::accent_red_muted())
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme::ACCENT_RED)
                            .hover(|style| style.opacity(0.86))
                            .active(|style| style.scale(0.98))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                cx.listener(move |_, _, _, cx| {
                                    activate_theme(qualified.clone(), cx)
                                }),
                            )
                            .child(if is_active { "已应用" } else { "应用" }),
                    ),
            );
        }
    }

    div()
        .size_full()
        .overflow_y_scroll()
        .bg(theme::BG_CANVAS)
        .px_8()
        .py_6()
        .child(
            div()
                .max_w(px(980.0))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_5()
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
                                .gap_1()
                                .child(
                                    div()
                                        .text_2xl()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(theme::TEXT_PRIMARY)
                                        .child("扩展贡献"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::TEXT_SECONDARY)
                                        .child("查看 Command、Home Section 与 Theme；所有插件调用和静态资源读取均经过 Host 边界。"),
                                ),
                        )
                        .child(
                            div()
                                .id("plugin-extensions-refresh")
                                .px_3()
                                .py_2()
                                .rounded_lg()
                                .cursor_pointer()
                                .bg(theme::BG_CARD)
                                .border_1()
                                .border_color(theme::BORDER_CARD)
                                .text_sm()
                                .text_color(theme::TEXT_SECONDARY)
                                .hover(|style| style.bg(theme::bg_hover()))
                                .active(|style| style.scale(0.98))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|_, _, _, cx| refresh(cx)),
                                )
                                .child("刷新"),
                        ),
                )
                .child(section_card("Commands", command_list))
                .child(section_card("Home Sections", home))
                .child(section_card("Themes", themes))
                .child(
                    div()
                        .text_xs()
                        .text_color(if current.status.contains("失败") {
                            theme::ACCENT_RED
                        } else {
                            theme::TEXT_TERTIARY
                        })
                        .child(if current.status.is_empty() {
                            "Command/Home 使用共享 guest-call budget；Theme 仅在显式选择时读取并解析，paint/layout 只消费 Host 预解析 palette。".to_string()
                        } else {
                            current.status
                        }),
                ),
        )
        .into_any_element()
}

fn section_card(title: &'static str, content: impl IntoElement) -> gpui::AnyElement {
    div()
        .p_4()
        .rounded_xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .text_base()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_PRIMARY)
                .child(title),
        )
        .child(content)
        .into_any_element()
}

fn empty_state(message: &'static str) -> gpui::AnyElement {
    div()
        .px_3()
        .py_2()
        .text_sm()
        .text_color(theme::TEXT_TERTIARY)
        .child(message)
        .into_any_element()
}
