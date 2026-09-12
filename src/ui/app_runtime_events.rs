use std::{
    sync::{Arc, mpsc::Receiver},
    thread,
};

use gpui::{Context, Entity, EventEmitter, Global, Subscription};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::{
    audio::{AudioEngine, AudioUiEvent, PlayerCommand},
    media_controls::SystemMediaEvent,
    model::PlaybackState,
};

use super::shell::MusicApp;

pub(crate) enum AppRuntimeEvent {
    Audio(AudioUiEvent),
    LibraryChanged,
    SystemMedia(SystemMediaEvent),
}

pub(crate) struct AppRuntimeEventBridge {
    audio_generation: u64,
}

impl EventEmitter<AppRuntimeEvent> for AppRuntimeEventBridge {}

impl AppRuntimeEventBridge {
    fn new() -> Self {
        Self {
            audio_generation: 0,
        }
    }

    pub(crate) fn attach_library_receiver(
        &mut self,
        library_events: Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        let (tx, mut rx) = unbounded_channel();
        let _ = thread::Builder::new()
            .name("yinqidao-library-ui-events".into())
            .spawn(move || {
                while library_events.recv().is_ok() {
                    if tx.send(()).is_err() {
                        break;
                    }
                }
            });

        cx.spawn(async move |this, cx| {
            while rx.recv().await.is_some() {
                while rx.try_recv().is_ok() {}
                if this
                    .update(cx, |_, cx| cx.emit(AppRuntimeEvent::LibraryChanged))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn attach_media_receiver(
        &mut self,
        media_events: Receiver<SystemMediaEvent>,
        cx: &mut Context<Self>,
    ) {
        let (tx, mut rx) = unbounded_channel();
        let _ = thread::Builder::new()
            .name("yinqidao-system-media-ui-events".into())
            .spawn(move || {
                while let Ok(event) = media_events.recv() {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            });

        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                if this
                    .update(cx, |_, cx| cx.emit(AppRuntimeEvent::SystemMedia(event)))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    pub(crate) fn attach_audio_receiver(
        &mut self,
        mut events: UnboundedReceiver<AudioUiEvent>,
        cx: &mut Context<Self>,
    ) {
        self.audio_generation = self.audio_generation.wrapping_add(1);
        let generation = self.audio_generation;
        cx.spawn(async move |this, cx| {
            while let Some(event) = events.recv().await {
                let alive = this.update(cx, |bridge, cx| {
                    if bridge.audio_generation == generation {
                        cx.emit(AppRuntimeEvent::Audio(event));
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }
}

struct AppRuntimeBinding {
    bridge: Entity<AppRuntimeEventBridge>,
    _subscription: Subscription,
    audio_engine_ptr: usize,
}

impl Global for AppRuntimeBinding {}

pub(crate) fn ensure_audio_runtime(app: &MusicApp, cx: &mut Context<MusicApp>) {
    let engine_ptr = app
        .engine
        .as_ref()
        .map_or(0, |engine| Arc::as_ptr(engine) as usize);

    if !cx.has_global::<AppRuntimeBinding>() {
        let bridge = cx.new(|_| AppRuntimeEventBridge::new());
        let subscription = cx.subscribe(&bridge, |app, _bridge, event, cx| {
            apply_runtime_event(app, event, cx);
        });
        cx.set_global(AppRuntimeBinding {
            bridge: bridge.clone(),
            _subscription: subscription,
            audio_engine_ptr: 0,
        });
    }

    let (bridge, current_ptr) = cx
        .try_global::<AppRuntimeBinding>()
        .map(|binding| (binding.bridge.clone(), binding.audio_engine_ptr))
        .expect("runtime event binding must exist after initialization");
    if current_ptr == engine_ptr {
        return;
    }

    if let Some(events) = app
        .engine
        .as_ref()
        .and_then(|engine| engine.take_ui_event_receiver())
    {
        bridge.update(cx, |bridge, cx| {
            bridge.attach_audio_receiver(events, cx);
        });
    }
    cx.update_global(|binding: &mut AppRuntimeBinding, _cx| {
        binding.audio_engine_ptr = engine_ptr;
    });
}

fn apply_runtime_event(
    app: &mut MusicApp,
    event: &AppRuntimeEvent,
    cx: &mut Context<MusicApp>,
) {
    match event {
        AppRuntimeEvent::Audio(AudioUiEvent::SnapshotChanged) => sync_audio_snapshot(app, cx),
        AppRuntimeEvent::Audio(AudioUiEvent::Error(error)) => {
            app.status.clone_from(error);
            cx.notify();
        }
        AppRuntimeEvent::LibraryChanged => {
            app.artwork_missing.clear();
            app.refresh_tracks_async(cx, Some("歌库已更新".into()));
        }
        AppRuntimeEvent::SystemMedia(event) => apply_system_media_event(app, event, cx),
    }
}

fn sync_audio_snapshot(app: &mut MusicApp, cx: &mut Context<MusicApp>) {
    let Some(engine) = app.engine.clone() else {
        return;
    };
    let old_track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
    let snapshot = engine.snapshot();

    if app.drag_target.is_none() {
        app.position_ms = snapshot.position_ms;
        app.config.position_ms = snapshot.position_ms;
    }
    if let Some(ratio) = app.pending_volume_ratio
        && (app.config.volume - ratio).abs() < 0.02
    {
        app.pending_volume_ratio = None;
    }
    if snapshot.current_track.is_some() {
        app.config.current_track = snapshot.current_track.as_ref().map(|track| track.id);
    }

    app.snapshot = snapshot;
    let current_track_id = app.snapshot.current_track.as_ref().map(|track| track.id);
    if current_track_id != old_track_id {
        app.request_current_enrichment(cx);
    }

    // Structural events are intentionally sparse. Hot position samples never enter this path.
    cx.notify();
}

fn apply_system_media_event(
    app: &mut MusicApp,
    event: &SystemMediaEvent,
    cx: &mut Context<MusicApp>,
) {
    match event {
        SystemMediaEvent::Play => {
            if app.snapshot.state != PlaybackState::Playing {
                app.toggle_play(cx);
            }
        }
        SystemMediaEvent::Pause => {
            if app.snapshot.state == PlaybackState::Playing {
                app.toggle_play(cx);
            }
        }
        SystemMediaEvent::Toggle => app.toggle_play(cx),
        SystemMediaEvent::Next => app.next(cx),
        SystemMediaEvent::Previous => app.previous(cx),
        SystemMediaEvent::Stop => {
            app.send(PlayerCommand::Stop);
            cx.notify();
        }
        SystemMediaEvent::SeekBy(delta_ms) => app.seek_relative(*delta_ms, cx),
        SystemMediaEvent::SetPosition(position) => {
            app.seek_to_ms(position.as_millis() as u64, cx);
        }
    }
}
