pub mod components;
mod enrichment;
mod home;
#[rustfmt::skip]
mod library;
#[allow(dead_code)]
#[path = "player.rs"]
mod player_legacy;
#[rustfmt::skip]
mod player_stage;
use player_stage as player;
pub mod route;
mod settings;
mod shell;
pub mod theme;

pub use shell::MusicApp;
