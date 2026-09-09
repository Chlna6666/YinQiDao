pub(crate) mod audio_debug_window;
pub(crate) mod audio_spatial_debug_3d;
pub mod components;
mod enrichment;
mod home;
#[rustfmt::skip]
mod library;
pub(crate) mod lyrics_overlay;
mod mini_player_lyrics;
mod player_facade;
#[allow(dead_code)]
mod player_legacy;
#[rustfmt::skip]
mod player_stage;
use player_facade as player;
pub mod route;
mod settings;
mod shell;
pub mod theme;

pub use shell::MusicApp;
