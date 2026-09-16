use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};
use gpui::{Context, IntoElement, div, hsla, prelude::*, px};
use gpui_tokio::Tokio;

use crate::{
    plugin::{
        abi::{AuthMethod, PluginCapability},
        accounts::{self, PluginAccountSessionStatus, PluginProviderSummary, PluginServiceSummary},
        extensions, frontend,
        logout::{PluginAccountLogoutOutcome, PluginLogoutAllResult},
    },
    ui::{MusicApp, theme},
};

#[derive(Clone, Debug, Default)]
struct ServiceAccountsSnapshot {
    loading: bool,
    loaded: bool,
    action_in_flight: bool,
    generation: u64,
    services: Arc<Vec<PluginServiceSummary>>,
    status: String,
    action_status: String,
}

static SERVICE_ACCOUNTS_STATE: OnceLock<Mutex<ServiceAccountsSnapshot>> = OnceLock::new();

fn state() -> &'static Mutex<ServiceAccountsSnapshot> {
    SERVICE_ACCOUNTS_STATE.get_or_init(|| Mutex::new(ServiceAccountsSnapshot::default()))
}

fn snapshot_for_generation(generation: u64) -> (ServiceAccountsSnapshot, bool) {
    let Ok(mut state) = state().lock() else {
        return (ServiceAccountsSnapshot::default(), false);
    };
    if state.loaded && state.generation != generation {
        state.loaded = false;
        state.services = Arc::new(Vec::new());
        state.status = "插件已更新，正在刷新音乐服务账号状态…".into();
        state.action_status.clear();
    }
    let should_refresh = !state.loaded && !state.loading;
    (state.clone(), should_refresh)
}

fn begin_refresh(generation: u64) -> bool {
    let Ok(mut state) = state().lock() else {
        return false;
    };
    if state.loading || state.action_in_flight {
        return false;
    }
    state.loading = true;
    state.status = "正在读取音乐服务、账号与权限状态…".into();
    if state.generation != generation {
        state.loaded = false;
        state.services = Arc::new(Vec::new());
    }
    true
}

fn complete_refresh(generation: u64, result: Result<Vec<PluginServiceSummary>>) {
    let current_generation = extensions::theme_registry_generation();
    let Ok(mut state) = state().lock() else {
        return;
    };
    state.loading = false;
    if current_generation != generation {
        state.loaded = false;
        state.generation = current_generation;
        state.services = Arc::new(Vec::new());
        state.status = "插件在读取账号状态期间已更新，旧快照已丢弃".into();
        return;
    }

    state.generation = generation;
    state.loaded = true;
    match result {
        Ok(services) => {
            let provider_count = services
                .iter()
                .map(|service| service.providers.len())
                .sum::<usize>();
            let account_count = services
                .iter()
                .flat_map(|service| service.providers.iter())
                .map(|provider| provider.accounts.len())
                .sum::<usize>();
            state.services = Arc::new(services);
            state.status = format!(
                "已同步 {} 个服务插件 · {provider_count} 个 Provider · {account_count} 个账号",
                state.services.len()
            );
        }
        Err(error) => {
            state.status = format!("读取音乐服务账号状态失败：{error:#}");
        }
    }
}

