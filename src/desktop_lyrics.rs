use std::{sync::Arc, time::Duration};

use gpui::{
    App, AppContext, BorrowAppContext, Bounds, Context, Global, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowKind, WindowOptions, point, px, size,
};

use crate::{
    hotkeys::LyricsHotkeyAction,
    lyrics::LyricWord,
    model::{PlaybackState, TrackId},
    settings::DesktopLyricsAlignment,
    ui::{MusicApp, lyrics_overlay::DesktopLyricsView},
};

const MIN_OVERLAY_WIDTH: f32 = 420.0;
const MIN_OVERLAY_HEIGHT: f32 = 92.0;
const DEFAULT_DESKTOP_LYRICS_BACKGROUND_OPACITY: f32 = 0.22;
const DESKTOP_LYRICS_MIN_WAKE_MS: u64 = 8;

#[derive(Default)]
struct DesktopLyricsWindowState {
    window: Option<WindowHandle<DesktopLyricsView>>,
    /// A hidden HWND is still owned by GPUI until the platform close callback fires.
    closing: bool,
    /// A fast off->on toggle while closing is coalesced into one reopen after destruction.
    reopen_after_close: bool,
}

impl Global for DesktopLyricsWindowState {}

#[derive(Clone, Debug)]
pub(crate) struct LyricsDisplay {
    pub track_id: TrackId,
    pub line_index: usize,
    pub position_ms: u64,
    pub current: String,
    pub current_words: Arc<[LyricWord]>,
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
        ensure_window_state(cx);

        if !self.config.desktop_lyrics.visible {
            request_overlay_close(cx, false);
            return;
        }

