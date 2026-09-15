use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Result, anyhow};
use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, Focusable, IntoElement, SharedString,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_tokio::Tokio;

use crate::{
    plugin::{
        abi::{AuthChallengeKind, AuthMethod, AuthPollResult, KeyValue, PluginCapability},
        accounts::{self, PluginServiceSummary},
        auth::PluginAuthFlowSnapshot,
        frontend, runtime_ports,
    },
    ui::{
        MusicApp,
        components::input::{HostTextInput, HostTextInputCommitHandler},
        theme,
    },
};

#[derive(Clone, Debug)]
struct AuthProviderOption {
    plugin_id: String,
    service_name: String,
    provider_id: String,
    provider_name: String,
    methods: Vec<AuthMethod>,
}

struct SensitiveValue(Vec<u8>);

impl SensitiveValue {
    fn new(value: String) -> Self {
        Self(value.into_bytes())
    }

    fn into_string(mut self) -> Result<String> {
        let bytes = std::mem::take(&mut self.0);
        String::from_utf8(bytes).map_err(|_| anyhow!("认证输入不是有效 UTF-8"))
    }
}

impl Drop for SensitiveValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

struct AuthWorkspaceState {
    generation: u64,
    options_loading: bool,
    options_loaded: bool,
    options_request_id: u64,
    options: Arc<Vec<AuthProviderOption>>,
    flow_busy: bool,
    flow_request_id: u64,
    active_flow: Option<PluginAuthFlowSnapshot>,
    form_values: HashMap<String, SensitiveValue>,
    form_submitted: bool,
    status: String,
}

impl Default for AuthWorkspaceState {
    fn default() -> Self {
        Self {
            generation: 0,
            options_loading: false,
            options_loaded: false,
            options_request_id: 0,
            options: Arc::new(Vec::new()),
            flow_busy: false,
            flow_request_id: 0,
            active_flow: None,
            form_values: HashMap::new(),
            form_submitted: false,
            status: String::new(),
        }
    }
}

#[derive(Clone)]
struct AuthRenderSnapshot {
    options_loading: bool,
    options: Arc<Vec<AuthProviderOption>>,
    flow_busy: bool,
    active_flow: Option<PluginAuthFlowSnapshot>,
    committed_fields: HashSet<String>,
    form_submitted: bool,
    status: String,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AuthInputKey {
    flow_id: u64,
    field_id: String,
}

static AUTH_STATE: OnceLock<Mutex<AuthWorkspaceState>> = OnceLock::new();

thread_local! {
    static AUTH_INPUTS: RefCell<HashMap<AuthInputKey, Entity<HostTextInput>>> = RefCell::new(HashMap::new());
}

fn state() -> &'static Mutex<AuthWorkspaceState> {
    AUTH_STATE.get_or_init(|| Mutex::new(AuthWorkspaceState::default()))
}

fn render_snapshot(generation: u64) -> (AuthRenderSnapshot, bool) {
    let mut stale_flow_id = None;
    let (snapshot, should_refresh) = match state().lock() {
        Ok(mut state) => {
            if state.generation == 0 {
                state.generation = generation;
            } else if state.generation != generation {
                stale_flow_id = state.active_flow.take().map(|flow| flow.flow_id);
                state.generation = generation;
                state.options_loaded = false;
                state.options_loading = false;
                state.options = Arc::new(Vec::new());
                state.options_request_id = state.options_request_id.wrapping_add(1);
                state.flow_request_id = state.flow_request_id.wrapping_add(1);
                state.flow_busy = false;
                state.form_values.clear();
                state.form_submitted = false;
                state.status = "插件包代际已变化，旧认证 challenge 已失效".into();
            }
            let should_refresh = !state.options_loaded && !state.options_loading;
            (
                AuthRenderSnapshot {
                    options_loading: state.options_loading,
                    options: state.options.clone(),
                    flow_busy: state.flow_busy,
                    active_flow: state.active_flow.clone(),
                    committed_fields: state.form_values.keys().cloned().collect(),
                    form_submitted: state.form_submitted,
                    status: state.status.clone(),
                },
                should_refresh,
            )
        }
        Err(_) => (
            AuthRenderSnapshot {
                options_loading: false,
                options: Arc::new(Vec::new()),
                flow_busy: false,
                active_flow: None,
                committed_fields: HashSet::new(),
                form_submitted: false,
                status: "认证 UI 状态锁已损坏".into(),
            },
            false,
        ),
    };

    if let Some(flow_id) = stale_flow_id {
        clear_flow_inputs(flow_id);
        if let Some(frontend) = frontend::global() {
            let _ = frontend.auth_flow_snapshot(flow_id);
        }
    }
    (snapshot, should_refresh)
}

