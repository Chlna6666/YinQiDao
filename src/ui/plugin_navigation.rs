use anyhow::{Result, bail};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};

use crate::plugin::management::{self, PluginRouteSummary};
use crate::plugin::ui::manifest::UiRoutePlacement;

use super::{route, shell::MusicApp, theme};

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

pub fn navigate(cx: &mut Context<MusicApp>, target: &PluginNavigationRoute) {
    route::navigate_path(cx, &target.pathname);
    cx.notify();
}

/// Temporary Host-owned route surface used until Component page-model exports are wired in.
/// It deliberately renders only validated Host metadata and never invokes guest code from paint.
pub fn render_route_shell(
    target: &PluginNavigationRoute,
    _app: &MusicApp,
    _cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
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
                                .child("插件页面已注册"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_SECONDARY)
                                .child(format!(
                                    "{} · page {} · {}",
                                    target.summary.plugin_id,
                                    target.summary.page_id,
                                    target.summary.qualified_id
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child("当前仅渲染 Host 已验证的路由壳层；声明式 UiPageModel 将由 Component runtime 异步获取并缓存，paint 阶段不会调用 WASM。"),
                        ),
                ),
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
    let pathname = target.pathname.clone();
    let label = target.summary.title.clone();
    let id = SharedString::from(format!("side-plugin-{}", target.summary.qualified_id));
    let text_color = if active {
        theme::ACCENT_RED
    } else {
        theme::TEXT_PRIMARY
    };

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
                if this.stage_open {
                    this.close_stage(cx);
                }
                route::navigate_path(cx, &pathname);
                cx.notify();
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
