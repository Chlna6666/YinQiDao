pub mod components;
mod enrichment;
mod home;
mod library;
#[path = "player.rs"]
mod player_legacy;
mod player_stage;
use player_stage as player;
pub mod route;
mod settings;
mod shell;
pub mod theme;

pub use shell::MusicApp;
