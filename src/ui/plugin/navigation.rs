use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use anyhow::{Result, bail};
use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};
use gpui_tokio::Tokio;

use crate::{
    model::AppPage,
    plugin::{
        commands::{self, PluginCommandContext, PluginCommandOpenPage},
        extensions::{self, PluginCommandSummary, PluginCommandSurface},
        management::{self, PluginFieldValue, PluginRouteSummary},
        ui::manifest::UiRoutePlacement,
    },
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

#[derive(Clone, Debug)]
struct PageLocalCommandCacheEntry {
    generation: u64,
    commands: Arc<[PluginCommandSummary]>,
}

thread_local! {
    static PAGE_LOCAL_COMMANDS: RefCell<HashMap<String, PageLocalCommandCacheEntry>> =
        RefCell::new(HashMap::new());
    static PAGE_LOCAL_LOADING: RefCell<HashSet<(String, u64)>> = RefCell::new(HashSet::new());
    static PAGE_LOCAL_IN_FLIGHT: RefCell<Option<String>> = const { RefCell::new(None) };
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
    ensure_page_local_commands(target, cx);
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

fn page_local_commands(plugin_id: &str) -> Option<Arc<[PluginCommandSummary]>> {
    let generation = extensions::theme_registry_generation();
    PAGE_LOCAL_COMMANDS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache
            .get(plugin_id)
            .is_some_and(|entry| entry.generation != generation)
        {
            cache.remove(plugin_id);
        }
        cache
            .get(plugin_id)
            .map(|entry| entry.commands.clone())
    })
}

/// Schedule a Host-only contribution snapshot refresh after the current render pass.
///
/// The render path performs only an atomic registry-generation load and a thread-local cache read.
/// Registry locking happens in the deferred controller callback below; guest/WASM code is never
/// executed while building the PageLocal command surface.
fn ensure_page_local_commands(target: &PluginNavigationRoute, cx: &mut Context<MusicApp>) {
    if page_local_commands(&target.summary.plugin_id).is_some() {
        return;
    }

    let plugin_id = target.summary.plugin_id.clone();
    let generation = extensions::theme_registry_generation();
    let loading_key = (plugin_id.clone(), generation);
    let should_schedule = PAGE_LOCAL_LOADING.with(|loading| {
        let mut loading = loading.borrow_mut();
        if loading.contains(&loading_key) {
            false
        } else {
            loading.insert(loading_key.clone());
            true
        }
    });
    if !should_schedule {
        return;
    }

    let view = cx.weak_entity();
    cx.defer(move |cx| {
        let result = extensions::commands(PluginCommandSurface::PageLocal).map(|commands| {
            commands
                .into_iter()
                .filter(|command| command.plugin_id == plugin_id)
                .collect::<Vec<_>>()
        });
        let current_generation = extensions::theme_registry_generation();
        PAGE_LOCAL_LOADING.with(|loading| {
            loading.borrow_mut().remove(&loading_key);
        });

        let mut error_status = None;
        if current_generation == generation {
            match result {
                Ok(commands) => PAGE_LOCAL_COMMANDS.with(|cache| {
                    cache.borrow_mut().insert(
                        plugin_id.clone(),
                        PageLocalCommandCacheEntry {
                            generation,
                            commands: commands.into(),
                        },
                    );
                }),
                Err(error) => {
                    error_status = Some(format!("读取插件 PageLocal 命令失败：{error:#}"));
                }
            }
        }

        let _ = view.update(cx, |app, app_cx| {
            if let Some(status) = error_status {
                app.status = status;
            }
            app_cx.notify();
        });
    });
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

fn invoke_page_local_command(
    app: &mut MusicApp,
    cx: &mut Context<MusicApp>,
    target: PluginNavigationRoute,
    qualified_id: String,
) {
    let allowed = page_local_commands(&target.summary.plugin_id).is_some_and(|commands| {
        commands
            .iter()
            .any(|command| command.qualified_id == qualified_id)
    });
    if !allowed {
        app.status = "PageLocal Command 快照已失效，请重试".into();
        ensure_page_local_commands(&target, cx);
        cx.notify();
        return;
    }

    let acquired = PAGE_LOCAL_IN_FLIGHT.with(|in_flight| {
        let mut in_flight = in_flight.borrow_mut();
        if in_flight.is_some() {
            false
        } else {
            *in_flight = Some(qualified_id.clone());
            true
        }
    });
    if !acquired {
        return;
    }

    let page_id = target.summary.page_id.clone();
    let task_id = qualified_id.clone();
    app.status = format!("正在执行页面命令：{qualified_id}");
    cx.notify();

    let task = Tokio::spawn_result(cx, async move {
        commands::invoke_command(
            &task_id,
            PluginCommandContext {
                surface: PluginCommandSurface::PageLocal,
                page_id: Some(page_id),
                track: None,
                playlist: None,
            },
        )
        .await
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |app, app_cx| {
            PAGE_LOCAL_IN_FLIGHT.with(|in_flight| {
                in_flight.borrow_mut().take();
            });
            match result {
                Ok(result) => apply_page_local_result(app, app_cx, &qualified_id, result),
                Err(error) => {
                    app.status = format!("页面命令执行失败：{error:#}");
                    app_cx.notify();
                }
            }
        })?;
        Ok(())
    })
    .detach();
}

