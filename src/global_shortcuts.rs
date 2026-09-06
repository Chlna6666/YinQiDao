use gpui::{App, Context, Timer, Window, WindowHandle};

use crate::{hotkeys::AppHotkeyAction, ui::MusicApp};

const GLOBAL_SHORTCUT_TICK: std::time::Duration = std::time::Duration::from_millis(40);

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

pub(crate) fn start_ui_service(main_window: WindowHandle<MusicApp>, cx: &mut App) {
    cx.spawn(async move |cx| -> anyhow::Result<()> {
        loop {
            Timer::after(GLOBAL_SHORTCUT_TICK).await;
            let actions = crate::hotkeys::drain_app_actions();
            if actions.is_empty() {
                continue;
            }

            let still_open = cx.update(|cx| {
                main_window
                    .update(cx, |app, window, app_cx| {
                        for action in actions {
                            app.apply_app_hotkey(action, window, app_cx);
                        }
                    })
                    .is_ok()
            })?;
            if !still_open {
                break;
            }
        }
        Ok(())
    })
    .detach();
}