fn begin_options_refresh(generation: u64) -> Option<u64> {
    let Ok(mut state) = state().lock() else {
        return None;
    };
    if state.options_loading {
        return None;
    }
    state.generation = generation;
    state.options_loading = true;
    state.options_request_id = state.options_request_id.wrapping_add(1);
    state.status = "正在读取可认证的音乐 Provider…".into();
    Some(state.options_request_id)
}

fn refresh_options(cx: &mut Context<MusicApp>) {
    let generation = runtime_ports::package_mutation_generation();
    let Some(request_id) = begin_options_refresh(generation) else {
        return;
    };
    cx.notify();

    let task = Tokio::spawn_result(cx, async move {
        tokio::task::spawn_blocking(accounts::service_summaries)
            .await
            .map_err(|_| anyhow!("认证 Provider 快照任务异常退出"))?
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_app, cx| {
            complete_options_refresh(request_id, generation, result);
            cx.notify();
        })?;
        Ok(())
    })
    .detach();
}

fn complete_options_refresh(
    request_id: u64,
    generation: u64,
    result: Result<Vec<PluginServiceSummary>>,
) {
    let current_generation = runtime_ports::package_mutation_generation();
    let Ok(mut state) = state().lock() else {
        return;
    };
    if state.options_request_id != request_id {
        return;
    }
    state.options_loading = false;
    if current_generation != generation {
        state.options_loaded = false;
        state.generation = current_generation;
        state.options = Arc::new(Vec::new());
        state.status = "插件在读取认证 Provider 期间已更新，旧结果已丢弃".into();
        return;
    }

    match result {
        Ok(services) => {
            let mut options = Vec::new();
            for service in services {
                if !service.enabled {
                    continue;
                }
                let plugin_id = service.plugin_id.clone();
                let service_name = service.name.clone();
                for provider in service.providers {
                    if !provider.capabilities.contains(&PluginCapability::Authentication)
                        || provider.auth_methods.is_empty()
                    {
                        continue;
                    }
                    options.push(AuthProviderOption {
                        plugin_id: plugin_id.clone(),
                        service_name: service_name.clone(),
                        provider_id: provider.provider_id,
                        provider_name: provider.display_name,
                        methods: provider.auth_methods,
                    });
                }
            }
            options.sort_by(|left, right| {
                left.service_name
                    .cmp(&right.service_name)
                    .then_with(|| left.provider_name.cmp(&right.provider_name))
                    .then_with(|| left.plugin_id.cmp(&right.plugin_id))
                    .then_with(|| left.provider_id.cmp(&right.provider_id))
            });
            state.options = Arc::new(options);
            state.options_loaded = true;
            state.status = format!("已发现 {} 个可认证 Provider", state.options.len());
        }
        Err(error) => {
            state.options_loaded = true;
            state.status = format!("读取认证 Provider 失败：{error:#}");
        }
    }
}

