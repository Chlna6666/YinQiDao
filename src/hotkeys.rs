use std::{
    sync::{
        Mutex, OnceLock,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LyricsHotkeyAction {
    ToggleVisible,
    ToggleLock,
    ToggleTranslation,
    IncreaseFont,
    DecreaseFont,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppHotkeyAction {
    TogglePlayPause,
    PreviousTrack,
    NextTrack,
    SeekBackward,
    SeekForward,
    VolumeDown,
    VolumeUp,
    ToggleMute,
    ToggleShuffle,
    CycleRepeat,
    ShowMainWindow,
    ToggleStage,
}

#[derive(Clone, Copy, Debug)]
enum ServiceCommand {
    SetEnabled(bool),
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
enum HotkeyEvent {
    Lyrics(LyricsHotkeyAction),
    App(AppHotkeyAction),
}

struct HotkeyService {
    command_tx: Sender<ServiceCommand>,
    lyrics_event_rx: Receiver<LyricsHotkeyAction>,
    app_event_rx: Receiver<AppHotkeyAction>,
    worker: Option<JoinHandle<()>>,
}

impl HotkeyService {
    fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (lyrics_event_tx, lyrics_event_rx) = mpsc::channel();
        let (app_event_tx, app_event_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("yinqidao-global-hotkeys".into())
            .spawn(move || platform::run(command_rx, lyrics_event_tx, app_event_tx))
            .ok();
        Self {
            command_tx,
            lyrics_event_rx,
            app_event_rx,
            worker,
        }
    }
}

static SERVICE: OnceLock<Mutex<HotkeyService>> = OnceLock::new();

fn service() -> &'static Mutex<HotkeyService> {
    SERVICE.get_or_init(|| Mutex::new(HotkeyService::new()))
}

pub(crate) fn set_enabled(enabled: bool) {
    if let Ok(service) = service().lock() {
        let _ = service.command_tx.send(ServiceCommand::SetEnabled(enabled));
    }
}

/// Drain lyric-window actions. Kept separate from application actions so the lightweight lyric
/// refresh loop never needs to understand transport or main-window state.
pub(crate) fn drain_actions() -> Vec<LyricsHotkeyAction> {
    let Ok(service) = service().lock() else {
        return Vec::new();
    };
    service.lyrics_event_rx.try_iter().collect()
}

/// Drain transport and main-window actions. These are dispatched against the main `MusicApp`
/// entity on the GPUI foreground thread; the Win32 message thread never touches player state.
pub(crate) fn drain_app_actions() -> Vec<AppHotkeyAction> {
    let Ok(service) = service().lock() else {
        return Vec::new();
    };
    service.app_event_rx.try_iter().collect()
}

pub(crate) fn shutdown() {
    let Ok(mut service) = service().lock() else {
        return;
    };
    let _ = service.command_tx.send(ServiceCommand::Shutdown);
    if let Some(worker) = service.worker.take() {
        drop(service);
        let _ = worker.join();
    }
}

#[cfg(windows)]
mod platform {
    #![allow(unsafe_code)]

    use std::{
        ffi::c_void,
        mem::MaybeUninit,
        ptr,
        sync::mpsc::{Receiver, RecvTimeoutError, Sender},
        time::Duration,
    };

    use super::{AppHotkeyAction, HotkeyEvent, LyricsHotkeyAction, ServiceCommand};

    const WM_HOTKEY: u32 = 0x0312;
    const PM_REMOVE: u32 = 0x0001;
    const PM_NOREMOVE: u32 = 0x0000;
    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_SHIFT: u32 = 0x0004;
    const MOD_NOREPEAT: u32 = 0x4000;

    const ID_PLAY_PAUSE: i32 = 0x5900;
    const ID_PREVIOUS: i32 = 0x5901;
    const ID_NEXT: i32 = 0x5902;
    const ID_SEEK_BACKWARD: i32 = 0x5903;
    const ID_SEEK_FORWARD: i32 = 0x5904;
    const ID_VOLUME_DOWN: i32 = 0x5905;
    const ID_VOLUME_UP: i32 = 0x5906;
    const ID_MUTE: i32 = 0x5907;
    const ID_SHUFFLE: i32 = 0x5908;
    const ID_REPEAT: i32 = 0x5909;
    const ID_SHOW_MAIN: i32 = 0x590a;
    const ID_TOGGLE_STAGE: i32 = 0x590b;

    const ID_TOGGLE_VISIBLE: i32 = 0x5940;
    const ID_TOGGLE_LOCK: i32 = 0x5941;
    const ID_TOGGLE_TRANSLATION: i32 = 0x5942;
    const ID_FONT_UP: i32 = 0x5943;
    const ID_FONT_DOWN: i32 = 0x5944;

    const VK_SPACE: u32 = 0x20;
    const VK_RETURN: u32 = 0x0d;
    const VK_LEFT: u32 = 0x25;
    const VK_UP: u32 = 0x26;
    const VK_RIGHT: u32 = 0x27;
    const VK_DOWN: u32 = 0x28;
    const VK_OEM_PLUS: u32 = 0xbb;
    const VK_OEM_MINUS: u32 = 0xbd;
    const VK_K: u32 = b'K' as u32;
    const VK_L: u32 = b'L' as u32;
    const VK_M: u32 = b'M' as u32;
    const VK_R: u32 = b'R' as u32;
    const VK_S: u32 = b'S' as u32;
    const VK_T: u32 = b'T' as u32;
    const VK_Y: u32 = b'Y' as u32;

    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Msg {
        hwnd: *mut c_void,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        pt: Point,
        l_private: u32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterHotKey(hwnd: *mut c_void, id: i32, modifiers: u32, virtual_key: u32) -> i32;
        fn UnregisterHotKey(hwnd: *mut c_void, id: i32) -> i32;
        fn PeekMessageW(
            message: *mut Msg,
            hwnd: *mut c_void,
            min_filter: u32,
            max_filter: u32,
            remove: u32,
        ) -> i32;
    }

    pub(super) fn run(
        command_rx: Receiver<ServiceCommand>,
        lyrics_event_tx: Sender<LyricsHotkeyAction>,
        app_event_tx: Sender<AppHotkeyAction>,
    ) {
        // RegisterHotKey(NULL, ...) posts WM_HOTKEY to this worker thread. Force creation of the
        // Win32 message queue before registration so the first shortcut cannot be lost at startup.
        let mut message = MaybeUninit::<Msg>::zeroed();
        unsafe {
            let _ = PeekMessageW(message.as_mut_ptr(), ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }

        let mut enabled = false;
        loop {
            match command_rx.recv_timeout(Duration::from_millis(12)) {
                Ok(ServiceCommand::SetEnabled(next)) => {
                    if next != enabled {
                        if next {
                            register_all();
                        } else {
                            unregister_all();
                        }
                        enabled = next;
                    }
                }
                Ok(ServiceCommand::Shutdown) => break,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            while let Ok(command) = command_rx.try_recv() {
                match command {
                    ServiceCommand::SetEnabled(next) => {
                        if next != enabled {
                            if next {
                                register_all();
                            } else {
                                unregister_all();
                            }
                            enabled = next;
                        }
                    }
                    ServiceCommand::Shutdown => {
                        if enabled {
                            unregister_all();
                        }
                        return;
                    }
                }
            }

            if !enabled {
                continue;
            }

            loop {
                let mut message = MaybeUninit::<Msg>::zeroed();
                let has_message = unsafe {
                    PeekMessageW(message.as_mut_ptr(), ptr::null_mut(), 0, 0, PM_REMOVE)
                } != 0;
                if !has_message {
                    break;
                }
                let message = unsafe { message.assume_init() };
                if message.message != WM_HOTKEY {
                    continue;
                }
                let event = event_for_id(message.w_param as i32);
                match event {
                    Some(HotkeyEvent::Lyrics(action)) => {
                        let _ = lyrics_event_tx.send(action);
                    }
                    Some(HotkeyEvent::App(action)) => {
                        let _ = app_event_tx.send(action);
                    }
                    None => {}
                }
            }
        }

        if enabled {
            unregister_all();
        }
    }

    fn event_for_id(id: i32) -> Option<HotkeyEvent> {
        match id {
            ID_PLAY_PAUSE => Some(HotkeyEvent::App(AppHotkeyAction::TogglePlayPause)),
            ID_PREVIOUS => Some(HotkeyEvent::App(AppHotkeyAction::PreviousTrack)),
            ID_NEXT => Some(HotkeyEvent::App(AppHotkeyAction::NextTrack)),
            ID_SEEK_BACKWARD => Some(HotkeyEvent::App(AppHotkeyAction::SeekBackward)),
            ID_SEEK_FORWARD => Some(HotkeyEvent::App(AppHotkeyAction::SeekForward)),
            ID_VOLUME_DOWN => Some(HotkeyEvent::App(AppHotkeyAction::VolumeDown)),
            ID_VOLUME_UP => Some(HotkeyEvent::App(AppHotkeyAction::VolumeUp)),
            ID_MUTE => Some(HotkeyEvent::App(AppHotkeyAction::ToggleMute)),
            ID_SHUFFLE => Some(HotkeyEvent::App(AppHotkeyAction::ToggleShuffle)),
            ID_REPEAT => Some(HotkeyEvent::App(AppHotkeyAction::CycleRepeat)),
            ID_SHOW_MAIN => Some(HotkeyEvent::App(AppHotkeyAction::ShowMainWindow)),
            ID_TOGGLE_STAGE => Some(HotkeyEvent::App(AppHotkeyAction::ToggleStage)),
            ID_TOGGLE_VISIBLE => Some(HotkeyEvent::Lyrics(LyricsHotkeyAction::ToggleVisible)),
            ID_TOGGLE_LOCK => Some(HotkeyEvent::Lyrics(LyricsHotkeyAction::ToggleLock)),
            ID_TOGGLE_TRANSLATION => {
                Some(HotkeyEvent::Lyrics(LyricsHotkeyAction::ToggleTranslation))
            }
            ID_FONT_UP => Some(HotkeyEvent::Lyrics(LyricsHotkeyAction::IncreaseFont)),
            ID_FONT_DOWN => Some(HotkeyEvent::Lyrics(LyricsHotkeyAction::DecreaseFont)),
            _ => None,
        }
    }

    fn register_all() {
        let base = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
        let shifted = base | MOD_SHIFT;
        for (id, modifiers, key, name) in [
            (ID_PLAY_PAUSE, base, VK_SPACE, "Ctrl+Alt+Space"),
            (ID_PREVIOUS, base, VK_LEFT, "Ctrl+Alt+Left"),
            (ID_NEXT, base, VK_RIGHT, "Ctrl+Alt+Right"),
            (ID_SEEK_BACKWARD, shifted, VK_LEFT, "Ctrl+Alt+Shift+Left"),
            (ID_SEEK_FORWARD, shifted, VK_RIGHT, "Ctrl+Alt+Shift+Right"),
            (ID_VOLUME_DOWN, base, VK_OEM_MINUS, "Ctrl+Alt+-"),
            (ID_VOLUME_UP, base, VK_OEM_PLUS, "Ctrl+Alt+="),
            (ID_MUTE, base, VK_M, "Ctrl+Alt+M"),
            (ID_SHUFFLE, base, VK_S, "Ctrl+Alt+S"),
            (ID_REPEAT, base, VK_R, "Ctrl+Alt+R"),
            (ID_SHOW_MAIN, base, VK_Y, "Ctrl+Alt+Y"),
            (ID_TOGGLE_STAGE, base, VK_RETURN, "Ctrl+Alt+Enter"),
            (ID_TOGGLE_VISIBLE, base, VK_L, "Ctrl+Alt+L"),
            (ID_TOGGLE_LOCK, base, VK_K, "Ctrl+Alt+K"),
            (ID_TOGGLE_TRANSLATION, base, VK_T, "Ctrl+Alt+T"),
            (ID_FONT_UP, base, VK_UP, "Ctrl+Alt+Up"),
            (ID_FONT_DOWN, base, VK_DOWN, "Ctrl+Alt+Down"),
        ] {
            let registered = unsafe { RegisterHotKey(ptr::null_mut(), id, modifiers, key) } != 0;
            if !registered {
                tracing::warn!(shortcut = name, "全局快捷键注册失败，可能已被其他程序占用");
            }
        }
    }

    fn unregister_all() {
        for id in [
            ID_PLAY_PAUSE,
            ID_PREVIOUS,
            ID_NEXT,
            ID_SEEK_BACKWARD,
            ID_SEEK_FORWARD,
            ID_VOLUME_DOWN,
            ID_VOLUME_UP,
            ID_MUTE,
            ID_SHUFFLE,
            ID_REPEAT,
            ID_SHOW_MAIN,
            ID_TOGGLE_STAGE,
            ID_TOGGLE_VISIBLE,
            ID_TOGGLE_LOCK,
            ID_TOGGLE_TRANSLATION,
            ID_FONT_UP,
            ID_FONT_DOWN,
        ] {
            unsafe {
                let _ = UnregisterHotKey(ptr::null_mut(), id);
            }
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::{
        sync::mpsc::{Receiver, Sender},
        time::Duration,
    };

    use super::{AppHotkeyAction, LyricsHotkeyAction, ServiceCommand};

    pub(super) fn run(
        command_rx: Receiver<ServiceCommand>,
        _lyrics_event_tx: Sender<LyricsHotkeyAction>,
        _app_event_tx: Sender<AppHotkeyAction>,
    ) {
        let mut warned = false;
        loop {
            match command_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(ServiceCommand::SetEnabled(true)) => {
                    if !warned {
                        tracing::warn!("系统级全局快捷键目前仅在 Windows 注册");
                        warned = true;
                    }
                }
                Ok(ServiceCommand::SetEnabled(false)) => {}
                Ok(ServiceCommand::Shutdown) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}
