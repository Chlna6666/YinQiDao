use std::sync::Arc;

use gpui::{
    Context, EncodedImageBytes, ImageFormat, IntoElement, ObjectFit, Subscription, WeakEntity,
    Window, WindowControlArea, div, hsla, img, prelude::*, px, rgb,
};

use crate::settings::DesktopLyricsAlignment;

use super::shell::MusicApp;

#[derive(Clone)]
struct RasterizedLine {
    png: Arc<[u8]>,
    logical_width: f32,
    logical_height: f32,
}

#[derive(Clone, Default)]
struct RasterizedLyrics {
    current: Option<RasterizedLine>,
    translation: Option<RasterizedLine>,
    next: Option<RasterizedLine>,
    next_translation: Option<RasterizedLine>,
}

#[cfg(windows)]
#[derive(Clone, Debug, PartialEq)]
struct RasterCacheKey {
    current: String,
    translation: Option<String>,
    next: Option<String>,
    next_translation: Option<String>,
    font_size_bits: u32,
    active_color: u32,
    inactive_color: u32,
    translation_color: u32,
    scale_factor_bits: u32,
}

pub(crate) struct DesktopLyricsView {
    parent: WeakEntity<MusicApp>,
    bounds_subscription: Option<Subscription>,
    #[cfg(windows)]
    raster_cache: Option<(RasterCacheKey, RasterizedLyrics)>,
}

impl DesktopLyricsView {
    pub(crate) fn new(parent: WeakEntity<MusicApp>) -> Self {
        Self {
            parent,
            bounds_subscription: None,
            #[cfg(windows)]
            raster_cache: None,
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

    #[cfg(windows)]
    fn rasterized_lyrics(
        &mut self,
        current: &str,
        translation: Option<&str>,
        next: Option<&str>,
        next_translation: Option<&str>,
        font_size: f32,
        active_color: u32,
        inactive_color: u32,
        translation_color: u32,
        scale_factor: f32,
    ) -> RasterizedLyrics {
        let scale_factor = scale_factor.clamp(0.75, 4.0);
        let key = RasterCacheKey {
            current: current.to_owned(),
            translation: translation.map(str::to_owned),
            next: next.map(str::to_owned),
            next_translation: next_translation.map(str::to_owned),
            font_size_bits: font_size.to_bits(),
            active_color,
            inactive_color,
            translation_color,
            scale_factor_bits: scale_factor.to_bits(),
        };
        if let Some((cached_key, cached)) = &self.raster_cache
            && *cached_key == key
        {
            return cached.clone();
        }

        let rasterized = RasterizedLyrics {
            current: rasterize_windows_text(
                current,
                font_size,
                active_color,
                true,
                scale_factor,
            ),
            translation: translation.and_then(|text| {
                rasterize_windows_text(
                    text,
                    (font_size * 0.52).max(13.0),
                    translation_color,
                    false,
                    scale_factor,
                )
            }),
            next: next.and_then(|text| {
                rasterize_windows_text(
                    text,
                    (font_size * 0.66).max(15.0),
                    inactive_color,
                    false,
                    scale_factor,
                )
            }),
            next_translation: next_translation.and_then(|text| {
                rasterize_windows_text(
                    text,
                    (font_size * 0.44).max(12.0),
                    translation_color,
                    false,
                    scale_factor,
                )
            }),
        };
        self.raster_cache = Some((key, rasterized.clone()));
        rasterized
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

        #[cfg(windows)]
        let rasterized = self.rasterized_lyrics(
            &current,
            translation.as_deref(),
            next.as_deref(),
            next_translation.as_deref(),
            config.font_size,
            config.active_color,
            config.inactive_color,
            config.translation_color,
            window.scale_factor(),
        );
        #[cfg(not(windows))]
        let rasterized = RasterizedLyrics::default();

        let parent_weak = self.parent.clone();
        let close_parent = self.parent.clone();
        let translation_parent = self.parent.clone();

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
            rasterized.current,
            config.alignment,
            config.font_size,
            config.active_color,
            gpui::FontWeight::SEMIBOLD,
            1.0,
        ));

        if let Some(translation) = translation {
            lyrics = lyrics.child(aligned_line(
                translation,
                rasterized.translation,
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
                rasterized.next,
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
                rasterized.next_translation,
                config.alignment,
                (config.font_size * 0.44).max(12.0),
                config.translation_color,
                gpui::FontWeight::NORMAL,
                0.58,
            ));
        }

        let toolbar = div()
            .absolute()
            .top(px(7.0))
            .right(px(8.0))
            .flex()
            .items_center()
            .gap(px(5.0))
            .window_control_area(WindowControlArea::Client)
            .child(toolbar_button(
                "desktop-lyrics-lock",
                if config.locked { "解锁" } else { "锁定" },
                move |_, _window, cx| {
                    let _ = parent_weak.update(cx, |app, app_cx| {
                        app.toggle_desktop_lyrics_lock(app_cx);
                    });
                },
            ))
            .child(toolbar_button(
                "desktop-lyrics-translation",
                if config.show_translation { "译" } else { "原" },
                move |_, _window, cx| {
                    let _ = translation_parent.update(cx, |app, app_cx| {
                        app.toggle_desktop_lyrics_translation(app_cx);
                    });
                },
            ))
            .child(toolbar_button(
                "desktop-lyrics-close",
                "×",
                move |_, window, cx| {
                    window.remove_window();
                    let _ = close_parent.update(cx, |app, app_cx| {
                        app.desktop_lyrics_window_closed(app_cx);
                    });
                },
            ));

        let mut root = div()
            .id("desktop-lyrics-root")
            .size_full()
            .relative()
            .overflow_hidden()
            .bg(hsla(0.0, 0.0, 0.0, 0.0))
            .px(px(24.0))
            .py(px(12.0))
            .child(lyrics)
            .child(toolbar);

        if !config.locked {
            root = root
                .window_control_area(WindowControlArea::Drag)
                .cursor_move();
        }

        root.into_any_element()
    }
}

