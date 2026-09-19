use std::{collections::HashSet, rc::Rc};

use anyhow::Result;
use gpui::{
    Context, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, SharedString, WeakEntity, div,
    hsla, img, linear_color_stop, linear_gradient, prelude::*, px, rgb,
};
use gpui_tokio::Tokio;
use lucide_gpui::icon;

use crate::{
    model::Track,
    plugin::{
        abi::PluginRoute,
        extensions::{self, PluginHomeSectionSummary},
        management::{self, PluginFieldValue},
    },
    ui::image_cache,
};

use super::{
    plugin_page_renderer::{self, PluginUiInteraction, PluginUiInteractionHandler},
    shell::{MusicApp, app_listener},
    theme::{
        self, ACCENT_RED, BORDER_CARD, TEXT_PRIMARY, TEXT_SECONDARY, TEXT_TERTIARY, TEXT_WHITE,
        elegant_gradient_for, format_time, press_transition, themed_icon, waveform_animation,
    },
};

pub(super) fn render(app: &MusicApp, view: &WeakEntity<MusicApp>) -> gpui::AnyElement {
    let has_online_plugin = app.has_online_plugins;
    let is_authenticated = app.online_authenticated;
    let has_online_content = !app.online_daily_tracks.is_empty()
        || !app.online_playlists.is_empty()
        || !app.online_new_tracks.is_empty();

    let mut home = div()
        .id("home-scroll")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .p_8()
        .gap_8()
        .child(header(app, view));

    if has_online_plugin && is_authenticated {
        // 在线服务已登录：优先显示在线音乐推荐
        home = home
            .child(discover_feature_cards(app, view))
            .children(recommended_playlists_section(app, view))
            .children(daily_recommended_tracks_section(app, view))
            .children(plugin_home_sections(view));

        if !app.tracks.is_empty() {
            home = home.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_8()
                    .child(stats_overview(app))
                    .child(featured_albums_section(app, view))
                    .child(recent_tracks_section(app, view)),
            );
        } else if !has_online_content && !app.online_recommendations_loading {
            home = home.child(empty_state(app, view));
        }
    } else if has_online_plugin && !is_authenticated {
        // 在线服务可用但未登录：提供登录引导卡片与本地音乐展示
        home = home.child(login_invitation_card(view));

        if !app.tracks.is_empty() {
            home = home.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_8()
                    .child(stats_overview(app))
                    .child(featured_albums_section(app, view))
                    .child(recent_tracks_section(app, view)),
            );
        } else {
            home = home.child(empty_state(app, view));
        }
    } else {
        // 纯本地音乐模式（无在线插件或已禁用）：只展示本地音乐库，绝不展示登录卡片
        if !app.tracks.is_empty() {
            home = home.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_8()
                    .child(stats_overview(app))
                    .child(featured_albums_section(app, view))
                    .child(recent_tracks_section(app, view)),
            );
        } else {
            home = home.child(empty_state(app, view));
        }
    }

    home.into_any_element()
}

fn login_invitation_card(view: &WeakEntity<MusicApp>) -> gpui::AnyElement {
    div()
        .id("home-login-card")
        .w_full()
        .flex_none()
        .p_6()
        .rounded_2xl()
        .bg(theme::BG_CARD)
        .border_1()
        .border_color(BORDER_CARD)
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .size(px(48.0))
                        .rounded_xl()
                        .bg(theme::accent_red_muted())
                        .border_1()
                        .border_color(hsla(348.0, 0.95, 0.56, 0.25))
                        .flex()
                        .items_center()
                        .justify_center()
                        .flex_none()
                        .child(themed_icon(icon!(sparkles), 24.0, ACCENT_RED.into())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_base()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(TEXT_PRIMARY)
                                .child("登录网易云音乐 · 解锁在线推荐与云端曲库"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_SECONDARY)
                                .child("支持扫码、Cookie、手机号等快捷认证；登录后即可同步每日推荐、私人雷达、云端歌单与 SQ 无损试听。"),
                        ),
                ),
        )
        .child(
            div()
                .id("home-login-card-btn")
                .flex_none()
                .h(px(38.0))
                .px_5()
                .py_2_5()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .bg(ACCENT_RED)
                .text_color(gpui::white())
                .hover(|s| s.bg(theme::accent_red_active()))
                .transition(press_transition())
                .active(|s| s.scale(0.96))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("立即登录"),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    app_listener(view, |this, _, _, cx| {
                        this.open_modal(crate::ui::components::modal::GlobalModal::ServiceAuth, cx);
                    }),
                ),
        )
        .into_any_element()
}

