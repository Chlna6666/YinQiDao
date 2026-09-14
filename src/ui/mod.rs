#[allow(dead_code)]
mod app_runtime_events;
pub(crate) mod audio_debug_analysis;
pub(crate) mod audio_debug_window;
pub(crate) mod audio_spatial_debug_3d;
pub mod components;
mod enrichment;
mod home;
#[rustfmt::skip]
mod library;
pub(crate) mod lyrics_overlay;
mod mini_player_lyrics;
mod mini_player_view;
mod player_facade;
#[allow(dead_code)]
mod player_legacy;
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
mod plugin_settings;
#[path = "plugin/theme.rs"]
mod plugin_theme;
mod stage_chrome;
mod stage_controls;
mod stage_lyrics;
use player_facade as player;
pub mod route;
#[path = "settings_with_plugins.rs"]
mod settings;
mod shell;
pub mod theme;

pub use shell::MusicApp;
