use gpui::{AnyElement, Context, IntoElement, SharedString, div, hsla, prelude::*, px};
use lucide_gpui::icon;

use crate::plugin::management::{PluginImportCandidate, PluginInstallStatus};
use crate::ui::{plugin_settings, shell::MusicApp, theme};

#[derive(Clone, Debug)]
pub enum ContextMenuAction {
    ViewList,
    Play,
    PlayNext,
    AddToQueue,
    DownloadAll,
}

#[derive(Clone, Debug)]
pub enum ContextMenuTarget {
    DailyRecommendations,
    OnlinePlaylist {
        route: crate::plugin::abi::PluginRoute,
        collection: crate::plugin::abi::MediaCollectionRef,
        title: String,
        subtitle: String,
        cover_url: Option<String>,
    },
    OnlineTrack {
        route: crate::plugin::abi::PluginRoute,
        track: crate::plugin::abi::RemoteTrack,
    },
}

#[derive(Clone, Debug)]
pub struct ContextMenuItem {
    pub label: String,
    pub icon: &'static str,
    pub action: ContextMenuAction,
}

#[derive(Clone, Debug)]
pub struct ContextMenuData {
    pub position: gpui::Point<gpui::Pixels>,
    pub title: String,
    pub items: Vec<ContextMenuItem>,
    pub target: ContextMenuTarget,
}

#[derive(Clone, Debug)]
pub enum GlobalModal {
    PluginImport(Box<PluginImportCandidate>),
    ServiceAuth,
    ContextMenu(Box<ContextMenuData>),
}

pub fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> Option<AnyElement> {
    match app.active_modal.as_ref() {
        Some(GlobalModal::PluginImport(candidate)) => {
            Some(render_plugin_import_modal(candidate, cx))
        }
        Some(GlobalModal::ServiceAuth) => Some(render_service_auth_modal(cx)),
        Some(GlobalModal::ContextMenu(data)) => Some(render_context_menu_modal(data, cx)),
        None => None,
    }
}