fn plugin_home_sections(view: &WeakEntity<MusicApp>) -> Option<gpui::AnyElement> {
    let sections = extensions::home_sections().ok()?;
    if sections.is_empty() {
        return None;
    }
    let interactive = management::ui_client_ready();
    let mut list = div().w_full().flex().flex_col().gap_4();
    for section in sections {
        let body = match extensions::home_section_snapshot(&section.qualified_id) {
            Ok(Some(snapshot)) => {
                let handler = interactive.then(|| home_plugin_interaction_handler(&section, view));
                plugin_page_renderer::render_plugin_page(
                    &snapshot.plugin_id,
                    &snapshot.page_id,
                    snapshot.revision,
                    snapshot.model.as_ref(),
                    handler,
                )
            }
            Ok(None) => plugin_home_placeholder("正在加载插件扩展内容…"),
            Err(_) => plugin_home_placeholder("插件扩展暂不可用"),
        };
        list = list.child(
            div()
                .id(SharedString::from(format!(
                    "plugin-home-section-{}",
                    section.qualified_id
                )))
                .w_full()
                .p_4()
                .rounded_xl()
                .bg(theme::BG_CARD)
                .border_1()
                .border_color(BORDER_CARD)
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
                                .text_lg()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(TEXT_PRIMARY)
                                .child(section.title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_TERTIARY)
                                .child(section.plugin_id),
                        ),
                )
                .child(body),
        );
    }

    Some(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .text_color(TEXT_PRIMARY)
                    .child("插件扩展"),
            )
            .child(list)
            .into_any_element(),
    )
}

fn plugin_home_placeholder(text: &'static str) -> gpui::AnyElement {
    div()
        .w_full()
        .min_h(px(72.0))
        .rounded_lg()
        .bg(theme::BG_CANVAS)
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_color(TEXT_TERTIARY)
        .child(text)
        .into_any_element()
}

fn home_plugin_interaction_handler(
    section: &PluginHomeSectionSummary,
    view: &WeakEntity<MusicApp>,
) -> PluginUiInteractionHandler {
    let plugin_id = section.plugin_id.clone();
    let page_id = section.page_id.clone();
    let view = view.clone();
    Rc::new(move |interaction, _window, cx| {
        let plugin_id = plugin_id.clone();
        let page_id = page_id.clone();
        let _ = view.update(cx, |app, app_cx| {
            let interaction = match interaction {
                PluginUiInteraction::BeginInput { .. } => {
                    app.status = "插件文本输入正在等待 Host 输入组件接入".into();
                    app_cx.notify();
                    return;
                }
                interaction => interaction,
            };

            app.status = format!("正在处理首页插件操作：{plugin_id}/{page_id}");
            app_cx.notify();
            let task = Tokio::spawn_result(app_cx, async move {
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
                    PluginUiInteraction::BeginInput { .. } => {
                        unreachable!("handled before dispatch")
                    }
                }
            });
            app_cx
                .spawn(async move |this, cx| -> Result<()> {
                    let result = task.await;
                    this.update(cx, |this, cx| {
                        this.status = match result {
                            Ok(result) => result.toast.unwrap_or_else(|| {
                                format!(
                                    "首页插件内容已更新：{}/{} · rev {}",
                                    result.snapshot.plugin_id,
                                    result.snapshot.page_id,
                                    result.snapshot.revision
                                )
                            }),
                            Err(error) => format!("首页插件操作失败：{error:#}"),
                        };
                        cx.notify();
                    })?;
                    Ok(())
                })
                .detach();
        });
    })
}

fn header(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let greeting = current_time_greeting();

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
                        .text_color(TEXT_PRIMARY)
                        .child(greeting),
                )
                .child(div().text_sm().text_color(TEXT_SECONDARY).child(
                    if app.has_online_plugins && app.online_authenticated {
                        if app.online_recommendations_loading {
                            "正在同步网易云音乐每日推荐与个性化歌单…"
                        } else if !app.online_daily_tracks.is_empty()
                            || !app.online_playlists.is_empty()
                        {
                            "网易云音乐推荐已同步 · 畅享高品质音乐"
                        } else {
                            "畅享海量云端曲库与每日个性化推荐"
                        }
                    } else if app.scan_in_progress {
                        "正在高速扫描音乐目录元数据…"
                    } else if app.has_online_plugins {
                        "畅享海量云端曲库与本地离线音乐库"
                    } else {
                        "本地音乐库 · 离线畅享高品质音乐"
                    },
                )),
        )
        .child(div().flex().items_center().gap_3().children(
            (app.has_online_plugins && !app.online_authenticated).then(|| {
                div()
                    .id("home-login-header")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_2()
                    .rounded_full()
                    .cursor_pointer()
                    .bg(theme::accent_red_muted())
                    .text_color(ACCENT_RED)
                    .border_1()
                    .border_color(hsla(348.0, 0.95, 0.56, 0.20))
                    .hover(|s| s.bg(theme::accent_red_active()))
                    .transition(press_transition())
                    .active(|s| s.scale(0.96))
                    .child(themed_icon(icon!(user), 15.0, ACCENT_RED.into()))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("登录网易云"),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        app_listener(view, |this, _, _, cx| {
                            this.open_modal(
                                crate::ui::components::modal::GlobalModal::ServiceAuth,
                                cx,
                            );
                        }),
                    )
            }),
        ))
}

fn format_play_count(score: Option<f32>) -> String {
    match score {
        Some(s) if s >= 100_000_000.0 => format!("{:.1}亿", s / 100_000_000.0),
        Some(s) if s >= 10_000.0 => format!("{:.1}万", s / 10_000.0),
        Some(s) if s > 0.0 => format!("{:.0}", s),
        _ => "精选".to_string(),
    }
}