fn refresh_services(cx: &mut Context<MusicApp>) {
    let generation = extensions::theme_registry_generation();
    if !begin_refresh(generation) {
        return;
    }
    cx.notify();
    let task = Tokio::spawn_result(cx, async move {
        tokio::task::spawn_blocking(accounts::service_summaries)
            .await
            .map_err(|_| anyhow!("音乐服务账号快照任务异常退出"))?
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            complete_refresh(generation, result);
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn begin_action(generation: u64, message: &str) -> bool {
    let current_generation = extensions::theme_registry_generation();
    let Ok(mut state) = state().lock() else {
        return false;
    };
    if current_generation != generation {
        state.loaded = false;
        state.generation = current_generation;
        state.services = Arc::new(Vec::new());
        state.action_status = "插件已更新，请刷新账号状态后重试".into();
        return false;
    }
    if state.loading || state.action_in_flight {
        return false;
    }
    state.action_in_flight = true;
    state.action_status = message.into();
    true
}

fn complete_action(generation: u64, result: Result<String>) -> bool {
    let current_generation = extensions::theme_registry_generation();
    let Ok(mut state) = state().lock() else {
        return false;
    };
    state.action_in_flight = false;
    state.loaded = false;
    state.services = Arc::new(Vec::new());
    if current_generation != generation {
        state.generation = current_generation;
        state.action_status = "插件在账号操作期间已更新，旧操作结果未复用".into();
        return true;
    }
    state.generation = generation;
    state.action_status = match result {
        Ok(status) => status,
        Err(error) => format!("账号操作失败：{error:#}"),
    };
    true
}

fn logout_account_action(
    plugin_id: String,
    provider_id: String,
    account_id: String,
    generation: u64,
    cx: &mut Context<MusicApp>,
) {
    if !begin_action(generation, "正在退出账号并撤销 Host 凭据…") {
        cx.notify();
        return;
    }
    cx.notify();
    let task = Tokio::spawn_result(cx, async move {
        let frontend = frontend::global().ok_or_else(|| anyhow!("插件服务前端尚未初始化"))?;
        let outcome = frontend
            .logout_account_by_id(&plugin_id, &provider_id, &account_id)
            .await?;
        Ok::<_, anyhow::Error>(summarize_account_logout(&outcome))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if complete_action(generation, result) {
                refresh_services(cx);
            } else {
                cx.notify();
            }
        })?;
        Ok(())
    })
    .detach();
}

fn logout_provider_action(
    plugin_id: String,
    provider_id: String,
    generation: u64,
    cx: &mut Context<MusicApp>,
) {
    if !begin_action(generation, "正在退出该 Provider 的全部账号并撤销凭据…") {
        cx.notify();
        return;
    }
    cx.notify();
    let task = Tokio::spawn_result(cx, async move {
        let frontend = frontend::global().ok_or_else(|| anyhow!("插件服务前端尚未初始化"))?;
        let result = frontend.logout_provider_all(&plugin_id, &provider_id).await?;
        Ok::<_, anyhow::Error>(summarize_bulk_logout("Provider", &result))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if complete_action(generation, result) {
                refresh_services(cx);
            } else {
                cx.notify();
            }
        })?;
        Ok(())
    })
    .detach();
}

fn logout_plugin_action(plugin_id: String, generation: u64, cx: &mut Context<MusicApp>) {
    if !begin_action(generation, "正在退出该插件的全部账号并撤销凭据…") {
        cx.notify();
        return;
    }
    cx.notify();
    let task = Tokio::spawn_result(cx, async move {
        let frontend = frontend::global().ok_or_else(|| anyhow!("插件服务前端尚未初始化"))?;
        let result = frontend.logout_plugin_all(&plugin_id).await?;
        Ok::<_, anyhow::Error>(summarize_bulk_logout("插件", &result))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            if complete_action(generation, result) {
                refresh_services(cx);
            } else {
                cx.notify();
            }
        })?;
        Ok(())
    })
    .detach();
}

fn summarize_account_logout(outcome: &PluginAccountLogoutOutcome) -> String {
    if outcome.operation_error.is_some() {
        return format!(
            "账号退出未完整完成：本地会话操作失败；Host 已撤销 {} 项账号凭据",
            outcome.secrets_revoked
        );
    }
    if outcome.secret_cleanup_error.is_some() {
        return "账号已退出，但 Host Secret 清理失败；请重试清理凭据".into();
    }
    if !outcome.remote_acknowledged || outcome.remote_error.is_some() {
        return format!(
            "账号已从本地路由退出并撤销 {} 项 Host 凭据；远端注销未确认",
            outcome.secrets_revoked
        );
    }
    format!(
        "账号已退出，远端已确认，并撤销 {} 项 Host 凭据",
        outcome.secrets_revoked
    )
}

fn summarize_bulk_logout(scope: &str, result: &PluginLogoutAllResult) -> String {
    let local_failures = result.local_failures();
    let remote_failures = result.remote_failures();
    let secret_failures = result.secret_cleanup_failures();
    let account_count = result.accounts.len();
    if local_failures != 0 || secret_failures != 0 {
        return format!(
            "{scope} 批量退出完成但存在异常：{account_count} 个账号 · 本地失败 {local_failures} · 凭据清理失败 {secret_failures}"
        );
    }
    if remote_failures != 0 {
        return format!(
            "{scope} 的 {account_count} 个账号已完成本地退出与凭据清理；{remote_failures} 个远端注销未确认"
        );
    }
    format!(
        "{scope} 的 {account_count} 个账号已安全退出；额外撤销 {} 项残余 Host 凭据",
        result.secrets_revoked
    )
}