fn render_service_auth_modal(cx: &mut Context<MusicApp>) -> AnyElement {
    div()
        .id("global-modal-backdrop")
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.50))
        .flex()
        .items_center()
        .justify_center()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|app, _, _, cx| {
                app.close_modal(cx);
                crate::ui::settings::cancel_service_auth_if_active(cx);
            }),
        )
        .child(
            div()
                .id("service-auth-modal-card")
                .occlude()
                .w(px(540.0))
                .max_w_full()
                .max_h(px(720.0))
                .overflow_y_scroll()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .rounded_2xl()
                .shadow_lg()
                .p_6()
                .flex()
                .flex_col()
                .gap_4()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child("服务账号登录与认证"),
                        )
                        .child(
                            div()
                                .id("service-auth-modal-close")
                                .cursor_pointer()
                                .p_1()
                                .rounded_full()
                                .hover(|s| s.bg(theme::bg_hover()))
                                .child(theme::themed_icon(
                                    icon!(x),
                                    16.0,
                                    theme::TEXT_SECONDARY.into(),
                                ))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|app, _, _, cx| {
                                        app.close_modal(cx);
                                        crate::ui::settings::cancel_service_auth_if_active(cx);
                                    }),
                                ),
                        ),
                )
                .child(crate::ui::settings::render_service_auth_modal_content(cx)),
        )
        .into_any_element()
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn render_plugin_import_modal(
    candidate: &PluginImportCandidate,
    cx: &mut Context<MusicApp>,
) -> AnyElement {
    let candidate_for_confirm = candidate.clone();

    let (
        badge_text,
        badge_bg,
        badge_color,
        banner_bg,
        banner_border,
        banner_text,
        confirm_button_text,
    ) = match &candidate.install_status {
        PluginInstallStatus::NewInstall => (
            "新插件",
            hsla(140.0, 0.45, 0.45, 0.12),
            hsla(140.0, 0.45, 0.36, 1.0),
            hsla(140.0, 0.45, 0.45, 0.08),
            hsla(140.0, 0.45, 0.45, 0.20),
            "该插件尚未安装，确认导入后将安全复制到插件库并加载注册。".to_string(),
            "确认导入",
        ),
        PluginInstallStatus::Upgrade { current_version } => (
            "版本升级",
            hsla(210.0, 0.65, 0.50, 0.12),
            hsla(210.0, 0.75, 0.45, 1.0),
            hsla(210.0, 0.65, 0.50, 0.08),
            hsla(210.0, 0.65, 0.50, 0.20),
            format!(
                "当前已安装旧版本 v{current_version}，确认后将升级至 v{} 并更新组件与扩展。",
                candidate.version
            ),
            "确认升级",
        ),
        PluginInstallStatus::SameVersion { current_version } => (
            "覆盖重载",
            hsla(40.0, 0.85, 0.50, 0.12),
            hsla(35.0, 0.90, 0.35, 1.0),
            hsla(40.0, 0.85, 0.50, 0.08),
            hsla(40.0, 0.85, 0.50, 0.20),
            format!(
                "已安装相同版本 v{current_version}。确认后将使用新文件覆盖现有插件并热重载运行时。"
            ),
            "确认覆盖",
        ),
        PluginInstallStatus::Downgrade { current_version } => (
            "降级警告",
            hsla(0.0, 0.65, 0.50, 0.12),
            theme::ACCENT_RED.into(),
            hsla(0.0, 0.65, 0.50, 0.08),
            hsla(0.0, 0.65, 0.50, 0.20),
            format!(
                "警告：当前已安装更高版本 v{current_version}，导入将降级至 v{}，部分已有配置可能不兼容！",
                candidate.version
            ),
            "仍然降级导入",
        ),
    };

    let mut providers_section = div().flex().flex_col().gap_2();
    if !candidate.providers.is_empty() {
        providers_section = providers_section.child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::TEXT_SECONDARY)
                .child("提供者与服务能力"),
        );
        for provider in &candidate.providers {
            let mut caps_row = div().flex().flex_wrap().gap_1();
            for cap in &provider.capabilities {
                caps_row = caps_row.child(
                    div()
                        .px_2()
                        .py(px(2.0))
                        .rounded_md()
                        .bg(theme::bg_pill())
                        .text_xs()
                        .text_color(theme::TEXT_PRIMARY)
                        .child(cap.clone()),
                );
            }
            for auth in &provider.auth_methods {
                caps_row = caps_row.child(
                    div()
                        .px_2()
                        .py(px(2.0))
                        .rounded_md()
                        .bg(hsla(210.0, 0.50, 0.50, 0.10))
                        .text_xs()
                        .text_color(hsla(210.0, 0.70, 0.40, 1.0))
                        .child(auth.clone()),
                );
            }
            providers_section = providers_section.child(
                div()
                    .p_3()
                    .rounded_xl()
                    .bg(theme::BG_CANVAS)
                    .border_1()
                    .border_color(theme::BORDER_CARD)
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme::TEXT_PRIMARY)
                            .child(format!("{} ({})", provider.display_name, provider.id)),
                    )
                    .child(caps_row),
            );
        }
    }

    let network_section = if !candidate.network_domains.is_empty() {
        let mut domains_row = div().flex().flex_wrap().gap_1();
        for domain in &candidate.network_domains {
            domains_row = domains_row.child(
                div()
                    .px_2()
                    .py(px(2.0))
                    .rounded_md()
                    .bg(theme::BG_CANVAS)
                    .border_1()
                    .border_color(theme::BORDER_CARD)
                    .text_xs()
                    .text_color(theme::TEXT_SECONDARY)
                    .child(domain.clone()),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::TEXT_SECONDARY)
                    .child("网络访问权限 (Host 白名单放行域名)"),
            )
            .child(domains_row)
    } else {
        div().flex().items_center().gap_1().child(
            div()
                .text_xs()
                .text_color(theme::TEXT_TERTIARY)
                .child("该插件无外部网络访问权限（Host 沙盒完全离线隔离）"),
        )
    };

    div()
        .id("global-modal-overlay")
        .absolute()
        .inset_0()
        .occlude()
        .bg(hsla(0.0, 0.0, 0.0, 0.50))
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|app, _, _, cx| {
                app.close_modal(cx);
                app.status = "已取消导入插件".into();
            }),
        )
        .child(
            div()
                .id("global-modal-card")
                .occlude()
                .w(px(580.0))
                .max_w_full()
                .max_h(px(660.0))
                .overflow_y_scroll()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .rounded_2xl()
                .shadow_lg()
                .p_6()
                .flex()
                .flex_col()
                .gap_4()
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .text_color(theme::TEXT_PRIMARY)
                                        .child("确认导入插件"),
                                )
                                .child(
                                    div()
                                        .px_2()
                                        .py(px(2.0))
                                        .rounded_full()
                                        .bg(badge_bg)
                                        .text_xs()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(badge_color)
                                        .child(badge_text),
                                ),
                        )
                        .child(
                            div()
                                .cursor_pointer()
                                .p_1()
                                .rounded_full()
                                .hover(|s| s.bg(theme::bg_hover()))
                                .child(theme::themed_icon(
                                    icon!(x),
                                    16.0,
                                    theme::TEXT_TERTIARY.into(),
                                ))
                                .on_mouse_down(
                                    gpui::MouseButton::Left,
                                    cx.listener(|app, _, _, cx| {
                                        app.close_modal(cx);
                                        app.status = "已取消导入插件".into();
                                    }),
                                ),
                        ),
                )
                .child(
                    div()
                        .p_3()
                        .rounded_xl()
                        .bg(banner_bg)
                        .border_1()
                        .border_color(banner_border)
                        .text_xs()
                        .text_color(badge_color)
                        .child(banner_text),
                )
                .child(
                    div()
                        .p_4()
                        .rounded_xl()
                        .bg(theme::BG_CANVAS)
                        .border_1()
                        .border_color(theme::BORDER_CARD)
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::TEXT_PRIMARY)
                                        .child(candidate.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::TEXT_SECONDARY)
                                        .child(format!("v{}", candidate.version)),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_SECONDARY)
                                .child(format!("插件 ID: {}", candidate.plugin_id)),
                        )
                        .children((!candidate.description.trim().is_empty()).then(|| {
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_SECONDARY)
                                .child(candidate.description.clone())
                        }))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child(format!(
                                    "组件文件: {} ({})",
                                    candidate.component_file,
                                    format_bytes(candidate.component_bytes)
                                )),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .truncate()
                                .child(format!("来源目录: {}", candidate.package_dir.display())),
                        ),
                )
                .children((!candidate.providers.is_empty()).then_some(providers_section))
                .child(network_section)
                .child(
                    div()
                        .pt_2()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_3()
                        .child(modal_secondary_button(
                            "plugin-import-cancel",
                            "取消",
                            false,
                            cx.listener(|app, _, _, cx| {
                                app.close_modal(cx);
                                app.status = "已取消导入插件".into();
                            }),
                        ))
                        .child(modal_primary_button(
                            "plugin-import-confirm",
                            confirm_button_text,
                            false,
                            cx.listener(move |app, _, _, cx| {
                                app.close_modal(cx);
                                plugin_settings::confirm_import(candidate_for_confirm.clone(), cx);
                            }),
                        )),
                ),
        )
        .into_any_element()
}