fn aligned_line(
    text: String,
    rasterized: Option<RasterizedLine>,
    alignment: DesktopLyricsAlignment,
    font_size: f32,
    color: u32,
    weight: gpui::FontWeight,
    opacity: f32,
) -> gpui::AnyElement {
    let content = if let Some(rasterized) = rasterized {
        img(EncodedImageBytes::new(ImageFormat::Png, rasterized.png))
            .w(px(rasterized.logical_width))
            .h(px(rasterized.logical_height))
            .max_w(px(1_420.0))
            .object_fit(ObjectFit::Contain)
            .into_any_element()
    } else {
        div()
            .max_w(px(1_420.0))
            .min_w(px(0.0))
            .text_size(px(font_size))
            .font_weight(weight)
            .text_color(rgb(color & 0x00ff_ffff))
            .truncate()
            .child(text)
            .into_any_element()
    };

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
        // Keep the destination opaque on Windows. ClearType/subpixel text is valid over an opaque
        // button, whereas rendering it directly into transparent swapchain pixels causes RGB
        // fringes/color blocks.
        .bg(rgb(0x2c_2c_2e))
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(rgb(0xff_ff_ff))
        .hover(|style| style.bg(rgb(0x1c_1c_1e)))
        .on_mouse_down(gpui::MouseButton::Left, move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        })
        .child(label)
}

#[cfg(windows)]
struct DesktopFontSet {
    regular: Vec<fontdue::Font>,
    emphasis: Vec<fontdue::Font>,
}

#[cfg(windows)]
fn desktop_font_set() -> Option<&'static DesktopFontSet> {
    use std::sync::OnceLock;

    static FONTS: OnceLock<Option<DesktopFontSet>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let fonts_dir = std::path::PathBuf::from(std::env::var_os("WINDIR")?).join("Fonts");
            let regular = load_windows_fonts(
                &fonts_dir,
                &[
                    "msyh.ttc",
                    "msjh.ttc",
                    "simsun.ttc",
                    "malgun.ttf",
                    "segoeui.ttf",
                    "arial.ttf",
                ],
            );
            let emphasis = load_windows_fonts(
                &fonts_dir,
                &[
                    "msyhbd.ttc",
                    "msjhbd.ttc",
                    "seguisb.ttf",
                    "segoeuib.ttf",
                    "arialbd.ttf",
                    "msyh.ttc",
                    "segoeui.ttf",
                ],
            );
            if regular.is_empty() && emphasis.is_empty() {
                None
            } else {
                Some(DesktopFontSet { regular, emphasis })
            }
        })
        .as_ref()
}

#[cfg(windows)]
fn load_windows_fonts(dir: &std::path::Path, candidates: &[&str]) -> Vec<fontdue::Font> {
    let mut fonts = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let Ok(bytes) = std::fs::read(dir.join(candidate)) else {
            continue;
        };
        let Ok(font) = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()) else {
            continue;
        };
        fonts.push(font);
    }
    fonts
}

