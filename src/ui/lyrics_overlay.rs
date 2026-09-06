use std::sync::{Arc, OnceLock};

use gpui::{
    Context, GpuMesh3d, GpuMesh3dDrawParameters, GpuMesh3dDrawRanges, GpuMesh3dRange,
    GpuMesh3dShader, GpuMesh3dVertex, IntoElement, Subscription, WeakEntity, WgslShaderSource,
    Window, WindowControlArea, canvas, div, hsla, prelude::*, px, rgb,
};

use crate::settings::DesktopLyricsAlignment;

use super::shell::MusicApp;

const INTERACTION_BACKGROUND_OPACITY: f32 = 0.36;
const LIQUID_GLASS_CORNER_RADIUS: f32 = 18.0;
const CONTROL_LANE_WIDTH: f32 = 224.0;
const LIQUID_GLASS_SHADER_SOURCE: &str = include_str!("lyrics_liquid_glass.wgsl");

pub(crate) struct DesktopLyricsView {
    parent: WeakEntity<MusicApp>,
    bounds_subscription: Option<Subscription>,
    hovered: bool,
    settings_open: bool,
}

impl DesktopLyricsView {
    pub(crate) fn new(parent: WeakEntity<MusicApp>) -> Self {
        Self {
            parent,
            bounds_subscription: None,
            hovered: false,
            settings_open: false,
        }
    }

    fn attach_bounds_observer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bounds_subscription.is_some() {
            return;
        }
        let parent = self.parent.clone();
        self.bounds_subscription = Some(cx.observe_window_bounds(window, move |_view, window, cx| {
            let bounds = window.bounds();
            let _ = parent.update(cx, |app, _cx| {
                app.persist_desktop_lyrics_bounds(bounds);
            });
        }));
    }
}

impl gpui::Render for DesktopLyricsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.attach_bounds_observer(window, cx);

        let Some(parent) = self.parent.upgrade() else {
            return div().size_full().into_any_element();
        };
        let app = parent.read(cx);
        let config = app.config.desktop_lyrics.clone();
        let display = app.desktop_lyrics_display();
        let current = display
            .as_ref()
            .map_or_else(|| "暂无同步歌词".to_string(), |lyrics| lyrics.current.clone());
        let translation = display
            .as_ref()
            .and_then(|lyrics| lyrics.translation.clone())
            .filter(|text| config.show_translation && !text.trim().is_empty());
        let next = display
            .as_ref()
            .and_then(|lyrics| lyrics.next.clone())
            .filter(|text| config.two_line && !text.trim().is_empty());
        let next_translation = display
            .as_ref()
            .and_then(|lyrics| lyrics.next_translation.clone())
            .filter(|text| config.two_line && config.show_translation && !text.trim().is_empty());
        let interacting = self.hovered || self.settings_open;
        let background_opacity = if interacting {
            config
                .background_opacity
                .max(INTERACTION_BACKGROUND_OPACITY)
        } else {
            config.background_opacity
        }
        .clamp(0.0, 0.85);

        let mut lyrics = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(3.0));

        lyrics = match config.alignment {
            DesktopLyricsAlignment::Left => lyrics.items_start(),
            DesktopLyricsAlignment::Center => lyrics.items_center(),
            DesktopLyricsAlignment::Right => lyrics.items_end(),
        };

        lyrics = lyrics.child(aligned_line(
            current,
            config.alignment,
            config.font_size,
            config.active_color,
            gpui::FontWeight::SEMIBOLD,
            1.0,
        ));

        if let Some(translation) = translation {
            lyrics = lyrics.child(aligned_line(
                translation,
                config.alignment,
                (config.font_size * 0.52).max(13.0),
                config.translation_color,
                gpui::FontWeight::MEDIUM,
                0.90,
            ));
        }
        if let Some(next) = next {
            lyrics = lyrics.child(aligned_line(
                next,
                config.alignment,
                (config.font_size * 0.66).max(15.0),
                config.inactive_color,
                gpui::FontWeight::MEDIUM,
                0.70,
            ));
        }
        if let Some(next_translation) = next_translation {
            lyrics = lyrics.child(aligned_line(
                next_translation,
                config.alignment,
                (config.font_size * 0.44).max(12.0),
                config.translation_color,
                gpui::FontWeight::NORMAL,
                0.58,
            ));
        }

        let mut root = div()
            .id("desktop-lyrics-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .window_control_area(WindowControlArea::Client)
            .on_hover(cx.listener(|this, hovered: &bool, _window, cx| {
                if this.hovered != *hovered {
                    this.hovered = *hovered;
                    cx.notify();
                }
            }));

        // Do not rely on WindowControlArea::Drag here. The root itself owns an interactive hover
        // hitbox, and GPUI intentionally lets the frontmost interactive hitbox suppress a drag
        // control area behind it. Starting the platform move explicitly from the root makes the
        // unlocked state deterministic while button/menu children keep using stop_propagation().
        if !config.locked {
            root = root.on_mouse_down(
                gpui::MouseButton::Left,
                move |event: &gpui::MouseDownEvent, window, cx| {
                    let drag_right = window.bounds().size.width - px(CONTROL_LANE_WIDTH);
                    if event.position.x < drag_right {
                        cx.stop_propagation();
                        window.start_window_move();
                    }
                },
            );
        }

        if background_opacity > 0.001 {
            root = root.child(liquid_glass_surface(background_opacity, interacting));
        }

        root = root.child(
            div()
                .absolute()
                .inset_0()
                .px(px(24.0))
                .py(px(12.0))
                .flex()
                .child(lyrics),
        );

        if interacting {
            let lock_parent = self.parent.clone();
            let translation_parent = self.parent.clone();
            let close_parent = self.parent.clone();
            let settings_view = cx.weak_entity();
            let toolbar = div()
                .absolute()
                .top(px(7.0))
                .right(px(8.0))
                .flex()
                .items_center()
                .gap(px(5.0))
                .occlude()
                .window_control_area(WindowControlArea::Client)
                .child(toolbar_button(
                    "desktop-lyrics-lock",
                    if config.locked { "解" } else { "锁" },
                    move |_, _window, cx| {
                        let _ = lock_parent.update(cx, |app, app_cx| {
                            app.toggle_desktop_lyrics_lock(app_cx);
                        });
                    },
                ))
                .child(toolbar_button(
                    "desktop-lyrics-translation",
                    "译",
                    move |_, _window, cx| {
                        let _ = translation_parent.update(cx, |app, app_cx| {
                            app.toggle_desktop_lyrics_translation(app_cx);
                        });
                    },
                ))
                .child(toolbar_button(
                    "desktop-lyrics-settings",
                    "⋯",
                    move |_, _window, cx| {
                        let _ = settings_view.update(cx, |view, view_cx| {
                            view.settings_open = !view.settings_open;
                            view_cx.notify();
                        });
                    },
                ))
                .child(toolbar_button(
                    "desktop-lyrics-close",
                    "×",
                    move |_, window, cx| {
                        let _ = close_parent.update(cx, |app, app_cx| {
                            app.desktop_lyrics_window_closed(app_cx);
                        });
                        window.remove_window();
                    },
                ));
            root = root.child(toolbar);
        }

        if self.settings_open {
            root = root.child(settings_panel(&self.parent, &config));
        }

        root.into_any_element()
    }
}