pub fn modal_primary_button<I, F>(
    id: I,
    label: &'static str,
    disabled: bool,
    on_press: F,
) -> AnyElement
where
    I: Into<gpui::ElementId>,
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let button = div()
        .id(id.into())
        .px_4()
        .py_2()
        .rounded_lg()
        .bg(theme::ACCENT_RED)
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::TEXT_WHITE)
        .child(label);
    if disabled {
        button.opacity(0.45).into_any_element()
    } else {
        button
            .cursor_pointer()
            .hover(|style| style.bg(theme::ACCENT_RED_HOVER))
            .active(|style| style.scale(0.98))
            .on_mouse_down(gpui::MouseButton::Left, on_press)
            .into_any_element()
    }
}

pub fn modal_secondary_button<I, F>(
    id: I,
    label: &'static str,
    disabled: bool,
    on_press: F,
) -> AnyElement
where
    I: Into<gpui::ElementId>,
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let button = div()
        .id(id.into())
        .px_4()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme::BORDER_CARD)
        .bg(theme::BG_CANVAS)
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

fn render_context_menu_modal(data: &ContextMenuData, cx: &mut Context<MusicApp>) -> AnyElement {
    let position = data.position;
    let data_clone = data.clone();

    div()
        .id("context-menu-backdrop")
        .absolute()
        .inset_0()
        .bg(hsla(0.0, 0.0, 0.0, 0.01))
        .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(|app, _, _, cx| {
                app.close_modal(cx);
            }),
        )
        .on_mouse_down(
            gpui::MouseButton::Right,
            cx.listener(|app, _, _, cx| {
                app.close_modal(cx);
            }),
        )
        .child(
            div()
                .id("context-menu-popup")
                .occlude()
                .absolute()
                .left(position.x)
                .top(position.y)
                .w(px(160.0))
                .bg(gpui::rgb(0xff_ff_ff))
                .border_1()
                .border_color(theme::BORDER_CARD)
                .rounded_xl()
                .shadow_lg()
                .p_1p5()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .children(data.items.iter().cloned().map(|item| {
                    let action = item.action.clone();
                    let target = data_clone.target.clone();
                    div()
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::bg_hover()))
                        .transition(theme::press_transition())
                        .child(theme::themed_icon(
                            item.icon,
                            14.0,
                            theme::TEXT_SECONDARY.into(),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme::TEXT_PRIMARY)
                                .child(item.label),
                        )
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(move |app, _, _, cx| {
                                app.close_modal(cx);
                                app.execute_context_menu_action(&action, &target, cx);
                            }),
                        )
                })),
        )
        .into_any_element()
}