fn apply_page_local_result(
    app: &mut MusicApp,
    cx: &mut Context<MusicApp>,
    qualified_id: &str,
    result: commands::PluginCommandResult,
) {
    let mut denied_page = None;
    if let Some(open_page) = result.open_page {
        if let Some(target) = command_navigation_target(&open_page) {
            navigate(app, cx, &target);
        } else {
            denied_page = Some(open_page.qualified_id);
        }
    }

    app.status = if let Some(toast) = result.toast {
        toast
    } else if let Some(qualified_page) = denied_page {
        format!("页面命令已执行，但目标页面当前不可见：{qualified_page}")
    } else {
        format!("页面命令已执行：{qualified_id}")
    };
    cx.notify();
}

fn command_navigation_target(open_page: &PluginCommandOpenPage) -> Option<PluginNavigationRoute> {
    sidebar_routes()
        .unwrap_or_default()
        .into_iter()
        .chain(settings_routes().unwrap_or_default())
        .find(|target| {
            target.summary.plugin_id == open_page.plugin_id
                && target.summary.page_id == open_page.page_id
        })
}

fn render_page_local_commands(
    target: &PluginNavigationRoute,
    commands: Arc<[PluginCommandSummary]>,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let invoking = PAGE_LOCAL_IN_FLIGHT.with(|in_flight| in_flight.borrow().clone());
    let busy = invoking.is_some();
    let mut row = div().flex().flex_wrap().items_center().gap_2();

    for command in commands.iter() {
        let qualified_id = command.qualified_id.clone();
        let is_invoking = invoking.as_deref() == Some(command.qualified_id.as_str());
        let target = target.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!(
                    "page-local-command-{}",
                    command.qualified_id
                )))
                .px_3()
                .py_1p5()
                .rounded_lg()
                .bg(theme::accent_red_muted())
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::ACCENT_RED)
                .opacity(if busy && !is_invoking { 0.48 } else { 1.0 })
                .when(!busy, |element| {
                    element
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.86))
                        .active(|style| style.scale(0.98))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |app, _, _, cx| {
                                invoke_page_local_command(
                                    app,
                                    cx,
                                    target.clone(),
                                    qualified_id.clone(),
                                );
                            }),
                        )
                })
                .child(if is_invoking {
                    format!("{} · 执行中", command.title)
                } else {
                    command.title.clone()
                }),
        );
    }

    div()
        .w_full()
        .p_3()
        .rounded_xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_SECONDARY)
                .child("页面命令"),
        )
        .child(row)
        .into_any_element()
}

/// Render only Host-validated immutable snapshots. Loading/guest execution happens in the async
/// navigation controller above; this function never calls the plugin runtime from paint.
pub fn render_route_shell(
    target: &PluginNavigationRoute,
    _app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    ensure_page_local_commands(target, cx);
    let local_commands = page_local_commands(&target.summary.plugin_id);
    let snapshot = management::page_snapshot(&target.summary.plugin_id, &target.summary.page_id)
        .ok()
        .flatten();
    let runtime_ready = management::ui_client_ready();

    let body = if let Some(snapshot) = snapshot {
        let handler = runtime_ready.then(|| interaction_handler(target, cx));
        plugin_page_renderer::render_plugin_page(
            &target.summary.plugin_id,
            snapshot.model.as_ref(),
            handler,
        )
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

    let mut content = div()
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
        );

    if let Some(commands) = local_commands.filter(|commands| !commands.is_empty()) {
        content = content.child(render_page_local_commands(target, commands, cx));
    }
    content = content.child(body);

    div()
        .size_full()
        .overflow_y_scroll()
        .bg(theme::BG_CANVAS)
        .px_8()
        .py_6()
        .child(content)
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