pub(super) fn render(cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let generation = runtime_ports::package_mutation_generation();
    let (snapshot, should_refresh) = render_snapshot(generation);
    if should_refresh {
        refresh_options(cx);
    }
    let view = cx.entity().downgrade();

    let body = if let Some(flow) = snapshot.active_flow.clone() {
        challenge_card(
            flow,
            snapshot.flow_busy,
            snapshot.form_submitted,
            &snapshot.committed_fields,
            &view,
            cx,
        )
    } else {
        provider_list(snapshot.options.clone(), snapshot.options_loading, cx)
    };

    div()
        .size_full()
        .overflow_y_scroll()
        .bg(theme::BG_CANVAS)
        .px_8()
        .py_6()
        .child(
            div()
                .max_w(px(900.0))
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
                                        .child("账号认证"),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme::TEXT_SECONDARY)
                                        .child("认证 UI 由 Host 统一渲染；插件只返回 challenge，Cookie/token 不写入普通配置。"),
                                ),
                        )
                        .child_if(snapshot.active_flow.is_none(), || {
                            action_button(
                                SharedString::new_static("plugin-auth-refresh"),
                                if snapshot.options_loading { "刷新中…" } else { "刷新 Provider" },
                                snapshot.options_loading,
                                cx.listener(|_, _, _, cx| refresh_options(cx)),
                            )
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(if snapshot.status.contains("失败")
                            || snapshot.status.contains("失效")
                            || snapshot.status.contains("拒绝")
                        {
                            theme::ACCENT_RED
                        } else {
                            theme::TEXT_TERTIARY
                        })
                        .child(if snapshot.status.is_empty() {
                            "选择 Provider 登录方式后，Host 会创建带代际票据的认证 flow。".to_string()
                        } else {
                            snapshot.status
                        }),
                )
                .child(body)
                .child(
                    div()
                        .p_4()
                        .rounded_xl()
                        .bg(theme::BG_CARD)
                        .border_1()
                        .border_color(theme::BORDER_CARD)
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child("表单字段一律按敏感输入处理。输入字段按 Enter 后由 Host 暂存为可清零字节缓冲；字段齐备即提交，提交参数不会进入 config.toml。二维码 challenge 当前展示并可复制 payload；原生二维码栅格渲染器后续单独接入。"),
                ),
        )
        .into_any_element()
}

fn provider_list(
    options: Arc<Vec<AuthProviderOption>>,
    loading: bool,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    if options.is_empty() {
        return div()
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
                    .child(if loading {
                        "正在读取认证 Provider…"
                    } else {
                        "当前没有可认证的 Provider"
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme::TEXT_TERTIARY)
                    .child("Provider 必须已启用，并声明 authentication capability 与至少一种 auth method。"),
            )
            .into_any_element();
    }

    let mut list = div().flex().flex_col().gap_3();
    for option in options.iter().cloned() {
        let mut methods = div().flex().flex_wrap().gap_2();
        for method in option.methods.iter().copied() {
            let plugin_id = option.plugin_id.clone();
            let provider_id = option.provider_id.clone();
            methods = methods.child(action_button(
                SharedString::from(format!(
                    "plugin-auth-method-{}-{}-{:?}",
                    option.plugin_id, option.provider_id, method
                )),
                auth_method_label(method),
                false,
                cx.listener(move |_, _, _, cx| {
                    begin_auth(plugin_id.clone(), provider_id.clone(), method, cx);
                }),
            ));
        }

        list = list.child(
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
                                        .child(option.provider_name),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme::TEXT_TERTIARY)
                                        .child(format!(
                                            "{} · {}/{}",
                                            option.service_name, option.plugin_id, option.provider_id
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child(format!("{} 种认证方式", option.methods.len())),
                        ),
                )
                .child(methods),
        );
    }
    list.into_any_element()
}

fn begin_auth(
    plugin_id: String,
    provider_id: String,
    method: AuthMethod,
    cx: &mut Context<MusicApp>,
) {
    let generation = runtime_ports::package_mutation_generation();
    let request_id = {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.flow_busy || state.active_flow.is_some() {
            return;
        }
        state.flow_busy = true;
        state.flow_request_id = state.flow_request_id.wrapping_add(1);
        state.status = format!("正在创建 {} challenge…", auth_method_label(method));
        state.flow_request_id
    };
    cx.notify();

    let Some(frontend) = frontend::global() else {
        finish_operation_error(request_id, "插件 Provider frontend 尚未初始化", cx);
        return;
    };
    let task = Tokio::spawn_result(cx, async move {
        frontend
            .auth_flow_begin(&plugin_id, &provider_id, method)
            .await
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_app, cx| complete_begin(request_id, generation, result, cx))?;
        Ok(())
    })
    .detach();
}

