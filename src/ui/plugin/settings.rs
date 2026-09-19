use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, anyhow};
use gpui::{Context, IntoElement, SharedString, div, hsla, prelude::*, px};
use gpui_tokio::Tokio;

use crate::plugin::management::{self, PluginSummary};

use super::{shell::MusicApp, theme};

#[derive(Clone, Debug, Default)]
struct PluginSettingsSnapshot {
    loading: bool,
    operation_in_flight: bool,
    loaded: bool,
    plugins: Arc<Vec<PluginSummary>>,
    status: String,
    pending_uninstall: Option<String>,
}

static PLUGIN_SETTINGS_STATE: OnceLock<Mutex<PluginSettingsSnapshot>> = OnceLock::new();

fn state() -> &'static Mutex<PluginSettingsSnapshot> {
    PLUGIN_SETTINGS_STATE.get_or_init(|| Mutex::new(PluginSettingsSnapshot::default()))
}

fn snapshot() -> PluginSettingsSnapshot {
    state()
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default()
}

fn begin_refresh() -> bool {
    let Ok(mut state) = state().lock() else {
        return false;
    };
    if state.loading || state.operation_in_flight {
        return false;
    }
    state.loading = true;
    true
}

fn complete_refresh(result: Result<Vec<PluginSummary>>) {
    let Ok(mut state) = state().lock() else {
        return;
    };
    state.loading = false;
    state.loaded = true;
    match result {
        Ok(plugins) => {
            state.plugins = Arc::new(plugins);
            if state.status.is_empty() {
                state.status = "插件列表已同步".into();
            }
        }
        Err(error) => state.status = format!("读取插件列表失败：{error:#}"),
    }
}

