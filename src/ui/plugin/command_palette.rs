use std::sync::Arc;

use anyhow::Result;
use gpui::{
    Bounds, Context, IntoElement, Render, SharedString, WeakEntity, Window, WindowBounds,
    WindowOptions, div, prelude::*, px, size,
};
use gpui_tokio::Tokio;

use crate::plugin::{
    commands::{self, PluginCommandContext, PluginCommandOpenPage},
    extensions::{self, PluginCommandSummary, PluginCommandSurface},
};

use super::{plugin_navigation, shell::MusicApp, theme};

const PALETTE_WIDTH: f32 = 720.0;
const PALETTE_HEIGHT: f32 = 520.0;

pub(super) fn open(app: &mut MusicApp, cx: &mut Context<MusicApp>) {
    let commands = match extensions::commands(PluginCommandSurface::CommandPalette) {
        Ok(commands) => commands,
        Err(error) => {
            app.status = format!("读取插件 Command Palette 失败：{error:#}");
            cx.notify();
            return;
        }
    };
    let parent = cx.entity().downgrade();
    let bounds = Bounds::centered(None, size(px(PALETTE_WIDTH), px(PALETTE_HEIGHT)), cx);
    if let Err(error) = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        move |_, window_cx| {
            let commands = Arc::new(commands);
            window_cx.new(move |_| PluginCommandPalette::new(parent, commands))
        },
    ) {
        app.status = format!("打开插件 Command Palette 失败：{error:#}");
        cx.notify();
    }
}

struct PluginCommandPalette {
    parent: WeakEntity<MusicApp>,
    commands: Arc<Vec<PluginCommandSummary>>,
    invoking: Option<String>,
    status: String,
}

impl PluginCommandPalette {
    fn new(parent: WeakEntity<MusicApp>, commands: Arc<Vec<PluginCommandSummary>>) -> Self {
        Self {
            parent,
            commands,
            invoking: None,
            status: String::new(),
        }
    }

    fn invoke(&mut self, qualified_id: String, cx: &mut Context<Self>) {
        if self.invoking.is_some() {
            return;
        }
        self.invoking = Some(qualified_id.clone());
        self.status = format!("正在执行 {qualified_id}…");
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
        let parent = self.parent.clone();
        cx.spawn(async move |this, cx| -> Result<()> {
            let result = task.await;
            let (status, open_page) = match result {
                Ok(result) => {
                    let status = result.toast.unwrap_or_else(|| {
                        result.open_page.as_ref().map_or_else(
                            || "插件 Command 执行完成".to_string(),
                            |page| {
                                format!(
                                    "插件 Command 执行完成；请求打开 {}/{}",
                                    page.plugin_id, page.page_id
                                )
                            },
                        )
                    });
                    (status, result.open_page)
                }
                Err(error) => (format!("插件 Command 执行失败：{error:#}"), None),
            };

            this.update(cx, |this, cx| {
                this.invoking = None;
                this.status = status.clone();
                cx.notify();
            })?;

            let _ = parent.update(cx, |app, app_cx| {
                app.status = status;
                if let Some(page) = open_page
                    && let Some(target) = navigation_target(&page)
                {
                    plugin_navigation::navigate(app, app_cx, &target);
                }
                app_cx.notify();
            });
            Ok(())
        })
        .detach();
    }
}

impl Render for PluginCommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let invoking = self.invoking.clone();
        let mut command_list = div().flex().flex_col().gap_2();
        if self.commands.is_empty() {
            command_list = command_list.child(
                div()
                    .p_4()
                    .rounded_xl()
                    .bg(theme::BG_CARD)
                    .border_1()
                    .border_color(theme::BORDER_CARD)
                    .text_sm()
                    .text_color(theme::TEXT_TERTIARY)
                    .child("没有插件声明 CommandPalette surface"),
            );
        } else {
            for command in self.commands.iter() {
                let qualified_id = command.qualified_id.clone();
                let busy = invoking.is_some();
                let is_invoking = invoking.as_deref() == Some(command.qualified_id.as_str());
                command_list = command_list.child(
                    div()
                        .id(SharedString::from(format!(
                            "palette-command-{}",
                            command.qualified_id
                        )))
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
                                        .child(format!(
                                            "{} · {}",
                                            command.plugin_id, command.qualified_id
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("palette-run-{qualified_id}")))
                                .px_3()
                                .py_1p5()
                                .rounded_lg()
                                .bg(theme::accent_red_muted())
                                .text_xs()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::ACCENT_RED)
                                .when(!busy, |element| {
                                    element
                                        .cursor_pointer()
                                        .hover(|style| style.opacity(0.86))
                                        .active(|style| style.scale(0.98))
                                        .on_mouse_down(
                                            gpui::MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.invoke(qualified_id.clone(), cx)
                                            }),
                                        )
                                })
                                .child(if is_invoking { "执行中" } else { "执行" }),
                        ),
                );
            }
        }

        div()
            .size_full()
            .bg(theme::BG_CANVAS)
            .text_color(theme::TEXT_PRIMARY)
            .flex()
            .flex_col()
            .child(
                div()
                    .px_6()
                    .pt_6()
                    .pb_4()
                    .border_b_1()
                    .border_color(theme::BORDER_HAIRLINE)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("插件 Command Palette"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme::TEXT_TERTIARY)
                            .child("仅显示声明 CommandPalette surface 的已启用插件命令；执行受 Host budget、权限和生命周期校验约束。"),
                    ),
            )
            .child(
                div()
                    .id("plugin-command-palette-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px_6()
                    .py_4()
                    .child(command_list),
            )
            .child(
                div()
                    .min_h(px(42.0))
                    .px_6()
                    .py_3()
                    .border_t_1()
                    .border_color(theme::BORDER_HAIRLINE)
                    .text_xs()
                    .text_color(if self.status.contains("失败") {
                        theme::ACCENT_RED
                    } else {
                        theme::TEXT_TERTIARY
                    })
                    .child(if self.status.is_empty() {
                        format!("{} 个可用插件命令", self.commands.len())
                    } else {
                        self.status.clone()
                    }),
            )
    }
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