pub(crate) fn default_netease_route() -> PluginRoute {
    PluginRoute {
        plugin_id: "netease".to_string(),
        provider_id: "netease-music".to_string(),
        account_id: String::new(),
        priority: 0,
        is_default: true,
    }
}

fn discover_feature_cards(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let route = app
        .online_route
        .clone()
        .unwrap_or_else(default_netease_route);

    // Card 1: 每日推荐
    let card1_subtitle = if let Some(track) = app.online_daily_tracks.first() {
        format!("每日推荐 | 从 [{}] 听起", track.title)
    } else {
        "每日 6:00 更新 · 专属精选".to_string()
    };
    let card1_cover = app
        .online_daily_tracks
        .first()
        .and_then(|t| t.cover_url.as_deref());
    let card1_tracks = app.online_daily_tracks.clone();
    let card1_route = route.clone();

    // Card 2: 心动模式
    let card2_cover = app
        .online_daily_tracks
        .get(1)
        .and_then(|t| t.cover_url.as_deref());
    let card2_track = app
        .online_daily_tracks
        .get(1)
        .cloned()
        .or_else(|| app.online_daily_tracks.first().cloned());
    let card2_route = route.clone();
    let card2_track_rc = card2_track.clone();
    let card2_route_rc = card2_route.clone();

    // Card 3: 私人雷达
    let card3_subtitle = if let Some(pl) = app.online_playlists.first() {
        format!("今天《{}》爱不释耳", pl.collection.title)
    } else {
        "发掘你的专属音乐".to_string()
    };
    let card3_cover = app
        .online_playlists
        .first()
        .and_then(|pl| pl.collection.artwork_url.as_deref());
    let card3_title = app
        .online_playlists
        .first()
        .map(|pl| pl.collection.title.clone())
        .unwrap_or_else(|| "私人雷达".to_string());
    let card3_collection = app
        .online_playlists
        .first()
        .map(|pl| pl.collection.source.clone());
    let card3_route = route.clone();
    let card3_col_rc = card3_collection.clone();
    let card3_route_rc = card3_route.clone();
    let card3_title_rc = card3_title.clone();
    let card3_sub_rc = card3_subtitle.clone();
    let card3_cov_rc = card3_cover.map(String::from);

    // Card 4: 私人漫游
    let card4_cover = app
        .online_daily_tracks
        .get(2)
        .and_then(|t| t.cover_url.as_deref());
    let card4_track = app
        .online_daily_tracks
        .get(2)
        .cloned()
        .or_else(|| app.online_new_tracks.first().cloned());
    let card4_route = route.clone();
    let card4_track_rc = card4_track.clone();
    let card4_route_rc = card4_route.clone();

    // Card 5: 新歌速递
    let card5_subtitle = if let Some(track) = app.online_new_tracks.first() {
        format!("最新《{}》新鲜出炉", track.title)
    } else {
        "精选最新上线新歌".to_string()
    };
    let card5_cover = app
        .online_new_tracks
        .first()
        .and_then(|t| t.cover_url.as_deref());
    let card5_track = app.online_new_tracks.first().cloned();
    let card5_route = route.clone();
    let card5_track_rc = card5_track.clone();
    let card5_route_rc = card5_route.clone();

    // Card 6: 歌单精选
    let card6_subtitle = if let Some(pl) = app.online_playlists.get(1) {
        format!("热门《{}》超高人气", pl.collection.title)
    } else {
        "超十万用户正在收听".to_string()
    };
    let card6_cover = app
        .online_playlists
        .get(1)
        .and_then(|pl| pl.collection.artwork_url.as_deref());
    let card6_title = app
        .online_playlists
        .get(1)
        .map(|pl| pl.collection.title.clone())
        .unwrap_or_else(|| "精选歌单".to_string());
    let card6_collection = app
        .online_playlists
        .get(1)
        .map(|pl| pl.collection.source.clone());
    let card6_route = route.clone();
    let card6_col_rc = card6_collection.clone();
    let card6_route_rc = card6_route.clone();
    let card6_title_rc = card6_title.clone();
    let card6_sub_rc = card6_subtitle.clone();
    let card6_cov_rc = card6_cover.map(String::from);

    div()
        .w_full()
        .flex()
        .flex_wrap()
        .gap_4()
        .child(feature_card(
            "每日推荐",
            card1_subtitle,
            icon!(calendar_heart),
            card1_cover,
            (rgb(0x3a1c1c), rgb(0x6b2222)),
            view,
            move |this, cx| {
                if !card1_tracks.is_empty() {
                    this.play_online_playlist_track(
                        card1_route.clone(),
                        "每日歌曲推荐".into(),
                        card1_tracks.clone(),
                        0,
                        cx,
                    );
                } else {
                    this.refresh_online_recommendations(cx);
                }
            },
            move |this, pos, cx| {
                this.open_context_menu(
                    pos,
                    "每日推荐".into(),
                    crate::ui::components::modal::ContextMenuTarget::DailyRecommendations,
                    cx,
                );
            },
        ))
        .child(feature_card(
            "心动模式",
            "你的红心歌曲和更多相似推荐".to_string(),
            icon!(heart),
            card2_cover,
            (rgb(0x3b1120), rgb(0x701a38)),
            view,
            move |this, cx| {
                if let Some(track) = card2_track.clone() {
                    this.play_online_remote_track(card2_route.clone(), track, cx);
                } else {
                    this.refresh_online_recommendations(cx);
                }
            },
            move |this, pos, cx| {
                if let Some(track) = card2_track_rc.clone() {
                    this.open_context_menu(
                        pos,
                        track.title.clone(),
                        crate::ui::components::modal::ContextMenuTarget::OnlineTrack {
                            route: card2_route_rc.clone(),
                            track,
                        },
                        cx,
                    );
                }
            },
        ))
        .child(feature_card(
            "私人雷达",
            card3_subtitle,
            icon!(radio),
            card3_cover,
            (rgb(0x23193d), rgb(0x43286e)),
            view,
            move |this, cx| {
                if let Some(col) = card3_collection.clone() {
                    this.load_and_play_online_playlist(
                        card3_route.clone(),
                        col,
                        card3_title.clone(),
                        cx,
                    );
                } else {
                    this.refresh_online_recommendations(cx);
                }
            },
            move |this, pos, cx| {
                if let Some(col) = card3_col_rc.clone() {
                    this.open_context_menu(
                        pos,
                        card3_title_rc.clone(),
                        crate::ui::components::modal::ContextMenuTarget::OnlinePlaylist {
                            route: card3_route_rc.clone(),
                            collection: col,
                            title: card3_title_rc.clone(),
                            subtitle: card3_sub_rc.clone(),
                            cover_url: card3_cov_rc.clone(),
                        },
                        cx,
                    );
                }
            },
        ))
        .child(feature_card(
            "私人漫游",
            "开启无限漫游随心听".to_string(),
            icon!(compass),
            card4_cover,
            (rgb(0x13283d), rgb(0x1e486b)),
            view,
            move |this, cx| {
                if let Some(track) = card4_track.clone() {
                    this.play_online_remote_track(card4_route.clone(), track, cx);
                } else {
                    this.refresh_online_recommendations(cx);
                }
            },
            move |this, pos, cx| {
                if let Some(track) = card4_track_rc.clone() {
                    this.open_context_menu(
                        pos,
                        track.title.clone(),
                        crate::ui::components::modal::ContextMenuTarget::OnlineTrack {
                            route: card4_route_rc.clone(),
                            track,
                        },
                        cx,
                    );
                }
            },
        ))
        .child(feature_card(
            "新歌速递",
            card5_subtitle,
            icon!(sparkles),
            card5_cover,
            (rgb(0x3a2912), rgb(0x6e4819)),
            view,
            move |this, cx| {
                if let Some(track) = card5_track.clone() {
                    this.play_online_remote_track(card5_route.clone(), track, cx);
                } else {
                    this.refresh_online_recommendations(cx);
                }
            },
            move |this, pos, cx| {
                if let Some(track) = card5_track_rc.clone() {
                    this.open_context_menu(
                        pos,
                        track.title.clone(),
                        crate::ui::components::modal::ContextMenuTarget::OnlineTrack {
                            route: card5_route_rc.clone(),
                            track,
                        },
                        cx,
                    );
                }
            },
        ))
        .child(feature_card(
            "精选歌单",
            card6_subtitle,
            icon!(list_music),
            card6_cover,
            (rgb(0x123227), rgb(0x1d5c47)),
            view,
            move |this, cx| {
                if let Some(col) = card6_collection.clone() {
                    this.load_and_play_online_playlist(
                        card6_route.clone(),
                        col,
                        card6_title.clone(),
                        cx,
                    );
                } else {
                    this.refresh_online_recommendations(cx);
                }
            },
            move |this, pos, cx| {
                if let Some(col) = card6_col_rc.clone() {
                    this.open_context_menu(
                        pos,
                        card6_title_rc.clone(),
                        crate::ui::components::modal::ContextMenuTarget::OnlinePlaylist {
                            route: card6_route_rc.clone(),
                            collection: col,
                            title: card6_title_rc.clone(),
                            subtitle: card6_sub_rc.clone(),
                            cover_url: card6_cov_rc.clone(),
                        },
                        cx,
                    );
                }
            },
        ))
}