fn complete_begin(
    request_id: u64,
    generation: u64,
    result: Result<PluginAuthFlowSnapshot>,
    cx: &mut Context<MusicApp>,
) {
    let mut open_url = None;
    let Ok(mut state) = state().lock() else {
        return;
    };
    if state.flow_request_id != request_id {
        return;
    }
    state.flow_busy = false;
    if runtime_ports::package_mutation_generation() != generation {
        state.status = "插件在创建认证 challenge 期间已更新，旧结果已丢弃".into();
        cx.notify();
        return;
    }

    match result {
        Ok(flow) => {
            clear_all_inputs();
            state.form_values.clear();
            state.form_submitted = false;
            if flow.challenge.kind == AuthChallengeKind::Browser {
                open_url = flow.challenge.verification_uri.clone();
            }
            state.status = match flow.challenge.kind {
                AuthChallengeKind::Form => "认证表单已就绪；逐项输入并按 Enter 确认。".into(),
                _ => "认证 challenge 已就绪；完成外部操作后点击“检查状态”。".into(),
            };
            state.active_flow = Some(flow);
        }
        Err(error) => {
            state.status = format!("创建认证 challenge 失败：{error:#}");
        }
    }
    drop(state);
    if let Some(url) = open_url {
        cx.open_url(&url);
    }
    cx.notify();
}

fn challenge_card(
    flow: PluginAuthFlowSnapshot,
    busy: bool,
    form_submitted: bool,
    committed_fields: &HashSet<String>,
    view: &WeakEntity<MusicApp>,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    let flow_id = flow.flow_id;
    let mut content = div().flex().flex_col().gap_3();

    match flow.challenge.kind {
        AuthChallengeKind::QrCode => {
            let payload = flow.challenge.qr_payload.clone().unwrap_or_default();
            let payload_for_copy = payload.clone();
            content = content
                .child(info_block("二维码 payload", payload))
                .child(action_button(
                    SharedString::from(format!("plugin-auth-copy-qr-{flow_id}")),
                    "复制二维码载荷",
                    false,
                    move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(payload_for_copy.clone()));
                    },
                ));
        }
        AuthChallengeKind::Browser => {
            let url = flow.challenge.verification_uri.clone().unwrap_or_default();
            let url_for_open = url.clone();
            content = content
                .child(info_block("浏览器登录地址", url))
                .child(action_button(
                    SharedString::from(format!("plugin-auth-open-browser-{flow_id}")),
                    "打开登录页面",
                    false,
                    move |_, _, cx| cx.open_url(&url_for_open),
                ));
        }
        AuthChallengeKind::DeviceCode => {
            let url = flow.challenge.verification_uri.clone().unwrap_or_default();
            let code = flow.challenge.user_code.clone().unwrap_or_default();
            let url_for_open = url.clone();
            let code_for_copy = code.clone();
            content = content
                .child(info_block("设备登录地址", url))
                .child(info_block("设备码", code))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(action_button(
                            SharedString::from(format!("plugin-auth-open-device-{flow_id}")),
                            "打开设备登录页",
                            false,
                            move |_, _, cx| cx.open_url(&url_for_open),
                        ))
                        .child(action_button(
                            SharedString::from(format!("plugin-auth-copy-device-{flow_id}")),
                            "复制设备码",
                            false,
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    code_for_copy.clone(),
                                ));
                            },
                        )),
                );
        }
        AuthChallengeKind::Form => {
            if form_submitted {
                content = content.child(
                    div()
                        .p_3()
                        .rounded_lg()
                        .bg(theme::BG_CANVAS)
                        .text_sm()
                        .text_color(theme::TEXT_SECONDARY)
                        .child("表单已提交。若 Provider 仍返回 Pending，请使用“检查状态”；敏感字段已从 Host 暂存缓冲移除。"),
                );
            } else {
                for field in flow.challenge.fields.iter().cloned() {
                    content = content.child(form_field_row(
                        flow_id,
                        field,
                        committed_fields,
                        view,
                    ));
                }
            }
        }
    }

    let can_poll = flow.challenge.kind != AuthChallengeKind::Form || form_submitted;
    let poll_button = can_poll.then(|| {
        action_button(
            SharedString::from(format!("plugin-auth-poll-{flow_id}")),
            if busy { "检查中…" } else { "检查状态" },
            busy,
            cx.listener(move |_, _, _, cx| poll_auth(flow_id, cx)),
        )
    });

    div()
        .p_5()
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
                .gap_3()
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(theme::TEXT_PRIMARY)
                                .child(format!("{} · {}", flow.plugin_id, flow.provider_id)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::TEXT_TERTIARY)
                                .child(format!("认证方式：{}", auth_method_label(flow.method))),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(format!("Flow #{}", flow.flow_id)),
                ),
        )
        .child(content)
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(poll_button)
                .child(action_button(
                    SharedString::from(format!("plugin-auth-cancel-{flow_id}")),
                    "取消认证",
                    false,
                    cx.listener(move |_, _, _, cx| cancel_auth(flow_id, cx)),
                )),
        )
        .into_any_element()
}

