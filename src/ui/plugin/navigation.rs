use std::rc::Rc;

use anyhow::{Result, bail};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use gpui_tokio::Tokio;

use crate::{
    model::AppPage,
    plugin::management::{self, PluginFieldValue, PluginRouteSummary},
    plugin::ui::manifest::UiRoutePlacement,
};

use super::{
    plugin_page_renderer::{self, PluginUiInteraction, PluginUiInteractionHandler},
    route,
    shell::MusicApp,
    theme,
};

const PLUGIN_ROUTE_PREFIX: &str = "/plugins/";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginNavigationRoute {
    pub summary: PluginRouteSummary,
    pub route_id: String,
    pub pathname: String,
}

pub fn sidebar_routes() -> Result<Vec<PluginNavigationRoute>> {
    management::sidebar_routes()?
        .into_iter()
        .map(navigation_route)
        .collect()
}

pub fn settings_routes() -> Result<Vec<PluginNavigationRoute>> {
    management::settings_routes()?
        .into_iter()
        .map(navigation_route)
        .collect()
}

pub fn resolve_path(pathname: &str) -> Result<Option<PluginNavigationRoute>> {
    let Some(rest) = pathname.strip_prefix(PLUGIN_ROUTE_PREFIX) else {
        return Ok(None);
    };
    let mut parts = rest.split('/');
    let Some(plugin_id) = parts.next().filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some(route_id) = parts.next().filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if parts.next().is_some() || !valid_path_id(plugin_id) || !valid_path_id(route_id) {
        return Ok(None);
    }

    let qualified_id = format!("plugin:{plugin_id}/{route_id}");
    let Some(summary) = management::route_summary(&qualified_id)? else {
        return Ok(None);
    };
    if summary.plugin_id != plugin_id || summary.placement == UiRoutePlacement::Hidden {
        return Ok(None);
    }
    navigation_route(summary).map(Some)
}

pub fn current(cx: &gpui::App) -> Result<Option<PluginNavigationRoute>> {
    resolve_path(&route::current_pathname(cx))
}

pub fn navigate(
    app: &mut MusicApp,
    cx: &mut Context<MusicApp>,
    target: &PluginNavigationRoute,
) {
    if app.stage_open {
        app.close_stage(cx);
    }
    app.page = AppPage::Settings;
    route::navigate_path(cx, &target.pathname);
    ensure_page_loaded(target, cx);
    cx.notify();
}