pub(super) fn render(cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let generation = extensions::theme_registry_generation();
    let (initial, should_refresh) = snapshot_for_generation(generation);
    if should_refresh {
        refresh_services(cx);
    }
    let current = state()
        .lock()
        .map(|state| state.clone())
        .unwrap_or(initial);
    let interaction_busy = current.loading || current.action_in_flight;

    let mut services = div().flex().flex_col().gap_4();
    if current.services.is_empty() {
        services = services.child(
            div()
                .p_5()
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
                        .child(if current.loading {
                            "正在读取音乐服务…"
                        } else if current.action_in_flight {
                            "正在处理账号操作…"
                        } else {
                            "当前没有可显示的音乐服务插件"
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(if interaction_busy {
                            "Host 正在后台执行账号/凭据操作，不会在 render 路径执行插件代码。"
                        } else {
                            "安装带 Provider 的插件后，可在此查看账号会话、能力和 Host 权限授权状态。"
                        }),
                ),
        );
    } else {
        for service in current.services.iter().cloned() {
            services = services.child(service_card(
                service,
                interaction_busy,
                generation,
                cx,
            ));
        }
    }

    let refresh_label = if current.action_in_flight {
        "操作中…"
    } else if current.loading {
        "刷新中…"
    } else {
        "刷新状态"
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
                                        .child("音乐服务与账号"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::TEXT_SECONDARY)
                                        .child("统一查看插件 Provider、账号会话、认证方式、能力与 Host 权限；Secret 不进入此页面。"),
                                ),
                        )
                        .child(refresh_button(
                            interaction_busy,
                            refresh_label,
                            cx.listener(|_, _, _, cx| refresh_services(cx)),
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(if status_is_error(&current.status) {
                            theme::ACCENT_RED
                        } else {
                            theme::TEXT_TERTIARY
                        })
                        .child(if current.status.is_empty() {
                            "账号状态来自 Host SessionCoordinator；PendingValidation 不会参与在线路由。".to_string()
                        } else {
                            current.status
                        }),
                )
                .child_if(!current.action_status.is_empty(), || {
                    div()
                        .p_3()
                        .rounded_lg()
                        .bg(theme::BG_CARD)
                        .border_1()
                        .border_color(theme::BORDER_HAIRLINE)
                        .text_xs()
                        .text_color(if status_is_error(&current.action_status) {
                            theme::ACCENT_RED
                        } else {
                            theme::TEXT_SECONDARY
                        })
                        .child(current.action_status.clone())
                })
                .child(services)
                .child(
                    div()
                        .p_4()
                        .rounded_xl()
                        .bg(theme::BG_CARD)
                        .border_1()
                        .border_color(theme::BORDER_CARD)
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child("本页只展示 Host 已验证的非 Secret 元数据。退出操作先使本地 Session 失效，再 best-effort 注销远端，最后由 Host 精确撤销账号、Provider 或插件 Secret namespace。"),
                ),
        )
        .into_any_element()
}

fn service_card(
    service: PluginServiceSummary,
    interaction_busy: bool,
    generation: u64,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let enabled = service.enabled;
    let provider_count = service.providers.len();
    let requested_domains = service.permissions.requested_network_domains.len();
    let granted_domains = service.permissions.granted_network_domains.len();
    let network_summary = if requested_domains == 0 {
        "网络域：未请求".to_string()
    } else {
        format!("网络域授权：{granted_domains}/{requested_domains}")
    };
    let playback_summary = if !service.permissions.playback_events_requested {
        "播放事件：未请求"
    } else if service.permissions.playback_events_granted {
        "播放事件：已授权"
    } else {
        "播放事件：未授权"
    };
    let plugin_id = service.plugin_id.clone();

    let mut providers = div().flex().flex_col().gap_3();
    if service.providers.is_empty() {
        providers = providers.child(
            div()
                .p_3()
                .rounded_lg()
                .bg(theme::BG_CANVAS)
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child("此插件没有声明音乐 Provider。"),
        );
    } else {
        for provider in service.providers {
            providers = providers.child(provider_card(
                &plugin_id,
                provider,
                interaction_busy,
                generation,
                cx,
            ));
        }
    }

    let plugin_id_for_logout = plugin_id.clone();
    div()
        .p_4()
        .rounded_xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::TEXT_PRIMARY)
                                        .child(service.name.clone()),
                                )
                                .child(enabled_badge(enabled)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_SECONDARY)
                                .child(format!("{} · v{}", plugin_id, service.version)),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child(format!("{provider_count} 个 Provider")),
                        )
                        .child(action_button(
                            "退出全部",
                            interaction_busy,
                            cx.listener(move |_, _, _, cx| {
                                logout_plugin_action(
                                    plugin_id_for_logout.clone(),
                                    generation,
                                    cx,
                                )
                            }),
                        )),
                ),
        )
        .child(
            div()
                .p_3()
                .rounded_lg()
                .bg(theme::BG_CANVAS)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::TEXT_SECONDARY)
                        .child("Host 权限"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(format!("{network_summary} · {playback_summary}")),
                )
                .child_if(!service.permissions.requested_network_domains.is_empty(), || {
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .truncate()
                        .child(format!(
                            "声明域名：{}",
                            service.permissions.requested_network_domains.join(", ")
                        ))
                })
                .child_if(!service.permissions.granted_network_domains.is_empty(), || {
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .truncate()
                        .child(format!(
                            "已授权：{}",
                            service.permissions.granted_network_domains.join(", ")
                        ))
                }),
        )
        .child(providers)
        .into_any_element()
}

