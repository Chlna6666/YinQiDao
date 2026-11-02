use std::sync::{Mutex, OnceLock};

use gpui::{Context, IntoElement, SharedString, div, prelude::*, px};

use super::{
    components, plugin_extensions, plugin_navigation, plugin_settings, route, shell, theme,
};
use shell::MusicApp;

#[path = "plugin/service_accounts.rs"]
mod plugin_service_accounts;
#[path = "plugin/service_auth.rs"]
mod plugin_service_auth;

pub(crate) fn render_service_auth(cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    plugin_service_auth::render(cx)
}

pub(crate) fn render_service_auth_modal_content(cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    plugin_service_auth::render_modal_content(cx)
}

pub(crate) fn cancel_service_auth_if_active(cx: &mut Context<MusicApp>) {
    plugin_service_auth::cancel_active_if_any(cx);
}

// Keep the existing large settings implementation unchanged and embed it as the Preferences tab.
// Its `super::{components, shell, theme}` imports resolve to the aliases above.
mod base {
    include!("settings.rs");
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum SettingsWorkspace {
    #[default]
    Preferences,
    Services,
    Authentication,
    Plugins,
    Extensions,
}

static SETTINGS_WORKSPACE: OnceLock<Mutex<SettingsWorkspace>> = OnceLock::new();

pub(crate) fn workspace() -> SettingsWorkspace {
    SETTINGS_WORKSPACE
        .get_or_init(|| Mutex::new(SettingsWorkspace::Preferences))
        .lock()
        .map(|workspace| *workspace)
        .unwrap_or(SettingsWorkspace::Preferences)
}

pub(crate) fn select_workspace(target: SettingsWorkspace, cx: &mut Context<MusicApp>) {
    if let Ok(mut workspace) = SETTINGS_WORKSPACE
        .get_or_init(|| Mutex::new(SettingsWorkspace::Preferences))
        .lock()
    {
        *workspace = target;
    }
    route::navigate_to(cx, route::AppRoute::Settings);
    cx.notify();
}

pub(crate) fn render_quick_audio_content(
    app: &MusicApp,
    cx: &mut Context<MusicApp>,
) -> gpui::AnyElement {
    div().flex().flex_col().gap_4()
        .child(base::eq_group(app, cx))
        .child(base::spatial_group(app, cx))
        .into_any_element()
}

pub(super) fn render(app: &MusicApp, cx: &mut Context<MusicApp>) -> gpui::AnyElement {
    let selected = workspace();
    // Sidebar- and Settings-placement routes share the same Host content surface. Placement controls
    // navigation exposure only; it must not grant different rendering/runtime privileges.
    let active_plugin_route = plugin_navigation::current(cx).ok().flatten();
    let settings_routes = plugin_navigation::settings_routes().unwrap_or_default();

    let body = if let Some(target) = active_plugin_route.as_ref() {
        plugin_navigation::render_route_shell(target, app, cx)
    } else {
        match selected {
            SettingsWorkspace::Preferences => base::render(app, cx),
            SettingsWorkspace::Services => plugin_service_accounts::render(cx),
            SettingsWorkspace::Authentication => plugin_service_auth::render(cx),
            SettingsWorkspace::Plugins => plugin_settings::render(app, cx),
            SettingsWorkspace::Extensions => plugin_extensions::render(app, cx),
        }
    };

    // Built-in workspaces already have sidebar entries; keep plugin-supplied
    // Settings placement navigation local to the Extensions workspace.
    let mut nav = div().flex().flex_wrap().gap_2().px_4().py_2();
    if active_plugin_route.is_some() || selected == SettingsWorkspace::Extensions {
        for target in settings_routes {
            let active = active_plugin_route
                .as_ref()
                .is_some_and(|current| current.pathname == target.pathname);
            let target_for_click = target.clone();
            nav = nav.child(workspace_button_dynamic(
                SharedString::from(format!("settings-plugin-route-{}", target.summary.qualified_id)),
                target.summary.title.clone(),
                active,
                cx.listener(move |this, _, _, cx| {
                    plugin_navigation::navigate(this, cx, &target_for_click);
                }),
            ));
        }
    }

    div()
        .size_full()
        .min_h(px(0.0))
        .flex()
        .flex_col()
        .bg(theme::BG_CANVAS)
        .children((active_plugin_route.is_some() || selected == SettingsWorkspace::Extensions)
            .then_some(nav))
        .child(div().flex_1().min_h(px(0.0)).overflow_hidden().child(body))
        .into_any_element()
}

fn workspace_button<F>(
    id: &'static str,
    label: &'static str,
    active: bool,
    on_press: F,
) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    workspace_button_dynamic(
        SharedString::new_static(id),
        label.to_string(),
        active,
        on_press,
    )
}

fn workspace_button_dynamic<F>(
    id: SharedString,
    label: String,
    active: bool,
    on_press: F,
) -> gpui::AnyElement
where
    F: Fn(&gpui::MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    div()
        .id(id)
        .px_4()
        .py_2()
        .rounded_lg()
        .cursor_pointer()
        .bg(if active {
            theme::accent_red_muted()
        } else {
            theme::BG_CANVAS.into()
        })
        .text_sm()
        .font_weight(if active {
            gpui::FontWeight::SEMIBOLD
        } else {
            gpui::FontWeight::NORMAL
        })
        .text_color(if active {
            theme::ACCENT_RED
        } else {
            theme::TEXT_SECONDARY
        })
        .hover(|style| style.bg(theme::bg_hover()))
        .active(|style| style.scale(0.98))
        .child(label)
        .on_mouse_down(gpui::MouseButton::Left, on_press)
        .into_any_element()
}