fn aligned_line(
    text: String,
    alignment: DesktopLyricsAlignment,
    font_size: f32,
    color: u32,
    weight: gpui::FontWeight,
    opacity: f32,
) -> gpui::AnyElement {
    let content = div()
        .max_w(px(1_420.0))
        .min_w(px(0.0))
        .text_size(px(font_size))
        .font_weight(weight)
        .text_color(rgb(color & 0x00ff_ffff))
        .truncate()
        .child(text);

    let row = div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .items_center()
        .opacity(opacity)
        .child(content);
    match alignment {
        DesktopLyricsAlignment::Left => row.justify_start(),
        DesktopLyricsAlignment::Center => row.justify_center(),
        DesktopLyricsAlignment::Right => row.justify_end(),
    }
    .into_any_element()
}

fn toolbar_button(
    id: &'static str,
    label: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .occlude()
        .window_control_area(WindowControlArea::Client)
        .min_w(px(28.0))
        .h(px(24.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .bg(hsla(0.0, 0.0, 0.10, 0.80))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(0xff_ff_ff))
        .hover(|style| style.bg(hsla(0.0, 0.0, 0.04, 0.92)))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        })
        .child(label)
}

fn settings_panel(
    parent: &WeakEntity<MusicApp>,
    config: &crate::settings::DesktopLyricsConfig,
) -> impl IntoElement {
    let topmost_parent = parent.clone();
    let two_line_parent = parent.clone();
    let translation_parent = parent.clone();
    let alignment_parent = parent.clone();
    let background_parent = parent.clone();
    let alignment_label = match config.alignment {
        DesktopLyricsAlignment::Left => "左",
        DesktopLyricsAlignment::Center => "中",
        DesktopLyricsAlignment::Right => "右",
    };

    div()
        .absolute()
        .top(px(35.0))
        .right(px(8.0))
        .w(px(190.0))
        .p(px(6.0))
        .rounded(px(12.0))
        .occlude()
        .window_control_area(WindowControlArea::Client)
        .bg(hsla(0.0, 0.0, 0.16, 0.92))
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.16))
        .shadow_md()
        .flex()
        .flex_col()
        .child(settings_row(
            "desktop-lyrics-menu-topmost",
            "总在最前",
            if config.always_on_top { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = topmost_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_topmost(app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-two-line",
            "切换双行模式",
            if config.two_line { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = two_line_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_two_line(app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-translation",
            "外文歌词显示",
            if config.show_translation { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = translation_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_translation(app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-alignment",
            "对齐方式",
            alignment_label,
            move |_, _window, cx| {
                let _ = alignment_parent.update(cx, |app, app_cx| {
                    let next = match app.config.desktop_lyrics.alignment {
                        DesktopLyricsAlignment::Left => DesktopLyricsAlignment::Center,
                        DesktopLyricsAlignment::Center => DesktopLyricsAlignment::Right,
                        DesktopLyricsAlignment::Right => DesktopLyricsAlignment::Left,
                    };
                    app.set_desktop_lyrics_alignment(next, app_cx);
                });
            },
        ))
        .child(settings_row(
            "desktop-lyrics-menu-background",
            "显示透明背景",
            if config.background_opacity > 0.01 { "✓" } else { "" },
            move |_, _window, cx| {
                let _ = background_parent.update(cx, |app, app_cx| {
                    app.toggle_desktop_lyrics_background(app_cx);
                });
            },
        ))
}

fn settings_row(
    id: &'static str,
    label: &'static str,
    value: &'static str,
    handler: impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(20.0))
        .px(px(7.0))
        .rounded(px(7.0))
        .flex()
        .items_center()
        .justify_between()
        .cursor_pointer()
        .window_control_area(WindowControlArea::Client)
        .text_xs()
        .text_color(rgb(0xf2_f2_f7))
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.10)))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        })
        .child(label)
        .child(
            div()
                .min_w(px(18.0))
                .text_right()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(0xff_ff_ff))
                .child(value),
        )
}