fn refresh_plugins(cx: &mut Context<MusicApp>) {
    if !begin_refresh() {
        return;
    }
    let task = Tokio::spawn_result(cx, async move {
        tokio::task::spawn_blocking(management::list_installed)
            .await
            .map_err(|_| anyhow!("插件列表任务异常退出"))?
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            complete_refresh(result);
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn begin_operation(status: impl Into<String>) -> bool {
    let Ok(mut state) = state().lock() else {
        return false;
    };
    if state.operation_in_flight {
        return false;
    }
    state.operation_in_flight = true;
    state.loading = false;
    state.status = status.into();
    true
}

fn complete_operation(result: Result<(String, Vec<PluginSummary>)>) {
    let Ok(mut state) = state().lock() else {
        return;
    };
    state.operation_in_flight = false;
    state.loaded = true;
    state.pending_uninstall = None;
    match result {
        Ok((status, plugins)) => {
            state.status = status;
            state.plugins = Arc::new(plugins);
        }
        Err(error) => state.status = format!("插件操作失败：{error:#}"),
    }
}

fn import_plugin(cx: &mut Context<MusicApp>) {
    if !begin_operation("等待选择插件文件…") {
        return;
    }
    let task = Tokio::spawn_result(cx, async move {
        let picked = rfd::AsyncFileDialog::new()
            .set_title("选择插件文件")
            .add_filter("插件文件 (*.wasm, *.toml)", &["wasm", "toml"])
            .pick_file()
            .await
            .map(|file| file.path().to_path_buf());
        let Some(path) = picked else {
            return Ok::<_, anyhow::Error>(None);
        };
        let candidate = tokio::task::spawn_blocking(move || management::inspect_plugin_file(&path))
            .await
            .map_err(|_| anyhow!("插件预检任务异常退出"))??;
        Ok(Some(candidate))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |this_app, cx| {
            let Ok(mut state) = state().lock() else {
                return;
            };
            state.operation_in_flight = false;
            match result {
                Ok(Some(candidate)) => {
                    state.status = format!("请确认是否导入插件 {}", candidate.name);
                    this_app.open_modal(
                        super::components::modal::GlobalModal::PluginImport(Box::new(candidate)),
                        cx,
                    );
                }
                Ok(None) => {
                    state.status = "已取消选择插件文件".into();
                }
                Err(err) => {
                    state.status = format!("插件预检失败：{err:#}");
                }
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

pub(crate) fn confirm_import(
    candidate: management::PluginImportCandidate,
    cx: &mut Context<MusicApp>,
) {
    let plugin_id = candidate.plugin_id.clone();
    let action_verb = match candidate.install_status {
        management::PluginInstallStatus::NewInstall => "导入",
        management::PluginInstallStatus::Upgrade { .. } => "升级",
        management::PluginInstallStatus::SameVersion { .. } => "覆盖",
        management::PluginInstallStatus::Downgrade { .. } => "降级导入",
    };
    if !begin_operation(format!("正在{action_verb}插件 {plugin_id}…")) {
        return;
    }
    let task = Tokio::spawn_result(cx, async move {
        let package_dir = candidate.package_dir;
        let imported =
            tokio::task::spawn_blocking(move || management::import_directory(&package_dir))
                .await
                .map_err(|_| anyhow!("插件导入任务异常退出"))??;
        let plugins = tokio::task::spawn_blocking(management::list_installed)
            .await
            .map_err(|_| anyhow!("插件列表刷新任务异常退出"))??;
        let action = if imported.updated_existing {
            "更新"
        } else {
            "导入"
        };
        let runtime_note = if imported.provider_runtime_refresh_pending {
            "；Provider/UI runtime 热替换未完成，旧运行时已安全停用，请重启应用恢复"
        } else {
            ""
        };
        Ok::<_, anyhow::Error>((
            format!(
                "插件 {} {}完成，版本 {}{}",
                imported.plugin_id, action, imported.version, runtime_note
            ),
            plugins,
        ))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            super::plugin_input::invalidate_all();
            complete_operation(result);
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn set_plugin_enabled(plugin_id: String, enabled: bool, cx: &mut Context<MusicApp>) {
    if !begin_operation(if enabled {
        "正在启用插件…"
    } else {
        "正在禁用插件…"
    }) {
        return;
    }
    let task = Tokio::spawn_result(cx, async move {
        let id_for_task = plugin_id.clone();
        let id_to_set = id_for_task.clone();
        let changed =
            tokio::task::spawn_blocking(move || management::set_enabled(&id_to_set, enabled))
                .await
                .map_err(|_| anyhow!("插件启停任务异常退出"))??;

        if enabled && changed {
            if let Some(frontend) = crate::plugin::frontend::global() {
                let _ = frontend.restore_plugin_accounts(&id_for_task).await;
            }
        }

        let plugins = tokio::task::spawn_blocking(management::list_installed)
            .await
            .map_err(|_| anyhow!("插件列表刷新任务异常退出"))??;
        let action = if enabled { "启用" } else { "禁用" };
        let status = if changed {
            format!("插件 {plugin_id} 已{action}")
        } else {
            format!("插件 {plugin_id} 已处于目标状态")
        };
        Ok::<_, anyhow::Error>((status, plugins, enabled, changed))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        let (complete_arg, enabled_val, changed_val) = match result {
            Ok((status, plugins, enabled, changed)) => (Ok((status, plugins)), enabled, changed),
            Err(e) => (Err(e), false, false),
        };
        this.update(cx, |this, cx| {
            super::plugin_input::invalidate_all();
            complete_operation(complete_arg);
            if changed_val {
                this.sync_online_service_state(cx);
                if enabled_val && this.online_authenticated {
                    this.refresh_online_recommendations(cx);
                }
            }
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn request_uninstall(plugin_id: String, cx: &mut Context<MusicApp>) {
    let armed = {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.operation_in_flight {
            return;
        }
        if state.pending_uninstall.as_deref() != Some(plugin_id.as_str()) {
            state.pending_uninstall = Some(plugin_id.clone());
            state.status = format!("再次点击“确认卸载”将删除插件 {plugin_id} 及其账号 Secret/权限");
            false
        } else {
            state.pending_uninstall = None;
            true
        }
    };
    if !armed {
        cx.notify();
        return;
    }
    if !begin_operation("正在卸载插件并回收资源…") {
        return;
    }
    let task = Tokio::spawn_result(cx, async move {
        let id_for_task = plugin_id.clone();
        let removed = tokio::task::spawn_blocking(move || management::uninstall(&id_for_task))
            .await
            .map_err(|_| anyhow!("插件卸载任务异常退出"))??;
        let plugins = tokio::task::spawn_blocking(management::list_installed)
            .await
            .map_err(|_| anyhow!("插件列表刷新任务异常退出"))??;
        Ok::<_, anyhow::Error>((
            if removed {
                format!("插件 {plugin_id} 已卸载，Host 资源已进入自动回收链")
            } else {
                format!("插件 {plugin_id} 已不存在")
            },
            plugins,
        ))
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_this, cx| {
            super::plugin_input::invalidate_all();
            complete_operation(result);
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

pub(super) fn render(_app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let current = snapshot();
    if !current.loaded && !current.loading && !current.operation_in_flight {
        refresh_plugins(cx);
    }
    let current = snapshot();

    let mut list = div().flex().flex_col().gap_3();
    if current.plugins.is_empty() {
        list = list.child(
            div()
                .p_5()
                .rounded_xl()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(theme::BORDER_CARD)
                .text_sm()
                .text_color(theme::TEXT_SECONDARY)
                .child(if current.loading {
                    "正在读取插件…"
                } else {
                    "尚未安装插件。点击右上角“导入插件”选择 .wasm 插件或 plugin.toml 清单包进行导入。"
                }),
        );
    } else {
        for plugin in current.plugins.iter().cloned() {
            list = list.child(plugin_card(plugin, &current, cx));
        }
    }

    let scroll_content = div()
        .id("plugin-settings-scroll")
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
                                        .child("插件与扩展"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::TEXT_SECONDARY)
                                        .child("管理已安装的音乐服务扩展与插件，支持导入、启用和管理账号认证。"),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(action_button(
                                    "plugin-import",
                                    "导入插件",
                                    false,
                                    current.operation_in_flight,
                                    cx.listener(|_, _, _, cx| import_plugin(cx)),
                                ))
                                .child(action_button(
                                    "plugin-refresh-list",
                                    "刷新",
                                    false,
                                    current.operation_in_flight || current.loading,
                                    cx.listener(|_, _, _, cx| refresh_plugins(cx)),
                                )),
                        ),
                )
                .child(list)
                .child(
                    div()
                        .text_xs()
                        .text_color(if current.status.starts_with("插件操作失败")
                            || current.status.starts_with("插件预检失败")
                            || current.status.starts_with("读取插件列表失败")
                        {
                            theme::ACCENT_RED
                        } else {
                            theme::TEXT_TERTIARY
                        })
                        .child(if current.status.is_empty() {
                            "支持导入 .wasm 插件文件或 plugin.toml 清单包；插件在沙盒环境中安全隔离运行。".to_string()
                        } else {
                            current.status.clone()
                        }),
                ),
        );

    scroll_content.into_any_element()
}

fn plugin_card(
    plugin: PluginSummary,
    state: &PluginSettingsSnapshot,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let plugin_id_for_toggle = plugin.plugin_id.clone();
    let target_enabled = !plugin.enabled;
    let plugin_id_for_remove = plugin.plugin_id.clone();
    let uninstall_armed = state.pending_uninstall.as_deref() == Some(plugin.plugin_id.as_str());

    div()
        .p_4()
        .rounded_xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(theme::BORDER_CARD)
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
                                .child(plugin.name.clone()),
                        )
                        .child(
                            div()
                                .px_2()
                                .py(px(2.0))
                                .rounded_full()
                                .bg(if plugin.enabled {
                                    hsla(140.0, 0.45, 0.45, 0.12)
                                } else {
                                    hsla(0.0, 0.0, 0.0, 0.05)
                                })
                                .text_xs()
                                .text_color(if plugin.enabled {
                                    hsla(140.0, 0.45, 0.36, 1.0)
                                } else {
                                    theme::TEXT_TERTIARY.into()
                                })
                                .child(if plugin.enabled {
                                    "已启用"
                                } else {
                                    "已禁用"
                                }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_SECONDARY)
                        .child(format!("{} · v{}", plugin.plugin_id, plugin.version)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(format!(
                            "Provider {} · Route {} · Page {} · Theme {} · 网络域 {}",
                            plugin.provider_count,
                            plugin.route_count,
                            plugin.page_count,
                            plugin.theme_count,
                            plugin.network_domain_count
                        )),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .gap_2()
                .children((plugin.enabled && plugin.provider_count > 0).then(|| {
                    action_button(
                        SharedString::from(format!("plugin-auth-{}", plugin.plugin_id)),
                        "账号认证",
                        false,
                        false,
                        cx.listener(|_, _, _, cx| {
                            super::settings::select_workspace(
                                super::settings::SettingsWorkspace::Authentication,
                                cx,
                            );
                        }),
                    )
                }))
                .child(action_button(
                    SharedString::from(format!("plugin-toggle-{}", plugin.plugin_id)),
                    if plugin.enabled { "禁用" } else { "启用" },
                    false,
                    state.operation_in_flight,
                    cx.listener(move |_, _, _, cx| {
                        set_plugin_enabled(plugin_id_for_toggle.clone(), target_enabled, cx)
                    }),
                ))
                .child(action_button(
                    SharedString::from(format!("plugin-uninstall-{}", plugin.plugin_id)),
                    if uninstall_armed {
                        "确认卸载"
                    } else {
                        "卸载"
                    },
                    true,
                    state.operation_in_flight,
                    cx.listener(move |_, _, _, cx| {
                        request_uninstall(plugin_id_for_remove.clone(), cx)
                    }),
                )),
        )
        .into_any_element()
}

fn action_button<I, F>(
    id: I,
    label: &'static str,
    danger: bool,
    disabled: bool,
    on_press: F,
) -> gpui::AnyElement
where
    I: Into<gpui::ElementId>,
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let text = if danger {
        theme::ACCENT_RED
    } else {
        theme::TEXT_PRIMARY
    };
    let button = div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme::BORDER_CARD)
        .bg(if danger {
            theme::accent_red_muted()
        } else {
            theme::BG_CANVAS.into()
        })
        .text_sm()
        .text_color(text)
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