fn form_field_row(
    flow_id: u64,
    field: KeyValue,
    committed_fields: &HashSet<String>,
    view: &WeakEntity<MusicApp>,
) -> gpui::AnyElement {
    let key = AuthInputKey {
        flow_id,
        field_id: field.key.clone(),
    };
    if let Some(input) = active_input(&key) {
        return div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_xs()
                    .text_color(theme::TEXT_SECONDARY)
                    .child(field.value),
            )
            .child(input)
            .child(
                div()
                    .text_xs()
                    .text_color(theme::TEXT_TERTIARY)
                    .child("敏感输入 · 按 Enter 确认此字段"),
            )
            .into_any_element();
    }

    let committed = committed_fields.contains(&field.key);
    let field_id = field.key.clone();
    let label = field.value.clone();
    let view = view.clone();
    div()
        .id(SharedString::from(format!(
            "plugin-auth-form-field-{flow_id}-{}",
            field.key
        )))
        .px_3()
        .py_3()
        .rounded_lg()
        .bg(theme::BG_CANVAS)
        .border_1()
        .border_color(theme::BORDER_HAIRLINE)
        .cursor_pointer()
        .hover(|style| style.bg(theme::bg_hover()))
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
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(theme::TEXT_PRIMARY)
                        .child(label.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::TEXT_TERTIARY)
                        .child(if committed {
                            "已确认；点击可重新输入"
                        } else {
                            "点击输入；内容始终以 secret 模式显示"
                        }),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(if committed {
                    theme::ACCENT_RED
                } else {
                    theme::TEXT_TERTIARY
                })
                .child(if committed { "已输入" } else { "待输入" }),
        )
        .on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
            activate_form_input(
                flow_id,
                field_id.clone(),
                label.clone(),
                view.clone(),
                window,
                cx,
            );
        })
        .into_any_element()
}

fn active_input(key: &AuthInputKey) -> Option<Entity<HostTextInput>> {
    AUTH_INPUTS.with(|inputs| inputs.borrow().get(key).cloned())
}

fn activate_form_input(
    flow_id: u64,
    field_id: String,
    label: String,
    view: WeakEntity<MusicApp>,
    window: &mut Window,
    cx: &mut App,
) {
    crate::ui::components::input::ensure_initialized(cx);
    let key = AuthInputKey {
        flow_id,
        field_id: field_id.clone(),
    };
    if let Some(input) = active_input(&key) {
        let focus = input.read(cx).focus_handle(cx);
        window.focus(&focus);
        return;
    }

    let commit_key = key.clone();
    let commit: HostTextInputCommitHandler = Rc::new(move |value, window, cx| {
        AUTH_INPUTS.with(|inputs| {
            inputs.borrow_mut().remove(&commit_key);
        });
        let bytes = SensitiveValue::new(value);
        let field_id = commit_key.field_id.clone();
        let _ = view.update(cx, |_app, app_cx| {
            commit_form_value(flow_id, field_id, bytes, app_cx);
        });
        window.request_animation_frame();
    });

    let input = cx.new(move |entity_cx| {
        HostTextInput::new(entity_cx, "", label, true, commit)
    });
    AUTH_INPUTS.with(|inputs| {
        inputs.borrow_mut().insert(key, input.clone());
    });
    let focus = input.read(cx).focus_handle(cx);
    window.focus(&focus);
    window.request_animation_frame();
}

