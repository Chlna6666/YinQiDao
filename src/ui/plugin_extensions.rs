use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use gpui_tokio::Tokio;

use crate::plugin::extensions::{
    self, PluginCommandSummary, PluginCommandSurface, PluginHomeSectionSummary, PluginThemeSummary,
};

use super::{shell::MusicApp, theme};

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

fn validate_theme(qualified_id: String, cx: &mut Context<MusicApp>) {
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.operation_in_flight {
            return;
        }
        state.operation_in_flight = true;
        state.status = format!("正在校验 Theme {qualified_id}…");
    }

    let task = Tokio::spawn_result(cx, async move {
        tokio::task::spawn_blocking(move || extensions::load_theme(&qualified_id))
            .await
            .map_err(|_| anyhow!("插件 Theme 校验任务异常退出"))?
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if let Ok(mut state) = state().lock() {
                state.operation_in_flight = false;
                state.status = match result {
                    Ok(theme) => {
                        let semantic_colors = [
                            theme.background.as_ref(),
                            theme.surface.as_ref(),
                            theme.surface_elevated.as_ref(),
                            theme.text_primary.as_ref(),
                            theme.text_secondary.as_ref(),
                            theme.accent.as_ref(),
                            theme.border.as_ref(),
                            theme.success.as_ref(),
                            theme.warning.as_ref(),
                            theme.error.as_ref(),
                        ]
                        .into_iter()
                        .flatten()
                        .count();
                        format!(
                            "Theme {} 校验通过：{} 个颜色 token",
                            theme.display_name, semantic_colors
                        )
                    }
                    Err(error) => format!("Theme 校验失败：{error:#}"),
                };
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

pub(super) fn render(_app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let current = snapshot();
    if !current.loaded && !current.loading && !current.operation_in_flight {
        refresh(cx);
    }
    let current = snapshot();

    let mut commands = div().flex().flex_col().gap_2();
    if current.commands.is_empty() {
        commands = commands.child(empty_state("暂无 Command contribution"));
    } else {
        for command in current.commands.iter() {
            commands = commands.child(extension_row(
                SharedString::from(format!("plugin-command-{}", command.qualified_id)),
                command.title.clone(),
                format!("{} · {}", command.plugin_id, command.qualified_id),
            ));
        }
    }

    let mut home = div().flex().flex_col().gap_2();
    if current.home_sections.is_empty() {
        home = home.child(empty_state("暂无 Home section contribution"));
    } else {
        for section in current.home_sections.iter() {
            home = home.child(extension_row(
                SharedString::from(format!("plugin-home-{}", section.qualified_id)),
                section.title.clone(),
                format!("{} · order {}", section.plugin_id, section.order),
            ));
        }
    }

    let mut themes = div().flex().flex_col().gap_2();
    if current.themes.is_empty() {
        themes = themes.child(empty_state("暂无 Theme contribution"));
    } else {
        for plugin_theme in current.themes.iter().cloned() {
            let qualified = plugin_theme.qualified_id.clone();
            themes = themes.child(
                div()
                    .id(SharedString::from(format!("plugin-theme-{}", plugin_theme.qualified_id)))
                    .p_3()
                    .rounded_lg()
                    .bg(theme::BG_CARD)
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
                                    .child(plugin_theme.display_name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::TEXT_TERTIARY)
                                    .truncate()
                                    .child(plugin_theme.qualified_id),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("validate-{qualified}")))
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
                                cx.listener(move |_, _, _, cx| validate_theme(qualified.clone(), cx)),
                            )
                            .child("Host 校验"),
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
                                        .child("查看已注册的 Command、Home Section 与 Theme；所有数据来自 Host 已验证快照。"),
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
                .child(section_card("Commands", commands))
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
                            "Theme 文件只在选择/校验时由 Host 读取；paint 阶段不执行文件 I/O 或 guest code。".to_string()
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

fn extension_row(id: SharedString, title: String, detail: String) -> gpui::AnyElement {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_lg()
        .bg(theme::BG_CANVAS)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_PRIMARY)
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child(detail),
        )
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