#[cfg(windows)]
fn rasterize_windows_text(
    text: &str,
    logical_font_size: f32,
    color: u32,
    emphasis: bool,
    scale_factor: f32,
) -> Option<RasterizedLine> {
    use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};
    use image::ImageEncoder as _;

    let font_set = desktop_font_set()?;
    let fonts = if emphasis && !font_set.emphasis.is_empty() {
        &font_set.emphasis
    } else if !font_set.regular.is_empty() {
        &font_set.regular
    } else {
        &font_set.emphasis
    };
    if fonts.is_empty() || text.is_empty() {
        return None;
    }

    let physical_font_size = (logical_font_size * scale_factor).max(1.0);
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings::default());

    append_with_font_fallback(&mut layout, fonts, text, physical_font_size);
    let glyphs = layout.glyphs();
    if glyphs.is_empty() {
        return None;
    }

    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for glyph in glyphs.iter().filter(|glyph| glyph.width != 0 && glyph.height != 0) {
        min_x = min_x.min(glyph.x);
        min_y = min_y.min(glyph.y);
        max_x = max_x.max(glyph.x + glyph.width as f32);
        max_y = max_y.max(glyph.y + glyph.height as f32);
    }
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return None;
    }

    let pad = (2.0 * scale_factor).ceil().max(2.0) as i32;
    let origin_x = min_x.floor() as i32 - pad;
    let origin_y = min_y.floor() as i32 - pad;
    let width = (max_x.ceil() as i32 - origin_x + pad).max(1) as usize;
    let height = (max_y.ceil() as i32 - origin_y + pad).max(1) as usize;
    if width > 16_384 || height > 2_048 {
        return None;
    }

    let mut rgba = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
    let red = ((color >> 16) & 0xff) as u8;
    let green = ((color >> 8) & 0xff) as u8;
    let blue = (color & 0xff) as u8;

    for glyph in glyphs {
        if glyph.width == 0 || glyph.height == 0 || glyph.font_index >= fonts.len() {
            continue;
        }
        let (_metrics, alpha) = fonts[glyph.font_index].rasterize_config(glyph.key);
        if alpha.is_empty() {
            continue;
        }
        let dst_x = glyph.x.floor() as i32 - origin_x;
        let dst_y = glyph.y.floor() as i32 - origin_y;
        for y in 0..glyph.height {
            let py = dst_y + y as i32;
            if !(0..height as i32).contains(&py) {
                continue;
            }
            for x in 0..glyph.width {
                let px = dst_x + x as i32;
                if !(0..width as i32).contains(&px) {
                    continue;
                }
                let src = alpha.get(y * glyph.width + x).copied().unwrap_or(0);
                if src == 0 {
                    continue;
                }
                let offset = (py as usize * width + px as usize) * 4;
                let dst_alpha = rgba[offset + 3] as u16;
                let src_alpha = src as u16;
                let combined = src_alpha + (dst_alpha * (255 - src_alpha) + 127) / 255;
                rgba[offset] = red;
                rgba[offset + 1] = green;
                rgba[offset + 2] = blue;
                rgba[offset + 3] = combined.min(255) as u8;
            }
        }
    }

    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &rgba,
            width as u32,
            height as u32,
            image::ColorType::Rgba8.into(),
        )
        .ok()?;

    Some(RasterizedLine {
        png: Arc::<[u8]>::from(png),
        logical_width: width as f32 / scale_factor,
        logical_height: height as f32 / scale_factor,
    })
}

#[cfg(windows)]
fn append_with_font_fallback(
    layout: &mut fontdue::layout::Layout,
    fonts: &[fontdue::Font],
    text: &str,
    font_size: f32,
) {
    use fontdue::layout::TextStyle;

    let mut segment_start = 0usize;
    let mut segment_font = None::<usize>;
    for (byte_offset, ch) in text.char_indices() {
        let font_index = if ch.is_whitespace() {
            segment_font.unwrap_or(0)
        } else {
            fonts
                .iter()
                .position(|font| font.has_glyph(ch))
                .unwrap_or_else(|| segment_font.unwrap_or(0))
        };

        match segment_font {
            None => segment_font = Some(font_index),
            Some(current) if current != font_index => {
                if byte_offset > segment_start {
                    layout.append(
                        fonts,
                        &TextStyle::new(&text[segment_start..byte_offset], font_size, current),
                    );
                }
                segment_start = byte_offset;
                segment_font = Some(font_index);
            }
            Some(_) => {}
        }
    }

    if segment_start < text.len() {
        layout.append(
            fonts,
            &TextStyle::new(
                &text[segment_start..],
                font_size,
                segment_font.unwrap_or(0),
            ),
        );
    }
}