fn liquid_glass_surface(opacity: f32, interacting: bool) -> gpui::AnyElement {
    match lyrics_liquid_glass_mesh() {
        Ok(mesh) => {
            let mut metadata = [[0.0_f32; 4]; 4];
            metadata[0][0] = opacity;
            metadata[0][1] = if interacting { 1.0 } else { 0.0 };
            metadata[0][2] = LIQUID_GLASS_CORNER_RADIUS;
            metadata[3][3] = 1.0;
            let parameters = GpuMesh3dDrawParameters {
                view_projection_model: metadata,
            };
            canvas(
                move |bounds, _window, _cx| bounds,
                move |bounds, _prepaint, window, _cx| {
                    window.paint_gpu_mesh_3d(bounds, mesh.clone(), parameters);
                },
            )
            .absolute()
            .inset_0()
            .into_any_element()
        }
        Err(_error) => div()
            .absolute()
            .inset_0()
            .rounded(px(LIQUID_GLASS_CORNER_RADIUS))
            .bg(hsla(0.0, 0.0, 0.18, opacity))
            .into_any_element(),
    }
}

fn lyrics_liquid_glass_mesh() -> Result<Arc<GpuMesh3d>, String> {
    static MESH: OnceLock<Result<Arc<GpuMesh3d>, String>> = OnceLock::new();
    MESH.get_or_init(|| {
        let result = build_lyrics_liquid_glass_mesh();
        if let Err(error) = &result {
            tracing::warn!(error = %error, "桌面歌词 Liquid Glass shader 不可用，回退透明灰背景");
        }
        result
    })
    .clone()
}

fn build_lyrics_liquid_glass_mesh() -> Result<Arc<GpuMesh3d>, String> {
    let source = WgslShaderSource::from_source(
        "src/ui/lyrics_liquid_glass.wgsl",
        LIQUID_GLASS_SHADER_SOURCE,
    )
    .map_err(|error| error.to_string())?;
    let shader = Arc::new(GpuMesh3dShader::new(
        Arc::new(source),
        "vs_lyrics_liquid_glass",
        "fs_lyrics_liquid_glass",
    ));
    let vertices = vec![
        GpuMesh3dVertex {
            position: [-1.0, -1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
        GpuMesh3dVertex {
            position: [1.0, -1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
        GpuMesh3dVertex {
            position: [1.0, 1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
        GpuMesh3dVertex {
            position: [-1.0, 1.0, 0.0],
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ];
    let indices = vec![0_u32, 1, 2, 0, 2, 3];
    let mesh = GpuMesh3d::new(
        Arc::from(vertices.into_boxed_slice()),
        Arc::from(indices.into_boxed_slice()),
        GpuMesh3dDrawRanges {
            opaque: GpuMesh3dRange::default(),
            glass: GpuMesh3dRange { start: 0, count: 6 },
            water: GpuMesh3dRange::default(),
        },
        [0.0, 0.0, 0.0],
        1.0,
        1.0,
        shader,
    );
    Ok(Arc::new(mesh))
}