fn feature_card<F, R>(
    title: &'static str,
    subtitle: String,
    badge_icon: &'static str,
    cover_url: Option<&str>,
    fallback_gradient: (gpui::Rgba, gpui::Rgba),
    view: &WeakEntity<MusicApp>,
    on_click: F,
    on_right_click: R,
) -> impl IntoElement
where
    F: Fn(&mut MusicApp, &mut Context<MusicApp>) + 'static,
    R: Fn(&mut MusicApp, gpui::Point<gpui::Pixels>, &mut Context<MusicApp>) + 'static,
{
    let bg_element = if let Some(url) = cover_url {
        image_cache::render_remote_cover(Some(url), 160.0, 200.0, 14.0)
    } else {
        div()
            .size_full()
            .rounded(px(14.0))
            .bg(linear_gradient(
                135.0,
                linear_color_stop(fallback_gradient.0, 0.0),
                linear_color_stop(fallback_gradient.1, 1.0),
            ))
            .into_any_element()
    };

    div()
        .flex_1()
        .min_w(px(145.0))
        .max_w(px(180.0))
        .h(px(200.0))
        .rounded(px(14.0))
        .relative()
        .overflow_hidden()
        .cursor_pointer()
        .border_1()
        .border_color(BORDER_CARD)
        .hover(|s| s.scale(1.02).border_color(ACCENT_RED))
        .transition(press_transition())
        .child(bg_element)
        .child(div().absolute().inset_0().bg(hsla(0.0, 0.0, 0.0, 0.25)))
        .child(
            div()
                .absolute()
                .top_3()
                .left_3()
                .flex()
                .items_center()
                .gap_1p5()
                .px_2p5()
                .py_1()
                .rounded_lg()
                .bg(hsla(0.0, 0.0, 0.0, 0.55))
                .child(themed_icon(badge_icon, 13.0, TEXT_WHITE.into()))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_WHITE)
                        .child(title),
                ),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .p_2p5()
                .bg(hsla(0.0, 0.0, 0.0, 0.65))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(TEXT_WHITE)
                        .line_clamp(2)
                        .child(subtitle),
                ),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            app_listener(view, move |this, _, _, cx| {
                on_click(this, cx);
            }),
        )
        .on_mouse_down(
            gpui::MouseButton::Right,
            app_listener(view, move |this, _, window, cx| {
                let pos = window.mouse_position();
                on_right_click(this, pos, cx);
            }),
        )
}

