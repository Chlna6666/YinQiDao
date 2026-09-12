use std::{sync::mpsc::Receiver, thread};

use gpui::{Context, EventEmitter};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::{
    audio::AudioUiEvent,
    media_controls::SystemMediaEvent,
};

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
    pub(crate) fn new(
        library_events: Receiver<()>,
        media_events: Receiver<SystemMediaEvent>,
        audio_events: Option<UnboundedReceiver<AudioUiEvent>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (library_tx, mut library_rx) = unbounded_channel();
        let (media_tx, mut media_rx) = unbounded_channel();

        let _ = thread::Builder::new()
            .name("yinqidao-library-ui-events".into())
            .spawn(move || {
                while library_events.recv().is_ok() {
                    if library_tx.send(()).is_err() {
                        break;
                    }
                }
            });
        let _ = thread::Builder::new()
            .name("yinqidao-system-media-ui-events".into())
            .spawn(move || {
                while let Ok(event) = media_events.recv() {
                    if media_tx.send(event).is_err() {
                        break;
                    }
                }
            });

        cx.spawn(async move |this, cx| {
            while library_rx.recv().await.is_some() {
                // Collapse any watcher bursts that arrived before GPUI handled this turn. The
                // watcher already debounces filesystem activity; this prevents multiple database
                // refreshes when several roots finish together without introducing polling.
                while library_rx.try_recv().is_ok() {}
                if this
                    .update(cx, |_, cx| cx.emit(AppRuntimeEvent::LibraryChanged))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        cx.spawn(async move |this, cx| {
            while let Some(event) = media_rx.recv().await {
                if this
                    .update(cx, |_, cx| cx.emit(AppRuntimeEvent::SystemMedia(event)))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut bridge = Self {
            audio_generation: 0,
        };
        if let Some(audio_events) = audio_events {
            bridge.attach_audio_receiver(audio_events, cx);
        }
        bridge
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
