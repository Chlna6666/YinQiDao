use gpui::{
    App, AppContext, Bounds, Context, Timer, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, point, px, size,
};

use crate::{
    hotkeys::LyricsHotkeyAction,
    settings::DesktopLyricsAlignment,
    ui::{MusicApp, lyrics_overlay::DesktopLyricsView},
};

const LYRICS_UI_TICK: std::time::Duration = std::time::Duration::from_millis(80);
const MIN_OVERLAY_WIDTH: f32 = 420.0;
const MIN_OVERLAY_HEIGHT: f32 = 92.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LyricsUiKey {
    track_id: Option<i64>,
    line_index: Option<usize>,
    content_hash: u64,
    style_hash: u64,
    desktop_visible: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct LyricsDisplay {
    pub current: String,
    pub translation: Option<String>,
    pub next: Option<String>,
    pub next_translation: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LyricsColorTarget {
    Active,
    Inactive,
    Translation,
}

impl MusicApp {
    pub(crate) fn sync_desktop_lyrics_window(&mut self, cx: &mut Context<Self>) {
        let existing = find_overlay_window(cx);
        if !self.config.desktop_lyrics.visible {
            if let Some(window) = existing {
                let _ = window.update(cx, |_view, window, _cx| window.remove_window());
            }
            return;
        }
        if let Some(window) = existing {
            let _ = window.update(cx, |_view, window, _cx| window.show_window());
            return;
        }

        let config = self.config.desktop_lyrics.clone();
        let width = config.width.clamp(MIN_OVERLAY_WIDTH, 1_600.0);
        let height = config.height.clamp(MIN_OVERLAY_HEIGHT, 520.0);
        let window_bounds = match (config.x, config.y) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => WindowBounds::Windowed(Bounds {
                origin: point(px(x), px(y)),
                size: size(px(width), px(height)),
            }),
            _ => WindowBounds::Windowed(Bounds::centered(None, size(px(width), px(height)), cx)),
        };
        let parent = cx.entity().downgrade();
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(window_bounds),
            window_min_size: Some(size(px(MIN_OVERLAY_WIDTH), px(MIN_OVERLAY_HEIGHT))),
            // Windows uses a borderless popup and applies HWND_TOPMOST explicitly below. Other
            // platforms keep the previous popup/normal behavior until they have a native adapter.
            kind: if cfg!(windows) || config.always_on_top {
                WindowKind::PopUp
            } else {
                WindowKind::Normal
            },
            focus: false,
            is_movable: true,
            is_resizable: true,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };

        match cx.open_window(options, move |_, cx| {
            cx.new(|_| DesktopLyricsView::new(parent))
        }) {
            Ok(window) => {
                let always_on_top = config.always_on_top;
                let applied = window
                    .update(cx, |_view, window, _cx| {
                        crate::window_platform::set_always_on_top(window, always_on_top)
                    })
                    .unwrap_or(false);
                #[cfg(windows)]
                if !applied {
                    tracing::warn!("桌面歌词 HWND 置顶状态应用失败");
                }
            }
            Err(error) => {
                self.status = format!("打开桌面歌词失败：{error:#}");
                self.config.desktop_lyrics.visible = false;
                self.save_config();
            }
        }
    }

    fn recreate_desktop_lyrics_window(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = find_overlay_window(cx) {
            let _ = window.update(cx, |_view, window, _cx| window.remove_window());
        }
        if self.config.desktop_lyrics.visible {
            self.sync_desktop_lyrics_window(cx);
        }
    }

    pub(crate) fn toggle_desktop_lyrics_visible(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.visible = !self.config.desktop_lyrics.visible;
        self.save_config();
        self.sync_desktop_lyrics_window(cx);
        cx.notify();
    }

    pub(crate) fn desktop_lyrics_window_closed(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.visible = false;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_desktop_lyrics_lock(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.locked = !self.config.desktop_lyrics.locked;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_desktop_lyrics_topmost(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.always_on_top = !self.config.desktop_lyrics.always_on_top;
        let always_on_top = self.config.desktop_lyrics.always_on_top;
        self.save_config();

        #[cfg(windows)]
        if let Some(overlay) = find_overlay_window(cx) {
            let applied = overlay
                .update(cx, |_view, window, _cx| {
                    crate::window_platform::set_always_on_top(window, always_on_top)
                })
                .unwrap_or(false);
            if !applied {
                self.status = "桌面歌词置顶状态应用失败".into();
            }
        }

        #[cfg(not(windows))]
        self.recreate_desktop_lyrics_window(cx);

        cx.notify();
    }

    pub(crate) fn toggle_desktop_lyrics_translation(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.show_translation = !self.config.desktop_lyrics.show_translation;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn toggle_desktop_lyrics_two_line(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.two_line = !self.config.desktop_lyrics.two_line;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_desktop_lyrics_font(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.font_size =
            (self.config.desktop_lyrics.font_size + delta).clamp(18.0, 64.0);
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_desktop_lyrics_background(
        &mut self,
        delta: f32,
        cx: &mut Context<Self>,
    ) {
        self.config.desktop_lyrics.background_opacity =
            (self.config.desktop_lyrics.background_opacity + delta).clamp(0.0, 0.85);
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_desktop_lyrics_alignment(
        &mut self,
        alignment: DesktopLyricsAlignment,
        cx: &mut Context<Self>,
    ) {
        self.config.desktop_lyrics.alignment = alignment;
        self.save_config();
        cx.notify();
    }

    pub(crate) fn set_desktop_lyrics_color(
        &mut self,
        target: LyricsColorTarget,
        color: u32,
        cx: &mut Context<Self>,
    ) {
        let color = color & 0x00ff_ffff;
        match target {
            LyricsColorTarget::Active => self.config.desktop_lyrics.active_color = color,
            LyricsColorTarget::Inactive => self.config.desktop_lyrics.inactive_color = color,
            LyricsColorTarget::Translation => self.config.desktop_lyrics.translation_color = color,
        }
        self.save_config();
        cx.notify();
    }

    pub(crate) fn reset_desktop_lyrics_bounds(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.x = None;
        self.config.desktop_lyrics.y = None;
        self.config.desktop_lyrics.width = 760.0;
        self.config.desktop_lyrics.height = 148.0;
        self.save_config();
        self.recreate_desktop_lyrics_window(cx);
        cx.notify();
    }

    pub(crate) fn persist_desktop_lyrics_bounds(&mut self, bounds: Bounds<gpui::Pixels>) {
        let x = f32::from(bounds.origin.x);
        let y = f32::from(bounds.origin.y);
        let width = f32::from(bounds.size.width).max(MIN_OVERLAY_WIDTH);
        let height = f32::from(bounds.size.height).max(MIN_OVERLAY_HEIGHT);
        let config = &mut self.config.desktop_lyrics;
        let changed = config.x.is_none_or(|old| (old - x).abs() >= 0.5)
            || config.y.is_none_or(|old| (old - y).abs() >= 0.5)
            || (config.width - width).abs() >= 0.5
            || (config.height - height).abs() >= 0.5;
        if !changed {
            return;
        }
        config.x = Some(x);
        config.y = Some(y);
        config.width = width;
        config.height = height;
        self.save_config();
    }

    pub(crate) fn toggle_lyrics_shortcuts(&mut self, cx: &mut Context<Self>) {
        self.config.lyrics_shortcuts.enabled = !self.config.lyrics_shortcuts.enabled;
        crate::hotkeys::set_enabled(self.config.lyrics_shortcuts.enabled);
        self.save_config();
        self.status = if self.config.lyrics_shortcuts.enabled {
            "系统级全局快捷键已启用".into()
        } else {
            "系统级全局快捷键已关闭".into()
        };
        cx.notify();
    }

    pub(crate) fn apply_lyrics_hotkey(
        &mut self,
        action: LyricsHotkeyAction,
        cx: &mut Context<Self>,
    ) {
        if !self.config.lyrics_shortcuts.enabled {
            return;
        }
        match action {
            LyricsHotkeyAction::ToggleVisible => self.toggle_desktop_lyrics_visible(cx),
            LyricsHotkeyAction::ToggleLock => self.toggle_desktop_lyrics_lock(cx),
            LyricsHotkeyAction::ToggleTranslation => self.toggle_desktop_lyrics_translation(cx),
            LyricsHotkeyAction::IncreaseFont => self.adjust_desktop_lyrics_font(2.0, cx),
            LyricsHotkeyAction::DecreaseFont => self.adjust_desktop_lyrics_font(-2.0, cx),
        }
    }

    pub(crate) fn desktop_lyrics_display(&self) -> Option<LyricsDisplay> {
        let track = self.snapshot.current_track.as_ref()?;
        let document = self.lyrics.get(&track.id)?;
        let lines = document.timed_lines();
        if lines.is_empty() {
            let current = document
                .plain
                .as_deref()?
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())?
                .to_owned();
            return Some(LyricsDisplay {
                current,
                translation: None,
                next: None,
                next_translation: None,
            });
        }

        let position_ms = self
            .engine
            .as_ref()
            .map_or(self.snapshot.position_ms, |engine| engine.progress().1);
        let index = lines
            .iter()
            .rposition(|line| line.timestamp_ms <= position_ms)
            .unwrap_or(0);
        let current = &lines[index];
        let next = lines.get(index + 1);
        Some(LyricsDisplay {
            current: current.text.clone(),
            translation: current
                .translation
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned),
            next: next.map(|line| line.text.clone()),
            next_translation: next.and_then(|line| {
                line.translation
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .map(str::to_owned)
            }),
        })
    }

    pub(crate) fn lyrics_ui_key(&self) -> LyricsUiKey {
        let track_id = self.snapshot.current_track.as_ref().map(|track| track.id);
        let position_ms = self
            .engine
            .as_ref()
            .map_or(self.snapshot.position_ms, |engine| engine.progress().1);
        let (line_index, content_hash) = track_id
            .and_then(|id| self.lyrics.get(&id))
            .map(|document| {
                let lines = document.timed_lines();
                if lines.is_empty() {
                    let text = document.plain.as_deref().unwrap_or_default();
                    (None, hash_text(text))
                } else {
                    let index = lines
                        .iter()
                        .rposition(|line| line.timestamp_ms <= position_ms)
                        .unwrap_or(0);
                    let mut hash = hash_text(&lines[index].text);
                    if let Some(translation) = lines[index].translation.as_deref() {
                        hash = mix_hash(hash, hash_text(translation));
                    }
                    if self.config.desktop_lyrics.two_line
                        && let Some(next) = lines.get(index + 1)
                    {
                        hash = mix_hash(hash, hash_text(&next.text));
                        if let Some(translation) = next.translation.as_deref() {
                            hash = mix_hash(hash, hash_text(translation));
                        }
                    }
                    (Some(index), hash)
                }
            })
            .unwrap_or((None, 0));

        let config = &self.config.desktop_lyrics;
        let mut style_hash = 0xcbf2_9ce4_8422_2325_u64;
        for value in [
            u64::from(config.active_color),
            u64::from(config.inactive_color),
            u64::from(config.translation_color),
            u64::from(config.font_size.to_bits()),
            config.show_translation as u64,
            config.two_line as u64,
            config.locked as u64,
            config.always_on_top as u64,
            config.alignment as u64,
        ] {
            style_hash = mix_hash(style_hash, value);
        }

        LyricsUiKey {
            track_id,
            line_index,
            content_hash,
            style_hash,
            desktop_visible: config.visible,
        }
    }
}

pub(crate) fn start_ui_service(main_window: WindowHandle<MusicApp>, cx: &mut App) {
    cx.spawn(async move |cx| -> anyhow::Result<()> {
        let mut last_key: Option<LyricsUiKey> = None;
        loop {
            Timer::after(LYRICS_UI_TICK).await;
            let actions = crate::hotkeys::drain_actions();
            let still_open = cx.update(|cx| {
                let mut changed = false;
                let result = main_window.update(cx, |app, _window, app_cx| {
                    for action in actions {
                        app.apply_lyrics_hotkey(action, app_cx);
                    }
                    app.sync_desktop_lyrics_window(app_cx);
                    let key = app.lyrics_ui_key();
                    changed = last_key.as_ref() != Some(&key);
                    last_key = Some(key);
                });
                if result.is_err() {
                    return false;
                }
                if changed
                    && let Some(window) = find_overlay_window(cx)
                {
                    let _ = window.update(cx, |_view, _window, view_cx| view_cx.notify());
                }
                true
            })?;
            if !still_open {
                break;
            }
        }
        Ok(())
    })
    .detach();
}

fn find_overlay_window(cx: &App) -> Option<WindowHandle<DesktopLyricsView>> {
    cx.windows()
        .into_iter()
        .find_map(|window| window.downcast::<DesktopLyricsView>())
}

fn hash_text(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[inline]
fn mix_hash(seed: u64, value: u64) -> u64 {
    (seed ^ value).wrapping_mul(0x0000_0100_0000_01b3)
}