fn recommended_playlists_section(
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> Option<gpui::AnyElement> {
    if app.online_playlists.is_empty() && !app.online_recommendations_loading {
        return None;
    }

    let route = app
        .online_route
        .clone()
        .unwrap_or_else(default_netease_route);

    let mut grid = div().w_full().flex().flex_wrap().gap_4();

    if app.online_playlists.is_empty() && app.online_recommendations_loading {
        for _ in 0..8 {
            grid = grid.child(
                div()
                    .w(px(150.0))
                    .h(px(190.0))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .size(px(150.0))
                            .rounded_xl()
                            .bg(theme::BG_CANVAS)
                            .border_1()
                            .border_color(BORDER_CARD)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(themed_icon(icon!(music), 24.0, TEXT_TERTIARY.into())),
                    )
                    .child(
                        div()
                            .h(px(14.0))
                            .w(px(110.0))
                            .rounded_md()
                            .bg(theme::BG_CANVAS),
                    ),
            );
        }
    } else {
        for item in app.online_playlists.iter().take(8) {
            let collection_ref = item.collection.source.clone();
            let route_clone = route.clone();
            let cover_url = item.collection.artwork_url.as_deref();
            let title = item.collection.title.clone();
            let subtitle = item
                .collection
                .subtitle
                .clone()
                .unwrap_or_else(|| "网易云音乐".to_string());
            let score_str = format_play_count(item.score);

            let title_play = title.clone();
            let title_menu = title.clone();
            let sub_menu = subtitle.clone();
            let cov_menu = cover_url.map(String::from);
            let col_menu = collection_ref.clone();
            let route_menu = route_clone.clone();

            grid = grid.child(
                div()
                    .w(px(150.0))
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    .hover(|s| s.scale(1.02))
                    .transition(press_transition())
                    .child(
                        div()
                            .size(px(150.0))
                            .rounded_xl()
                            .relative()
                            .overflow_hidden()
                            .border_1()
                            .border_color(BORDER_CARD)
                            .child(image_cache::render_remote_cover(
                                cover_url, 150.0, 150.0, 12.0,
                            ))
                            .child(
                                div()
                                    .absolute()
                                    .top_2()
                                    .right_2()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(hsla(0.0, 0.0, 0.0, 0.55))
                                    .child(themed_icon(icon!(headphones), 11.0, TEXT_WHITE.into()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .text_color(TEXT_WHITE)
                                            .child(score_str),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .mt_2()
                            .w(px(150.0))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(TEXT_PRIMARY)
                            .line_clamp(2)
                            .child(title),
                    )
                    .child(
                        div()
                            .mt_0p5()
                            .w(px(150.0))
                            .text_xs()
                            .text_color(TEXT_TERTIARY)
                            .truncate()
                            .child(subtitle),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Left,
                        app_listener(view, move |this, _, _, cx| {
                            this.load_and_play_online_playlist(
                                route_clone.clone(),
                                collection_ref.clone(),
                                title_play.clone(),
                                cx,
                            );
                        }),
                    )
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        app_listener(view, move |this, _, window, cx| {
                            let mouse_pos = window.mouse_position();
                            this.open_context_menu(
                                mouse_pos,
                                title_menu.clone(),
                                crate::ui::components::modal::ContextMenuTarget::OnlinePlaylist {
                                    route: route_menu.clone(),
                                    collection: col_menu.clone(),
                                    title: title_menu.clone(),
                                    subtitle: sub_menu.clone(),
                                    cover_url: cov_menu.clone(),
                                },
                                cx,
                            );
                        }),
                    ),
            );
        }
    }

    Some(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(TEXT_PRIMARY)
                                    .child("推荐歌单"),
                            )
                            .child(themed_icon(icon!(chevron_right), 18.0, TEXT_PRIMARY.into())),
                    )
                    .child(
                        div()
                            .size(px(28.0))
                            .rounded_full()
                            .hover(|s| s.bg(theme::bg_hover()))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(themed_icon(icon!(refresh_cw), 15.0, TEXT_SECONDARY.into()))
                            .on_mouse_down(
                                gpui::MouseButton::Left,
                                app_listener(view, |this, _, _, cx| {
                                    this.refresh_online_recommendations(cx);
                                }),
                            ),
                    ),
            )
            .child(grid)
            .into_any_element(),
    )
}

