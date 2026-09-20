use std::time::Duration;

use gpui::{
    App, AppContext as _, BorrowAppContext as _, Context, Entity, EventEmitter, Global,
};

use crate::model::PlaybackState;

use super::shell::MusicApp;

/// Cross-view scrub previews are bounded independently from raw pointer frequency.
/// The active slider still paints every local input sample; semantic consumers do not need
/// 240+ notifications per second.
pub(crate) const PROGRESS_PREVIEW_INTERVAL: Duration = Duration::from_micros(8_333);

#[derive(Clone, Copy, Debug)]
pub(crate) enum AppUiEvent {
    PlaybackStateChanged(PlaybackState),
    ProgressChanged {
        position_ms: u64,
        ratio: Option<f32>,
    },
}

pub(crate) struct AppUiEventBridge;

impl EventEmitter<AppUiEvent> for AppUiEventBridge {}

#[derive(Default)]
struct AppUiEventBridgeCache {
    bridge: Option<Entity<AppUiEventBridge>>,
}

impl Global for AppUiEventBridgeCache {}

pub(crate) fn bridge(cx: &mut Context<MusicApp>) -> Entity<AppUiEventBridge> {
    cx.update_default_global(|cache: &mut AppUiEventBridgeCache, cx| {
        if let Some(bridge) = &cache.bridge {
            return bridge.clone();
        }
        let bridge = cx.new(|_| AppUiEventBridge);
        cache.bridge = Some(bridge.clone());
        bridge
    })
}

pub(crate) fn emit_music(cx: &mut Context<MusicApp>, event: AppUiEvent) {
    let bridge = bridge(cx);
    bridge.update(cx, |_, cx| cx.emit(event));
}

pub(crate) fn emit_from_app(
    bridge: &Entity<AppUiEventBridge>,
    event: AppUiEvent,
    cx: &mut App,
) {
    let _ = bridge.update(cx, |_, cx| cx.emit(event));
}