fn provider_card(
    plugin_id: &str,
    provider: PluginProviderSummary,
    interaction_busy: bool,
    generation: u64,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let capability_text = capability_list(&provider.capabilities);
    let auth_text = auth_method_list(&provider.auth_methods);
    let account_count = provider.accounts.len();
    let provider_id = provider.provider_id.clone();
    let mut accounts = div().flex().flex_col().gap_2();
    if provider.accounts.is_empty() {
        accounts = accounts.child(
            div()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(theme::BG_CARD)
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child("尚无已登记账号；可使用 Provider 级清理撤销残余 Host 凭据。"),
        );
    } else {
        for account in provider.accounts {
            let account_capabilities = capability_list(&account.capabilities);
            let account_id = account.account_id.clone();
            let plugin_id_for_account = plugin_id.to_owned();
            let provider_id_for_account = provider_id.clone();
            let action_label = if account.state == PluginAccountSessionStatus::LoggedOut {
                "清理凭据"
            } else {
                "退出"
            };
            accounts = accounts.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(theme::BG_CARD)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(theme::TEXT_PRIMARY)
                                            .truncate()
                                            .child(account.display_name),
                                    )
                                    .child_if(account.is_default, || small_badge("默认")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme::TEXT_TERTIARY)
                                    .truncate()
                                    .child(format!(
                                        "优先级 {} · 账号能力：{}",
                                        account.priority, account_capabilities
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(session_badge(account.state))
                            .child(action_button(
                                action_label,
                                interaction_busy,
                                cx.listener(move |_, _, _, cx| {
                                    logout_account_action(
                                        plugin_id_for_account.clone(),
                                        provider_id_for_account.clone(),
                                        account_id.clone(),
                                        generation,
                                        cx,
                                    )
                                }),
                            )),
                    ),
            );
        }
    }

    let plugin_id_for_provider = plugin_id.to_owned();
    let provider_id_for_logout = provider_id.clone();
    div()
        .p_3()
        .rounded_lg()
        .bg(theme::BG_CANVAS)
        .border_1()
        .border_color(theme::BORDER_HAIRLINE)
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child(provider.display_name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_SECONDARY)
                                .child(provider_id),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child(format!("{account_count} 个账号")),
                        )
                        .child(action_button(
                            "退出全部",
                            interaction_busy,
                            cx.listener(move |_, _, _, cx| {
                                logout_provider_action(
                                    plugin_id_for_provider.clone(),
                                    provider_id_for_logout.clone(),
                                    generation,
                                    cx,
                                )
                            }),
                        )),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child(format!("能力：{capability_text}")),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child(format!("认证方式：{auth_text}")),
        )
        .child(accounts)
        .into_any_element()
}

fn enabled_badge(enabled: bool) -> gpui::AnyElement {
    div()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .bg(if enabled {
            hsla(140.0, 0.45, 0.45, 0.12)
        } else {
            hsla(0.0, 0.0, 0.0, 0.05)
        })
        .text_xs()
        .text_color(if enabled {
            hsla(140.0, 0.45, 0.36, 1.0)
        } else {
            theme::TEXT_TERTIARY
        })
        .child(if enabled { "已启用" } else { "已禁用" })
        .into_any_element()
}