fn daily_recommended_tracks_section(
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> Option<gpui::AnyElement> {
    if app.online_daily_tracks.is_empty() && !app.online_recommendations_loading {
        return None;
    }

    let route = app
        .online_route
        .clone()
        .unwrap_or_else(default_netease_route);

    let mut track_grid = div().w_full().flex().flex_wrap().gap_3();

    if app.online_daily_tracks.is_empty() && app.online_recommendations_loading {
        for _ in 0..6 {
            track_grid = track_grid.child(
                div()
                    .flex_1()
                    .min_w(px(320.0))
                    .max_w(px(460.0))
                    .h(px(60.0))
                    .flex()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(rgb(0xff_ff_ff))
                    .border_1()
                    .border_color(BORDER_CARD)
                    .gap_3()
                    .child(div().size(px(44.0)).rounded_lg().bg(theme::BG_CANVAS))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1p5()
                            .child(
                                div()
                                    .h(px(14.0))
                                    .w(px(120.0))
                                    .rounded_md()
                                    .bg(theme::BG_CANVAS),
                            )
                            .child(
                                div()
                                    .h(px(11.0))
                                    .w(px(80.0))
                                    .rounded_md()
                                    .bg(theme::BG_CANVAS),
                            ),
                    ),
            );
        }
    }

    for track in app.online_daily_tracks.iter().take(12) {
        let remote_track = track.clone();
        let route_clone = route.clone();
        let cover_url = track.cover_url.as_deref();
        let title = track.title.clone();
        let artist_line = if !track.album.is_empty() {
            format!("{} · {}", track.artists.join("/"), track.album)
        } else {
            track.artists.join("/")
        };
        let duration = format_time(track.duration_ms.unwrap_or(0));

        let track_for_menu = remote_track.clone();
        let track_route_menu = route_clone.clone();
        let track_title_menu = title.clone();

        track_grid = track_grid.child(
            div()
                .flex_1()
                .min_w(px(320.0))
                .max_w(px(460.0))
                .h(px(60.0))
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .py_2()
                .rounded_xl()
                .bg(rgb(0xff_ff_ff))
                .border_1()
                .border_color(BORDER_CARD)
                .hover(|s| s.bg(theme::bg_hover()).border_color(ACCENT_RED))
                .transition(press_transition())
                .cursor_pointer()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(image_cache::render_remote_cover(cover_url, 44.0, 44.0, 8.0))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.0))
                                .gap(px(1.0))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1p5()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .text_color(TEXT_PRIMARY)
                                                .truncate()
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .px_1()
                                                .py_0p5()
                                                .rounded(px(3.0))
                                                .bg(hsla(348.0, 0.90, 0.95, 1.0))
                                                .border_1()
                                                .border_color(hsla(348.0, 0.90, 0.60, 0.3))
                                                .text_color(ACCENT_RED)
                                                .text_xs()
                                                .child("SQ"),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(TEXT_SECONDARY)
                                        .truncate()
                                        .child(artist_line),
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size(px(26.0))
                                .rounded_full()
                                .bg(theme::accent_red_muted())
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(themed_icon(icon!(play), 13.0, ACCENT_RED.into())),
                        )
                        .child(div().text_xs().text_color(TEXT_TERTIARY).child(duration)),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    app_listener(view, move |this, _, _, cx| {
                        this.play_online_remote_track(
                            route_clone.clone(),
                            remote_track.clone(),
                            cx,
                        );
                    }),
                )
                .on_mouse_down(
                    gpui::MouseButton::Right,
                    app_listener(view, move |this, _, window, cx| {
                        let pos = window.mouse_position();
                        this.open_context_menu(
                            pos,
                            track_title_menu.clone(),
                            crate::ui::components::modal::ContextMenuTarget::OnlineTrack {
                                route: track_route_menu.clone(),
                                track: track_for_menu.clone(),
                            },
                            cx,
                        );
                    }),
                ),
        );
    }

    let first_track = app.online_daily_tracks.first().cloned();
    let play_all_route = route.clone();

    Some(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .text_color(TEXT_PRIMARY)
                                    .child("每日推荐歌曲"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(TEXT_SECONDARY)
                                    .child("基于你的音乐品味每日更新 · 精选高保真原声"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("home-play-all-daily")
                                    .flex()
                                    .items_center()
                                    .gap_1p5()
                                    .px_4()
                                    .py_1p5()
                                    .rounded_full()
                                    .bg(ACCENT_RED)
                                    .text_color(TEXT_WHITE)
                                    .cursor_pointer()
                                    .hover(|s| s.opacity(0.90))
                                    .transition(press_transition())
                                    .child(themed_icon(icon!(play), 14.0, TEXT_WHITE.into()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::BOLD)
                                            .child("播放全部"),
                                    )
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        app_listener(view, move |this, _, _, cx| {
                                            if let Some(track) = first_track.clone() {
                                                this.play_online_remote_track(
                                                    play_all_route.clone(),
                                                    track,
                                                    cx,
                                                );
                                            }
                                        }),
                                    ),
                            )
                            .child(
                                div()
                                    .id("home-view-all-daily")
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_3()
                                    .py_1p5()
                                    .rounded_full()
                                    .bg(theme::BG_CANVAS)
                                    .border_1()
                                    .border_color(BORDER_CARD)
                                    .text_color(TEXT_PRIMARY)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::bg_hover()))
                                    .transition(press_transition())
                                    .child(themed_icon(icon!(eye), 13.0, TEXT_SECONDARY.into()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child("查看列表"),
                                    )
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        app_listener(view, |this, _, _, cx| {
                                            this.show_daily_recommendations_playlist(cx);
                                        }),
                                    ),
                            ),
                    ),
            )
            .child(track_grid)
            .into_any_element(),
    )
}

