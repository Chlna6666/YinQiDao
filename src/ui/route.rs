pub use crate::model::AppPage as AppRoute;
use gpui::App;

/// 获取当前 gpui-router 路由
pub fn current_route(cx: &App) -> AppRoute {
    let location = gpui_router::use_location(cx);
    AppRoute::from_pathname(&location.pathname)
}

/// 使用 gpui-router 进行路由跳转
pub fn navigate_to(cx: &mut App, route: AppRoute) {
    let mut navigate = gpui_router::use_navigate(cx);
    navigate(route.pathname().into());
}
