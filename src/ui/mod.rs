#[allow(dead_code)]
mod app_runtime_events;
pub(crate) mod app_ui_events;
pub(crate) mod audio_debug_analysis;
pub(crate) mod audio_debug_window;
pub(crate) mod audio_spatial_debug_3d;
pub mod components;
mod enrichment;
mod favorite_sync;
mod home;
pub(crate) mod image_cache;
#[rustfmt::skip]
mod library;
pub(crate) mod lyrics_overlay;
mod mini_player_lyrics;
mod mini_player_view;
mod online_audio_cache;
mod online_playlist;
mod player_facade;
#[allow(dead_code)]
mod player_legacy;
mod queue_cover;
mod queue_drawer;
mod quick_audio_window;
#[rustfmt::skip]
mod player_stage;
#[path = "plugin/command_palette.rs"]
mod plugin_command_palette;
#[path = "plugin/extensions.rs"]
mod plugin_extensions;
#[path = "plugin/input.rs"]
mod plugin_input;
#[path = "plugin/navigation.rs"]
mod plugin_navigation;
#[path = "plugin/page_renderer.rs"]
mod plugin_page_renderer;
#[path = "plugin/settings.rs"]
pub(crate) mod plugin_settings;
#[path = "plugin/theme.rs"]
mod plugin_theme;
mod stage_chrome;
mod stage_controls;
mod stage_lyrics;
use player_facade as player;
pub mod route;
#[path = "settings_with_plugins.rs"]
pub(crate) mod settings;
mod shell;
pub mod theme;

pub use shell::MusicApp;

/// Visible inactive windows can keep animating. Their actual cadence comes
/// from the window's inherited or explicit GPUI inactive_frame_clock; no
/// process-wide override and no idle polling loop is installed here.
pub(crate) fn enable_inactive_window_animations(window: &mut gpui::Window) {
    window.set_inactive_animation_engine_enabled(true);
    window.set_inactive_dirty_redraw_enabled(true);
    let interval = window
        .inactive_frame_clock()
        .and_then(|clock| clock.fps())
        .map(|fps| std::time::Duration::from_secs_f64(1.0 / f64::from(fps.get())));
    window.set_inactive_dirty_frame_retry_interval(interval);
}