fn stats_overview(app: &MusicApp) -> impl IntoElement {
    let track_count = app.tracks.len();
    let album_count = app
        .tracks
        .iter()
        .map(|t| t.album.as_str())
        .collect::<HashSet<_>>()
        .len();
    let artist_count = app
        .tracks
        .iter()
        .map(|t| t.artist.as_str())
        .collect::<HashSet<_>>()
        .len();

    div()
        .flex()
        .items_center()
        .gap_4()
        .child(stat_badge(
            "歌曲总计",
            format!("{track_count} 首"),
            icon!(music),
        ))
        .child(stat_badge(
            "已收录专辑",
            format!("{album_count} 张"),
            icon!(disc_3),
        ))
        .child(stat_badge(
            "艺术家",
            format!("{artist_count} 位"),
            icon!(users_round),
        ))
}

fn stat_badge(label: &'static str, val: String, icon: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2p5()
        .rounded_xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .size(px(32.0))
                .rounded_lg()
                .bg(theme::accent_red_muted())
                .flex()
                .items_center()
                .justify_center()
                .child(themed_icon(icon, 16.0, ACCENT_RED.into())),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_xs().text_color(TEXT_TERTIARY).child(label))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .child(val),
                ),
        )
}

fn featured_albums_section(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let mut albums: [Option<&Track>; 8] = [None; 8];
    let mut album_count = 0_usize;
    for track in &app.tracks {
        let duplicate = albums[..album_count]
            .iter()
            .flatten()
            .any(|existing| existing.album.as_str() == track.album.as_str());
        if duplicate {
            continue;
        }
        albums[album_count] = Some(track);
        album_count += 1;
        if album_count == albums.len() {
            break;
        }
    }

    let mut grid = div().flex().flex_wrap().gap_5();
    for track in albums[..album_count].iter().flatten().copied() {
        grid = grid.child(album_card(track, app, view));
    }

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child("精选与专辑推荐"),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child("点击卡片畅享整张专辑"),
                ),
        )
        .child(grid)
}

fn album_card(track: &Track, app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let track_id = track.id;
    let artwork = app.artworks.get(&track_id).cloned();

    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(150.0))
            .rounded_xl()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track_id);
        div()
            .size(px(150.0))
            .rounded_xl()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(c1, 0.0),
                linear_color_stop(c2, 1.0),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(icon!(disc_3), 42.0, hsla(0.0, 0.0, 1.0, 0.80)))
            .into_any_element()
    };

    div()
        .id(SharedString::from(format!("home-album-card-{track_id}")))
        .w(px(150.0))
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .cursor_pointer()
        .hover(|s| s.scale(1.02))
        .transition(press_transition())
        .active(|s| s.scale(0.97))
        .child(
            div()
                .size(px(150.0))
                .rounded_xl()
                .overflow_hidden()
                .relative()
                .child(cover)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded_xl()
                        .bg(hsla(0.0, 0.0, 0.0, 0.20))
                        .opacity(0.0)
                        .hover(|s| s.opacity(1.0))
                        .transition(press_transition())
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .size(px(40.0))
                                .rounded_full()
                                .bg(ACCENT_RED)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(themed_icon(icon!(play), 20.0, hsla(0.0, 0.0, 1.0, 1.0))),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(1.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(TEXT_PRIMARY)
                        .truncate()
                        .child(track.album.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_SECONDARY)
                        .truncate()
                        .child(track.artist.clone()),
                ),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            app_listener(view, move |this, _, _, cx| this.play_track(track_id, cx)),
        )
}

fn recent_tracks_section(app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    let mut list = div().flex().flex_col().gap_1p5();

    for track in app.tracks.iter().take(8) {
        list = list.child(track_row(track, app, view));
    }

    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(TEXT_PRIMARY)
                        .child("最近收录歌曲"),
                )
                .child(
                    div()
                        .id("home-see-all-btn")
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(ACCENT_RED)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.80))
                        .child("查看全部歌曲 →")
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            app_listener(view, |this, _, _, cx| {
                                this.show_library_tab(crate::model::LibraryTab::Songs, cx);
                            }),
                        ),
                ),
        )
        .child(list)
}