fn commit_form_value(
    flow_id: u64,
    field_id: String,
    value: SensitiveValue,
    cx: &mut Context<MusicApp>,
) {
    let mut submission = None;
    let mut request_id = 0;
    {
        let Ok(mut state) = state().lock() else {
            return;
        };
        let Some(flow) = state.active_flow.clone() else {
            return;
        };
        if flow.flow_id != flow_id || flow.challenge.kind != AuthChallengeKind::Form {
            return;
        }
        if !flow.challenge.fields.iter().any(|field| field.key == field_id) {
            return;
        }
        state.form_values.insert(field_id, value);
        state.status = format!(
            "已确认 {}/{} 个表单字段",
            state.form_values.len(),
            flow.challenge.fields.len()
        );

        let complete = flow
            .challenge
            .fields
            .iter()
            .all(|field| state.form_values.contains_key(&field.key));
        if complete && !state.flow_busy {
            let mut values = Vec::with_capacity(flow.challenge.fields.len());
            for field in &flow.challenge.fields {
                let Some(value) = state.form_values.remove(&field.key) else {
                    return;
                };
                let Ok(value) = value.into_string() else {
                    state.status = "认证表单输入编码异常".into();
                    return;
                };
                values.push(KeyValue {
                    key: field.key.clone(),
                    value,
                });
            }
            state.flow_busy = true;
            state.form_submitted = true;
            state.flow_request_id = state.flow_request_id.wrapping_add(1);
            request_id = state.flow_request_id;
            state.status = "认证表单字段已齐备，正在提交…".into();
            submission = Some(values);
        }
    }
    cx.notify();

    if let Some(values) = submission {
        submit_form(flow_id, request_id, values, cx);
    }
}

fn submit_form(
    flow_id: u64,
    request_id: u64,
    mut values: Vec<KeyValue>,
    cx: &mut Context<MusicApp>,
) {
    let Some(frontend) = frontend::global() else {
        wipe_submission(&mut values);
        finish_operation_error(request_id, "插件 Provider frontend 尚未初始化", cx);
        return;
    };
    let task = Tokio::spawn_result(cx, async move {
        let result = frontend.auth_flow_submit(flow_id, &values).await;
        wipe_submission(&mut values);
        result
    });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_app, cx| {
            complete_flow_result(flow_id, request_id, true, result, cx);
        })?;
        Ok(())
    })
    .detach();
}

fn poll_auth(flow_id: u64, cx: &mut Context<MusicApp>) {
    let request_id = {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state.flow_busy
            || state
                .active_flow
                .as_ref()
                .is_none_or(|flow| flow.flow_id != flow_id)
        {
            return;
        }
        state.flow_busy = true;
        state.flow_request_id = state.flow_request_id.wrapping_add(1);
        state.status = "正在检查认证状态…".into();
        state.flow_request_id
    };
    cx.notify();

    let Some(frontend) = frontend::global() else {
        finish_operation_error(request_id, "插件 Provider frontend 尚未初始化", cx);
        return;
    };
    let task = Tokio::spawn_result(cx, async move { frontend.auth_flow_poll(flow_id).await });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_app, cx| {
            complete_flow_result(flow_id, request_id, false, result, cx);
        })?;
        Ok(())
    })
    .detach();
}

fn complete_flow_result(
    flow_id: u64,
    request_id: u64,
    submitted_form: bool,
    result: Result<AuthPollResult>,
    cx: &mut Context<MusicApp>,
) {
    let mut terminal = false;
    let Ok(mut state) = state().lock() else {
        return;
    };
    if state.flow_request_id != request_id
        || state
            .active_flow
            .as_ref()
            .is_none_or(|flow| flow.flow_id != flow_id)
    {
        return;
    }
    state.flow_busy = false;

    match result {
        Ok(AuthPollResult::Pending) => {
            state.form_submitted |= submitted_form;
            state.status = "Provider 仍在等待认证完成；完成外部操作后再次检查状态。".into();
        }
        Ok(AuthPollResult::Authenticated(account)) => {
            terminal = true;
            state.active_flow = None;
            state.form_values.clear();
            state.form_submitted = false;
            state.status = format!("账号 {} 已认证并立即加入 Host 路由", account.display_name);
        }
        Ok(AuthPollResult::Expired) => {
            terminal = true;
            state.active_flow = None;
            state.form_values.clear();
            state.form_submitted = false;
            state.status = "认证 challenge 已过期，请重新开始。".into();
        }
        Ok(AuthPollResult::Denied(reason)) => {
            terminal = true;
            state.active_flow = None;
            state.form_values.clear();
            state.form_submitted = false;
            let reason = truncate_text(&reason, 512);
            state.status = format!("认证被 Provider 拒绝：{reason}");
        }
        Err(error) => {
            if submitted_form {
                state.form_submitted = false;
            }
            state.status = format!("认证操作失败：{error:#}");
        }
    }
    drop(state);
    if terminal {
        clear_flow_inputs(flow_id);
    }
    cx.notify();
}

