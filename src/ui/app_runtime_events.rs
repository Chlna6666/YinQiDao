use std::{
    sync::{Arc, mpsc::Receiver},
    thread,
    time::Duration,
};

use gpui::{Context, Entity, EventEmitter, Global, Subscription, Timer};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::{
    audio::{AudioUiEvent, PlayerCommand},
    media_controls::SystemMediaEvent,
    model::PlaybackState,
};

use super::shell::MusicApp;

const RUNTIME_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) enum AppRuntimeEvent {
    Audio(AudioUiEvent),
    LibraryChanged,
    SystemMedia(SystemMediaEvent),
}

pub(crate) struct AppRuntimeEventBridge {
    audio_generation: u64,
    maintenance_started: bool,
}

impl EventEmitter<AppRuntimeEvent> for AppRuntimeEventBridge {}

impl AppRuntimeEventBridge {
    fn new() -> Self {
        Self {
            audio_generation: 0,
            maintenance_started: false,
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
            let mut pending_event = None;
            loop {
                let event = if let Some(event) = pending_event.take() {
                    event
                } else {
                    let Some(event) = events.recv().await else {
                        break;
                    };
                    event
                };

                let event = match event {
                    AudioUiEvent::SnapshotChanged => {
                        loop {
                            match events.try_recv() {
                                Ok(AudioUiEvent::SnapshotChanged) => {}
                                Ok(next) => {
                                    pending_event = Some(next);
                                    break;
                                }
                                Err(_) => break,
                            }
                        }
                        AudioUiEvent::SnapshotChanged
                    }
                    event => event,
                };

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

    fn start_maintenance(
        &mut self,
        parent: gpui::WeakEntity<MusicApp>,
        cx: &mut Context<Self>,
    ) {
        if self.maintenance_started {
            return;
        }
        self.maintenance_started = true;
        cx.spawn(async move |_this, cx| {
            loop {
                Timer::after(RUNTIME_MAINTENANCE_INTERVAL).await;
                if parent
                    .update(cx, |app, cx| app.runtime_maintenance_tick(cx))
                    .is_err()
                {
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
    root_sources_attached: bool,
}

impl Global for AppRuntimeBinding {}

fn ensure_binding(cx: &mut Context<MusicApp>) -> Entity<AppRuntimeEventBridge> {
    if let Some(binding) = cx.try_global::<AppRuntimeBinding>() {
        return binding.bridge.clone();
    }

    let bridge = cx.new(|_| AppRuntimeEventBridge::new());
    let subscription = cx.subscribe(&bridge, |app, _bridge, event, cx| {
        apply_runtime_event(app, event, cx);
    });
    cx.set_global(AppRuntimeBinding {
        bridge: bridge.clone(),
        _subscription: subscription,
        audio_engine_ptr: 0,
        root_sources_attached: false,
    });
    bridge
}

pub(crate) fn attach_root_sources(
    app: &MusicApp,
    library_events: Receiver<()>,
    media_events: Receiver<SystemMediaEvent>,
    cx: &mut Context<MusicApp>,
) {
    let bridge = ensure_binding(cx);
    let already_attached = cx
        .try_global::<AppRuntimeBinding>()
        .is_some_and(|binding| binding.root_sources_attached);
    if !already_attached {
        let parent = cx.entity().downgrade();
        bridge.update(cx, |bridge, cx| {
            bridge.attach_library_receiver(library_events, cx);
            bridge.attach_media_receiver(media_events, cx);
            bridge.start_maintenance(parent, cx);
        });
        cx.update_global(|binding: &mut AppRuntimeBinding, _cx| {
            binding.root_sources_attached = true;
        });
    }
    ensure_audio_runtime(app, cx);
}

pub(crate) fn ensure_audio_runtime(app: &MusicApp, cx: &mut Context<MusicApp>) {
    let bridge = ensure_binding(cx);
    let engine_ptr = app
        .engine
        .as_ref()
        .map_or(0, |engine| Arc::as_ptr(engine) as usize);
    let current_ptr = cx
        .try_global::<AppRuntimeBinding>()
        .map_or(0, |binding| binding.audio_engine_ptr);
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
        AppRuntimeEvent::Audio(AudioUiEvent::SnapshotChanged) => {
            app.sync_audio_snapshot_event(cx);
        }
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
        SystemMediaEvent::SetVolume(volume) => app.set_app_volume(*volume, cx),
    }
}