pub(super) fn track_row(
    track: &Track,
    app: &MusicApp,
    view: &WeakEntity<MusicApp>,
) -> impl IntoElement {
    let track_id = track.id;
    let is_current = app
        .snapshot
        .current_track
        .as_ref()
        .is_some_and(|t| t.id == track_id);
    let is_playing = is_current && app.snapshot.state == crate::model::PlaybackState::Playing;
    let artwork = app.artworks.get(&track_id).cloned();

    let cover = if let Some(bytes) = artwork {
        img(EncodedImageBytes::new(ImageFormat::Png, bytes))
            .size(px(42.0))
            .rounded_md()
            .object_fit(ObjectFit::Cover)
            .into_any_element()
    } else {
        let (c1, c2) = elegant_gradient_for(track_id);
        div()
            .size(px(42.0))
            .rounded_md()
            .bg(linear_gradient(
                135.0,
                linear_color_stop(c1, 0.0),
                linear_color_stop(c2, 1.0),
            ))
            .flex()
            .items_center()
            .justify_center()
            .child(themed_icon(icon!(music), 18.0, hsla(0.0, 0.0, 1.0, 0.85)))
            .into_any_element()
    };

    div()
        .id(SharedString::from(format!("home-track-{track_id}")))
        .w_full()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .py_2()
        .rounded_xl()
        .cursor_pointer()
        .bg(if is_current {
            theme::accent_red_muted()
        } else {
            rgb(0xff_ff_ff).into()
        })
        .border_1()
        .border_color(if is_current {
            hsla(348.0, 0.95, 0.56, 0.25)
        } else {
            BORDER_CARD.into()
        })
        .hover(|s| s.bg(theme::bg_hover()))
        .transition(press_transition())
        .active(|s| s.scale(0.99))
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .flex_1()
                .min_w(px(0.0))
                .child(cover)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w(px(0.0))
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(if is_current {
                                    gpui::FontWeight::BOLD
                                } else {
                                    gpui::FontWeight::MEDIUM
                                })
                                .text_color(if is_current { ACCENT_RED } else { TEXT_PRIMARY })
                                .truncate()
                                .child(track.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(TEXT_SECONDARY)
                                .truncate()
                                .child(format!("{} · {}", track.artist, track.album)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child_if(is_playing, || waveform_animation(true))
                .child(
                    div()
                        .id(SharedString::from(format!("home-add-queue-{track_id}")))
                        .size(px(26.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .hover(|s| s.bg(theme::bg_active()))
                        .child(themed_icon(icon!(plus), 14.0, hsla(220.0, 0.08, 0.50, 1.0)))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            app_listener(view, move |this, _, _, cx| {
                                this.add_to_queue(track_id, cx);
                            }),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(TEXT_TERTIARY)
                        .child(format_time(track.duration_ms)),
                ),
        )
        .on_mouse_down(
            gpui::MouseButton::Left,
            app_listener(view, move |this, _, _, cx| this.play_track(track_id, cx)),
        )
}

fn empty_state(_app: &MusicApp, view: &WeakEntity<MusicApp>) -> impl IntoElement {
    div()
        .w_full()
        .p_12()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_4()
        .rounded_2xl()
        .bg(rgb(0xff_ff_ff))
        .border_1()
        .border_color(BORDER_CARD)
        .child(
            div()
                .size(px(56.0))
                .rounded_full()
                .bg(theme::accent_red_muted())
                .flex()
                .items_center()
                .justify_center()
                .child(themed_icon(icon!(folder_open), 28.0, ACCENT_RED.into())),
        )
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(TEXT_PRIMARY)
                .child("尚未添加本地歌曲"),
        )
        .child(
            div()
                .text_sm()
                .text_color(TEXT_SECONDARY)
                .child("选择一个包含音频的文件夹，音栖岛会自动提取元数据并建立本地音乐索引库"),
        )
        .child(
            div()
                .id("empty-add-folder")
                .flex()
                .items_center()
                .gap_2()
                .px_5()
                .py_2p5()
                .rounded_full()
                .cursor_pointer()
                .bg(ACCENT_RED)
                .text_color(TEXT_WHITE)
                .hover(|s| s.opacity(0.90))
                .transition(press_transition())
                .active(|s| s.scale(0.96))
                .child(themed_icon(
                    icon!(folder_plus),
                    16.0,
                    hsla(0.0, 0.0, 1.0, 1.0),
                ))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("立即选择音乐目录"),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    app_listener(view, |this, _, _, cx| this.choose_folder(cx)),
                ),
        )
        .child(
            div()
                .text_xs()
                .text_color(TEXT_TERTIARY)
                .child("全面支持 FLAC、WAV、APE、ALAC、MP3、M4A、AAC、OGG 等高保真无损格式"),
        )
}

fn current_time_greeting() -> &'static str {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hour = ((now + 8 * 3600) % 86400) / 3600;
    if hour < 12 {
        "早上好，开启今日动听旋律"
    } else if hour < 18 {
        "下午好，来一段惬意音乐时光"
    } else {
        "晚上好，让音乐为你卸下疲惫"
    }
}