        // Never create a second HWND while the previous GPUI window is still in its deferred
        // destruction phase. A rapid second click merely records one reopen request.
        let closing = cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
            if state.closing {
                state.reopen_after_close = true;
                true
            } else {
                false
            }
        });
        if closing {
            return;
        }

        let existing =
            cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| state.window.clone());
        if let Some(window) = existing {
            // Keep the tracked instance and collapse any historical duplicate surfaces left by an
            // older build. Hiding before removal prevents a stale DComp frame from remaining visible.
            for extra in desktop_lyrics_windows(cx) {
                if extra == window {
                    continue;
                }
                let _ = extra.update(cx, |_view, window, _cx| {
                    window.hide_window();
                    window.remove_window();
                });
            }

            let always_on_top = self.config.desktop_lyrics.always_on_top;
            if window
                .update(cx, |_view, window, _cx| {
                    // Style first, show second. On Windows this prevents the shell from ever seeing
                    // the lyric surface as a normal app/taskbar window.
                    let applied = crate::window_platform::configure_desktop_lyrics_window(
                        window,
                        always_on_top,
                    );
                    window.show_window();
                    // A hidden NOACTIVATE popup does not automatically get an input-driven first
                    // frame. Force GPUI to build/present immediately instead of waiting for the
                    // first mouse move to wake the inactive widget.
                    window.refresh();
                    window.request_animation_frame();
                    applied
                })
                .is_ok()
            {
                return;
            }
            cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
                state.window = None;
            });
        }

        // Recover from old builds that could leave more than one DesktopLyricsView alive. Adopt one
        // as the canonical widget and synchronously hide/remove every extra surface.
        let mut overlays = desktop_lyrics_windows(cx).into_iter();
        if let Some(primary) = overlays.next() {
            for extra in overlays {
                let _ = extra.update(cx, |_view, window, _cx| {
                    window.hide_window();
                    window.remove_window();
                });
            }
            cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
                state.window = Some(primary.clone());
                state.closing = false;
                state.reopen_after_close = false;
            });

            let always_on_top = self.config.desktop_lyrics.always_on_top;
            if primary
                .update(cx, |_view, window, _cx| {
                    let applied = crate::window_platform::configure_desktop_lyrics_window(
                        window,
                        always_on_top,
                    );
                    window.show_window();
                    // A hidden NOACTIVATE popup does not automatically get an input-driven first
                    // frame. Force GPUI to build/present immediately instead of waiting for the
                    // first mouse move to wake the inactive widget.
                    window.refresh();
                    window.request_animation_frame();
                    applied
                })
                .is_ok()
            {
                return;
            }
            cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
                state.window = None;
            });
        }

        let config = self.config.desktop_lyrics.clone();
        let width = config.width.clamp(MIN_OVERLAY_WIDTH, 1_600.0);
        let height = config.height.clamp(MIN_OVERLAY_HEIGHT, 520.0);
        let window_bounds = match (config.x, config.y) {
            (Some(x), Some(y)) if x.is_finite() && y.is_finite() => {
                WindowBounds::Windowed(Bounds {
                    origin: point(px(x), px(y)),
                    size: size(px(width), px(height)),
                })
            }
            _ => WindowBounds::Windowed(Bounds::centered(None, size(px(width), px(height)), cx)),
        };
        let parent = cx.entity().downgrade();
        let options = WindowOptions {
            titlebar: None,
            window_bounds: Some(window_bounds),
            window_min_size: Some(size(px(MIN_OVERLAY_WIDTH), px(MIN_OVERLAY_HEIGHT))),
            // Desktop lyrics are a widget/panel on every platform, not a document window.
            kind: WindowKind::PopUp,
            focus: false,
            // Keep the native surface hidden until TOOLWINDOW/NOACTIVATE/POPUP styles have been
            // applied. Showing first lets Explorer briefly register a taskbar/Alt-Tab entry.
            show: false,
            is_movable: true,
            // Windows resizing frames opt the HWND back into Snap Layouts. Keep widget geometry
            // application-owned there; other platforms retain their existing resize behavior.
            is_resizable: !cfg!(windows),
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };

        match cx.open_window(options, move |_, cx| {
            cx.new(|cx| DesktopLyricsView::new(parent, cx))
        }) {
            Ok(window) => {
                cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
                    state.window = Some(window.clone());
                    state.closing = false;
                    state.reopen_after_close = false;
                });
                let always_on_top = config.always_on_top;
                let applied = window
                    .update(cx, |_view, window, _cx| {
                        let applied = crate::window_platform::configure_desktop_lyrics_window(
                            window,
                            always_on_top,
                        );
                        window.show_window();
                        // First presentation must not depend on pointer input. This widget is
                        // NOACTIVATE, so explicitly dirty and request one presentation frame.
                        window.refresh();
                        window.request_animation_frame();
                        applied
                    })
                    .unwrap_or(false);
                #[cfg(windows)]
                if !applied {
                    tracing::warn!("桌面歌词 HWND 小组件样式应用失败");
                }
            }
            Err(error) => {
                cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
                    state.window = None;
                    state.closing = false;
                    state.reopen_after_close = false;
                });
                self.status = format!("打开桌面歌词失败：{error:#}");
                self.config.desktop_lyrics.visible = false;
                self.save_config();
            }
        }
    }

    fn recreate_desktop_lyrics_window(&mut self, cx: &mut Context<Self>) {
        ensure_window_state(cx);
        if !self.config.desktop_lyrics.visible {
            request_overlay_close(cx, false);
            return;
        }

        // Recreate only after GPUI confirms that the old native window is gone.
        if !request_overlay_close(cx, true) {
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
        ensure_window_state(cx);
        // Hide first so the surface disappears immediately; GPUI owns the actual HWND destruction.
        request_overlay_close(cx, false);
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
        {
            ensure_window_state(cx);
            let overlay =
                cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| state.window.clone());
            if let Some(overlay) = overlay {
                let applied = overlay
                    .update(cx, |_view, window, _cx| {
                        crate::window_platform::set_always_on_top(window, always_on_top)
                    })
                    .unwrap_or(false);
                if !applied {
                    self.status = "桌面歌词置顶状态应用失败".into();
                }
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

    pub(crate) fn toggle_desktop_lyrics_background(&mut self, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.background_opacity =
            if self.config.desktop_lyrics.background_opacity > 0.01 {
                0.0
            } else {
                DEFAULT_DESKTOP_LYRICS_BACKGROUND_OPACITY
            };
        self.save_config();
        cx.notify();
    }

    pub(crate) fn adjust_desktop_lyrics_font(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.config.desktop_lyrics.font_size =
            (self.config.desktop_lyrics.font_size + delta).clamp(18.0, 64.0);
        self.save_config();
        cx.notify();
    }

    #[allow(dead_code)]
    pub(crate) fn adjust_desktop_lyrics_background(&mut self, delta: f32, cx: &mut Context<Self>) {
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
        let width = 760.0;
        let height = 148.0;
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
        self.config.desktop_lyrics.x = Some(f32::from(bounds.origin.x));
        self.config.desktop_lyrics.y = Some(f32::from(bounds.origin.y));
        self.config.desktop_lyrics.width = width;
        self.config.desktop_lyrics.height = height;
        self.save_config();

        ensure_window_state(cx);
        let overlay =
            cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| state.window.clone());
        let moved = overlay
            .as_ref()
            .is_some_and(|overlay| {
                overlay
                    .update(cx, |_view, window, _cx| {
                        crate::window_platform::set_desktop_lyrics_bounds(window, bounds)
                    })
                    .unwrap_or(false)
            });

        // Windows keeps the same HWND, avoiding a one-frame duplicate DirectComposition surface.
        // Platforms without direct geometry support fall back to the previous recreate path.
        if self.config.desktop_lyrics.visible && !moved {
            self.recreate_desktop_lyrics_window(cx);
        }
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
            let position_ms = self
                .engine
                .as_ref()
                .map_or(self.snapshot.position_ms, |engine| engine.progress().1);
            return Some(LyricsDisplay {
                track_id: track.id,
                line_index: 0,
                position_ms,
                current,
                current_words: Arc::from(Vec::<LyricWord>::new()),
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
            track_id: track.id,
            line_index: index,
            position_ms,
            current: current.text.clone(),
            current_words: current.words.clone(),
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

    pub(crate) fn desktop_lyrics_next_boundary_delay(&self) -> Option<Duration> {
        if !self.config.desktop_lyrics.visible || self.snapshot.state != PlaybackState::Playing {
            return None;
        }
        let track = self.snapshot.current_track.as_ref()?;
        let lines = self.lyrics.get(&track.id)?.timed_lines();
        if lines.is_empty() {
            return None;
        }
        let position_ms = self
            .engine
            .as_ref()
            .map_or(self.snapshot.position_ms, |engine| engine.progress().1);
        let index = lines
            .iter()
            .rposition(|line| line.timestamp_ms <= position_ms)
            .unwrap_or(0);
        let line = &lines[index];

        // Wake only at semantic lyric boundaries. Word sweep itself is compositor-driven; the CPU
        // wakes once for the next authored word/syllable or the next line, whichever comes first.
        let mut next_timestamp = lines.get(index + 1).map(|line| line.timestamp_ms);
        let next_word_index = line
            .words
            .partition_point(|word| word.timestamp_ms <= position_ms);
        if let Some(word) = line.words.get(next_word_index) {
            next_timestamp = Some(
                next_timestamp.map_or(word.timestamp_ms, |current| current.min(word.timestamp_ms)),
            );
        }

        let timestamp = next_timestamp?;
        Some(Duration::from_millis(
            timestamp
                .saturating_sub(position_ms)
                .max(DESKTOP_LYRICS_MIN_WAKE_MS),
        ))
    }
}

pub(crate) fn initialize(main_window: WindowHandle<MusicApp>, cx: &mut App) {
    ensure_window_state(cx);

    // Window removal is asynchronous with respect to the render/event turn. Keep the closing state
    // until GPUI tells us the final DesktopLyricsView is actually inaccessible, then perform at most
    // one coalesced reopen.
    let reopen_window = main_window.clone();
    cx.on_window_closed(move |cx| {
        if !desktop_lyrics_windows(cx).is_empty() {
            return;
        }

        let reopen = cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
            state.window = None;
            state.closing = false;
            let reopen = state.reopen_after_close;
            state.reopen_after_close = false;
            reopen
        });
        if reopen {
            let _ = reopen_window.update(cx, |app, _window, app_cx| {
                if app.config.desktop_lyrics.visible {
                    app.sync_desktop_lyrics_window(app_cx);
                }
            });
        }
    })
    .detach();

    let _ = main_window.update(cx, |app, _window, app_cx| {
        app.sync_desktop_lyrics_window(app_cx);
    });
}

pub(crate) fn shutdown(cx: &mut App) {
    ensure_window_state(cx);
    cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
        state.reopen_after_close = false;
    });
    request_overlay_close(cx, false);
}

fn ensure_window_state(cx: &mut App) {
    if !cx.has_global::<DesktopLyricsWindowState>() {
        cx.set_global(DesktopLyricsWindowState::default());
    }
}

fn desktop_lyrics_windows(cx: &App) -> Vec<WindowHandle<DesktopLyricsView>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<DesktopLyricsView>())
        .collect()
}

fn request_overlay_close(cx: &mut App, reopen_after_close: bool) -> bool {
    let overlays = desktop_lyrics_windows(cx);
    if overlays.is_empty() {
        cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
            state.window = None;
            state.closing = false;
            state.reopen_after_close = false;
        });
        return false;
    }

    cx.update_global(|state: &mut DesktopLyricsWindowState, _cx| {
        state.closing = true;
        state.reopen_after_close = reopen_after_close;
    });

    for overlay in overlays {
        let _ = overlay.update(cx, |_view, window, _cx| {
            // Hiding is immediate at the platform level and removes stale taskbar thumbnails /
            // DirectComposition pixels before the deferred GPUI destruction completes.
            window.hide_window();
            window.remove_window();
        });
    }
    true
}