fn session_badge(status: PluginAccountSessionStatus) -> gpui::AnyElement {
    let (label, background, text) = match status {
        PluginAccountSessionStatus::Authenticated => (
            "已认证",
            hsla(140.0, 0.45, 0.45, 0.12),
            hsla(140.0, 0.45, 0.36, 1.0),
        ),
        PluginAccountSessionStatus::PendingValidation => (
            "待验证",
            hsla(42.0, 0.75, 0.50, 0.14),
            hsla(38.0, 0.75, 0.36, 1.0),
        ),
        PluginAccountSessionStatus::Expired => (
            "已过期",
            hsla(8.0, 0.72, 0.52, 0.12),
            theme::ACCENT_RED,
        ),
        PluginAccountSessionStatus::LoggedOut => (
            "已退出",
            hsla(0.0, 0.0, 0.0, 0.05),
            theme::TEXT_TERTIARY,
        ),
    };
    div()
        .flex_none()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .bg(background)
        .text_xs()
        .text_color(text)
        .child(label)
        .into_any_element()
}

fn small_badge(label: &'static str) -> gpui::AnyElement {
    div()
        .px_2()
        .py(px(1.0))
        .rounded_full()
        .bg(theme::accent_red_muted())
        .text_xs()
        .text_color(theme::ACCENT_RED)
        .child(label)
        .into_any_element()
}

fn action_button<F>(label: &'static str, disabled: bool, on_press: F) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let button = div()
        .flex_none()
        .px_2()
        .py(px(4.0))
        .rounded_lg()
        .border_1()
        .border_color(theme::BORDER_HAIRLINE)
        .bg(theme::accent_red_muted())
        .text_xs()
        .text_color(theme::ACCENT_RED)
        .child(label);
    if disabled {
        button.opacity(0.40).into_any_element()
    } else {
        button
            .cursor_pointer()
            .hover(|style| style.bg(theme::bg_hover()))
            .active(|style| style.scale(0.98))
            .on_mouse_down(gpui::MouseButton::Left, on_press)
            .into_any_element()
    }
}

fn refresh_button<F>(disabled: bool, label: &'static str, on_press: F) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let button = div()
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme::BORDER_CARD)
        .bg(theme::BG_CARD)
        .text_sm()
        .text_color(theme::TEXT_PRIMARY)
        .child(label);
    if disabled {
        button.opacity(0.45).into_any_element()
    } else {
        button
            .cursor_pointer()
            .hover(|style| style.bg(theme::bg_hover()))
            .active(|style| style.scale(0.98))
            .on_mouse_down(gpui::MouseButton::Left, on_press)
            .into_any_element()
    }
}

fn status_is_error(status: &str) -> bool {
    status.contains("失败") || status.contains("异常") || status.contains("未确认") || status.contains("已丢弃")
}

fn capability_list(capabilities: &[PluginCapability]) -> String {
    if capabilities.is_empty() {
        return "无".into();
    }
    capabilities
        .iter()
        .map(|capability| match capability {
            PluginCapability::Authentication => "认证",
            PluginCapability::Search => "搜索",
            PluginCapability::Metadata => "元数据",
            PluginCapability::Lyrics => "歌词",
            PluginCapability::Artwork => "封面",
            PluginCapability::Streaming => "串流",
            PluginCapability::Playlists => "歌单",
            PluginCapability::MediaCollections => "媒体收藏",
            PluginCapability::CloudLibrary => "云曲库",
            PluginCapability::Recommendations => "推荐",
            PluginCapability::Recognition => "识别",
            PluginCapability::UserProfile => "用户资料",
            PluginCapability::LikeSync => "喜欢同步",
            PluginCapability::PlaybackEvents => "播放事件",
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn auth_method_list(methods: &[AuthMethod]) -> String {
    if methods.is_empty() {
        return "未声明".into();
    }
    methods
        .iter()
        .map(|method| match method {
            AuthMethod::QrCode => "二维码",
            AuthMethod::BrowserOAuth => "浏览器 OAuth",
            AuthMethod::DeviceCode => "设备码",
            AuthMethod::CookieImport => "Cookie 导入",
            AuthMethod::CustomForm => "Host 表单",
        })
        .collect::<Vec<_>>()
        .join(" · ")
}
