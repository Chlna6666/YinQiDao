pub mod components;
mod enrichment;
mod home;
#[rustfmt::skip]
mod library;
pub(crate) mod lyrics_overlay;
mod mini_player_lyrics;
#[allow(dead_code)]
#[path = "player.rs"]
mod player_legacy;
mod player_facade;
#[rustfmt::skip]
mod player_stage;
use player_facade as player;
pub mod route;
mod settings;
mod settings_content;
mod shell;
pub mod theme;

pub use shell::MusicApp;
