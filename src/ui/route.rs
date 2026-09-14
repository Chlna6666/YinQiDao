pub use crate::model::AppPage as AppRoute;
use gpui::App;

/// 获取当前 gpui-router pathname。插件路由保持原始 pathname，不强行映射到固定 AppPage。
pub fn current_pathname(cx: &App) -> String {
    gpui_router::use_location(cx).pathname.to_string()
}

/// 获取当前内建 gpui-router 路由。未知/插件 pathname 仍按 AppPage 的兜底规则映射。
pub fn current_route(cx: &App) -> AppRoute {
    AppRoute::from_pathname(&current_pathname(cx))
}

/// 使用 gpui-router 进行内建页面跳转。
pub fn navigate_to(cx: &mut App, route: AppRoute) {
    navigate_path(cx, route.pathname());
}

/// 使用 gpui-router 跳转到 Host 已验证的动态 pathname。
pub fn navigate_path(cx: &mut App, pathname: &str) {
    let mut navigate = gpui_router::use_navigate(cx);
    navigate(pathname.into());
}