fn ensure_page_loaded(target: &PluginNavigationRoute, cx: &mut Context<MusicApp>) {
    let already_cached = management::page_snapshot(&target.summary.plugin_id, &target.summary.page_id)
        .ok()
        .flatten()
        .is_some();
    if already_cached || !management::ui_client_ready() {
        return;
    }

    let plugin_id = target.summary.plugin_id.clone();
    let page_id = target.summary.page_id.clone();
    let task = Tokio::spawn_result(cx, async move {
        management::load_page(&plugin_id, &page_id).await
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |this, cx| {
            match result {
                Ok(snapshot) => {
                    this.status = format!(
                        "插件页面已加载：{}/{} · rev {}",
                        snapshot.plugin_id, snapshot.page_id, snapshot.revision
                    );
                }
                Err(error) => this.status = format!("插件页面加载失败：{error:#}"),
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn interaction_handler(
    target: &PluginNavigationRoute,
    cx: &mut Context<MusicApp>,
) -> PluginUiInteractionHandler {
    let view = cx.weak_entity();
    let target = target.clone();
    Rc::new(move |interaction, _window, cx| {
        let target = target.clone();
        let _ = view.update(cx, |app, app_cx| {
            dispatch_interaction(app, app_cx, &target, interaction);
        });
    })
}

fn dispatch_interaction(
    app: &mut MusicApp,
    cx: &mut Context<MusicApp>,
    target: &PluginNavigationRoute,
    interaction: PluginUiInteraction,
) {
    if let PluginUiInteraction::BeginInput { .. } = interaction {
        // Keep text editing fail-closed until the Host reuses its IME/focus-aware input component.
        // Do not fall back to keydown scraping or expose secret field contents through status/logs.
        app.status = "插件文本输入正在等待 Host 输入组件接入".into();
        cx.notify();
        return;
    }

    let plugin_id = target.summary.plugin_id.clone();
    let page_id = target.summary.page_id.clone();
    app.status = format!("正在处理插件页面操作：{plugin_id}/{page_id}");
    cx.notify();

    let task = Tokio::spawn_result(cx, async move {
        match interaction {
            PluginUiInteraction::Action { action_id } => {
                management::dispatch_action(&plugin_id, &page_id, &action_id).await
            }
            PluginUiInteraction::SelectChanged { field_id, value } => {
                management::dispatch_field_changed(
                    &plugin_id,
                    &page_id,
                    &field_id,
                    PluginFieldValue::Text(value),
                )
                .await
            }
            PluginUiInteraction::ToggleChanged { field_id, value } => {
                management::dispatch_field_changed(
                    &plugin_id,
                    &page_id,
                    &field_id,
                    PluginFieldValue::Bool(value),
                )
                .await
            }
            PluginUiInteraction::BeginInput { .. } => unreachable!("handled before async dispatch"),
        }
    });

    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |this, cx| {
            match result {
                Ok(result) => {
                    this.status = result.toast.unwrap_or_else(|| {
                        format!(
                            "插件页面已更新：{}/{} · rev {}",
                            result.snapshot.plugin_id,
                            result.snapshot.page_id,
                            result.snapshot.revision
                        )
                    });
                    if result.close {
                        this.page = AppPage::Settings;
                        route::navigate_to(cx, route::AppRoute::Settings);
                    }
                }
                Err(error) => this.status = format!("插件页面操作失败：{error:#}"),
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

/// Render only Host-validated immutable snapshots. Loading/guest execution happens in the async
/// navigation controller above; this function never calls the plugin runtime from paint.
pub fn render_route_shell(
    target: &PluginNavigationRoute,
    _app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let snapshot = management::page_snapshot(&target.summary.plugin_id, &target.summary.page_id)
        .ok()
        .flatten();
    let runtime_ready = management::ui_client_ready();

    let body = if let Some(snapshot) = snapshot {
        let handler = runtime_ready.then(|| interaction_handler(target, cx));
        plugin_page_renderer::render_page(snapshot.model.as_ref(), handler)
    } else {
        div()
            .p_4()
            .rounded_xl()
            .bg(theme::BG_CARD)
            .border_1()
            .border_color(theme::BORDER_CARD)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::TEXT_PRIMARY)
                    .child(if runtime_ready {
                        "正在等待插件页面快照"
                    } else {
                        "插件 Component UI runtime 尚未就绪"
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::TEXT_TERTIARY)
                    .child("页面加载与 WASM 调用只在异步 controller 中发生；GPUI render/paint 不执行 guest。"),
            )
            .into_any_element()
    };

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
                .gap_4()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(theme::TEXT_PRIMARY)
                        .child(target.summary.title.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(format!(
                            "{} · page {}",
                            target.summary.plugin_id, target.summary.page_id
                        )),
                )
                .child(body),
        )
        .into_any_element()
}

fn navigation_route(summary: PluginRouteSummary) -> Result<PluginNavigationRoute> {
    let route_id = route_id_from_qualified(&summary.qualified_id, &summary.plugin_id)?;
    Ok(PluginNavigationRoute {
        pathname: format!("{PLUGIN_ROUTE_PREFIX}{}/{}", summary.plugin_id, route_id),
        summary,
        route_id,
    })
}

fn route_id_from_qualified(qualified: &str, plugin_id: &str) -> Result<String> {
    let prefix = format!("plugin:{plugin_id}/");
    let Some(route_id) = qualified.strip_prefix(&prefix) else {
        bail!("插件 route qualified id 与 plugin id 不一致: {qualified}");
    };
    if !valid_path_id(route_id) {
        bail!("插件 route id 不能安全映射到 pathname: {route_id:?}");
    }
    Ok(route_id.to_owned())
}

fn valid_path_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'_')
        })
}

pub fn sidebar_entry(
    target: PluginNavigationRoute,
    active: bool,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let label = target.summary.title.clone();
    let id = SharedString::from(format!("side-plugin-{}", target.summary.qualified_id));
    let text_color = if active {
        theme::ACCENT_RED
    } else {
        theme::TEXT_PRIMARY
    };
    let target_for_click = target.clone();

    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2p5()
        .px_2()
        .py_1p5()
        .rounded_lg()
        .cursor_pointer()
        .bg(if active {
            theme::accent_red_muted()
        } else {
            gpui::hsla(0.0, 0.0, 0.0, 0.0)
        })
        .hover(move |style| {
            style.bg(if active {
                theme::accent_red_muted()
            } else {
                theme::bg_hover()
            })
        })
        .active(|style| style.scale(0.98))
        .child(
            div()
                .w(px(3.0))
                .h(px(14.0))
                .rounded_full()
                .bg(if active {
                    theme::ACCENT_RED.into()
                } else {
                    gpui::hsla(0.0, 0.0, 0.0, 0.0)
                }),
        )
        .child(
            div()
                .text_sm()
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::NORMAL
                })
                .text_color(text_color)
                .truncate()
                .child(label),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                navigate(this, cx, &target_for_click);
            }),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_allowed_by_plugin_namespace_are_path_safe() {
        assert!(valid_path_id("netease.main_1"));
        assert!(!valid_path_id("NetEase"));
        assert!(!valid_path_id("../escape"));
        assert!(!valid_path_id("a/b"));
    }

    #[test]
    fn qualified_route_must_match_plugin_namespace() {
        assert_eq!(
            route_id_from_qualified("plugin:demo.music/home", "demo.music").expect("route"),
            "home"
        );
        assert!(route_id_from_qualified("plugin:other/home", "demo.music").is_err());
    }
}