fn cancel_auth(flow_id: u64, cx: &mut Context<MusicApp>) {
    let request_id = {
        let Ok(mut state) = state().lock() else {
            return;
        };
        if state
            .active_flow
            .as_ref()
            .is_none_or(|flow| flow.flow_id != flow_id)
        {
            return;
        }
        state.active_flow = None;
        state.form_values.clear();
        state.form_submitted = false;
        state.flow_busy = false;
        state.flow_request_id = state.flow_request_id.wrapping_add(1);
        state.status = "正在取消认证 flow…".into();
        state.flow_request_id
    };
    clear_flow_inputs(flow_id);
    cx.notify();

    let Some(frontend) = frontend::global() else {
        finish_cancel(request_id, Err(anyhow!("插件 Provider frontend 尚未初始化")), cx);
        return;
    };
    let task = Tokio::spawn_result(cx, async move { frontend.auth_flow_cancel(flow_id).await });
    cx.spawn(async move |this, cx| -> Result<()> {
        let result = task.await;
        this.update(cx, |_app, cx| finish_cancel(request_id, result, cx))?;
        Ok(())
    })
    .detach();
}

fn finish_cancel(request_id: u64, result: Result<bool>, cx: &mut Context<MusicApp>) {
    let Ok(mut state) = state().lock() else {
        return;
    };
    if state.flow_request_id != request_id || state.active_flow.is_some() {
        return;
    }
    state.status = match result {
        Ok(true) => "认证 flow 已取消，Provider 已确认。".into(),
        Ok(false) => "认证 flow 已在 Host 本地取消；Provider 未返回确认。".into(),
        Err(error) => format!("认证 flow 已在 Host 本地取消；远端清理失败：{error:#}"),
    };
    cx.notify();
}

fn finish_operation_error(request_id: u64, error: &str, cx: &mut Context<MusicApp>) {
    let Ok(mut state) = state().lock() else {
        return;
    };
    if state.flow_request_id != request_id {
        return;
    }
    state.flow_busy = false;
    state.form_submitted = false;
    state.status = error.to_string();
    cx.notify();
}

fn wipe_submission(values: &mut [KeyValue]) {
    for field in values {
        let value = std::mem::take(&mut field.value);
        let mut bytes = value.into_bytes();
        bytes.fill(0);
    }
}

fn clear_flow_inputs(flow_id: u64) {
    AUTH_INPUTS.with(|inputs| {
        inputs.borrow_mut().retain(|key, _| key.flow_id != flow_id);
    });
}

fn clear_all_inputs() {
    AUTH_INPUTS.with(|inputs| inputs.borrow_mut().clear());
}

fn info_block(label: &'static str, value: String) -> gpui::AnyElement {
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
                .child(label),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::TEXT_PRIMARY)
                .child(value),
        )
        .into_any_element()
}

fn action_button<F>(id: SharedString, label: &'static str, disabled: bool, on_press: F) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut Window, &mut App) + 'static,
{
    let button = div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_lg()
        .border_1()
        .border_color(theme::BORDER_CARD)
        .bg(theme::BG_CARD)
        .text_sm()
        .text_color(if disabled {
            theme::TEXT_TERTIARY
        } else {
            theme::TEXT_PRIMARY
        })
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

fn auth_method_label(method: AuthMethod) -> &'static str {
    match method {
        AuthMethod::QrCode => "二维码",
        AuthMethod::BrowserOAuth => "浏览器 OAuth",
        AuthMethod::DeviceCode => "设备码",
        AuthMethod::CookieImport => "Cookie 导入",
        AuthMethod::CustomForm => "Host 表单",
    }
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let result = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}
