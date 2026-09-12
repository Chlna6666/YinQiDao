use gpui::{App, Context, Entity, Global, Subscription, Window, WindowHandle};

use crate::{
    hotkeys::{AppHotkeyAction, HotkeyEventBridge, LyricsHotkeyAction},
    ui::MusicApp,
};

struct HotkeyUiBindings {
    _bridge: Entity<HotkeyEventBridge>,
    _app_subscription: Subscription,
    _lyrics_subscription: Subscription,
}

impl Global for HotkeyUiBindings {}

impl MusicApp {
    pub(crate) fn toggle_global_shortcuts(&mut self, cx: &mut Context<Self>) {
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

    pub(crate) fn apply_app_hotkey(
        &mut self,
        action: AppHotkeyAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.config.lyrics_shortcuts.enabled {
            return;
        }

        match action {
            AppHotkeyAction::TogglePlayPause => self.toggle_play(cx),
            AppHotkeyAction::PreviousTrack => self.previous(cx),
            AppHotkeyAction::NextTrack => self.next(cx),
            AppHotkeyAction::SeekBackward => self.seek_relative(-10_000, cx),
            AppHotkeyAction::SeekForward => self.seek_relative(10_000, cx),
            AppHotkeyAction::VolumeDown => self.adjust_volume(-0.05, cx),
            AppHotkeyAction::VolumeUp => self.adjust_volume(0.05, cx),
            AppHotkeyAction::ToggleMute => self.toggle_mute(cx),
            AppHotkeyAction::ToggleShuffle => self.toggle_shuffle(cx),
            AppHotkeyAction::CycleRepeat => self.cycle_repeat(cx),
            AppHotkeyAction::ShowMainWindow => {
                window.show_window();
                if window.is_minimized() {
                    window.restore_window();
                }
                window.activate_window();
            }
            AppHotkeyAction::ToggleStage => {
                window.show_window();
                if window.is_minimized() {
                    window.restore_window();
                }
                window.activate_window();
                self.toggle_stage(cx);
            }
        }
    }
}

pub(crate) fn install_event_bridge(main_window: WindowHandle<MusicApp>, cx: &mut App) {
    let Some(bridge) = crate::hotkeys::event_bridge(cx) else {
        tracing::warn!("全局快捷键 GPUI 事件桥已被占用，跳过重复安装");
        return;
    };

    let app_window = main_window.clone();
    let app_subscription = cx.subscribe(
        &bridge,
        move |_bridge, action: &AppHotkeyAction, cx| {
            let _ = app_window.update(cx, |app, window, app_cx| {
                app.apply_app_hotkey(*action, window, app_cx);
            });
        },
    );

    let lyrics_window = main_window;
    let lyrics_subscription = cx.subscribe(
        &bridge,
        move |_bridge, action: &LyricsHotkeyAction, cx| {
            let _ = lyrics_window.update(cx, |app, _window, app_cx| {
                app.apply_lyrics_hotkey(*action, app_cx);
            });
        },
    );

    cx.set_global(HotkeyUiBindings {
        _bridge: bridge,
        _app_subscription: app_subscription,
        _lyrics_subscription: lyrics_subscription,
    });
}
