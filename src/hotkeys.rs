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

#[derive(Clone, Copy, Debug)]
enum ServiceCommand {
    SetEnabled(bool),
    Shutdown,
}

struct HotkeyService {
    command_tx: Sender<ServiceCommand>,
    event_rx: Receiver<LyricsHotkeyAction>,
    worker: Option<JoinHandle<()>>,
}

impl HotkeyService {
    fn new() -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("yinqidao-lyrics-hotkeys".into())
            .spawn(move || platform::run(command_rx, event_tx))
            .ok();
        Self {
            command_tx,
            event_rx,
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

pub(crate) fn drain_actions() -> Vec<LyricsHotkeyAction> {
    let Ok(service) = service().lock() else {
        return Vec::new();
    };
    service.event_rx.try_iter().collect()
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

    use super::{LyricsHotkeyAction, ServiceCommand};

    const WM_HOTKEY: u32 = 0x0312;
    const PM_REMOVE: u32 = 0x0001;
    const PM_NOREMOVE: u32 = 0x0000;
    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_NOREPEAT: u32 = 0x4000;

    const ID_TOGGLE_VISIBLE: i32 = 0x5940;
    const ID_TOGGLE_LOCK: i32 = 0x5941;
    const ID_TOGGLE_TRANSLATION: i32 = 0x5942;
    const ID_FONT_UP: i32 = 0x5943;
    const ID_FONT_DOWN: i32 = 0x5944;

    const VK_L: u32 = b'L' as u32;
    const VK_K: u32 = b'K' as u32;
    const VK_T: u32 = b'T' as u32;
    const VK_UP: u32 = 0x26;
    const VK_DOWN: u32 = 0x28;

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

    pub(super) fn run(command_rx: Receiver<ServiceCommand>, event_tx: Sender<LyricsHotkeyAction>) {
        // RegisterHotKey(NULL, ...) posts WM_HOTKEY to this thread. Force creation of the Win32
        // message queue before registering so the first shortcut cannot be lost during startup.
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
                let action = match message.w_param as i32 {
                    ID_TOGGLE_VISIBLE => Some(LyricsHotkeyAction::ToggleVisible),
                    ID_TOGGLE_LOCK => Some(LyricsHotkeyAction::ToggleLock),
                    ID_TOGGLE_TRANSLATION => Some(LyricsHotkeyAction::ToggleTranslation),
                    ID_FONT_UP => Some(LyricsHotkeyAction::IncreaseFont),
                    ID_FONT_DOWN => Some(LyricsHotkeyAction::DecreaseFont),
                    _ => None,
                };
                if let Some(action) = action {
                    let _ = event_tx.send(action);
                }
            }
        }

        if enabled {
            unregister_all();
        }
    }

    fn register_all() {
        let modifiers = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
        for (id, key, name) in [
            (ID_TOGGLE_VISIBLE, VK_L, "Ctrl+Alt+L"),
            (ID_TOGGLE_LOCK, VK_K, "Ctrl+Alt+K"),
            (ID_TOGGLE_TRANSLATION, VK_T, "Ctrl+Alt+T"),
            (ID_FONT_UP, VK_UP, "Ctrl+Alt+Up"),
            (ID_FONT_DOWN, VK_DOWN, "Ctrl+Alt+Down"),
        ] {
            let registered = unsafe { RegisterHotKey(ptr::null_mut(), id, modifiers, key) } != 0;
            if !registered {
                tracing::warn!(shortcut = name, "桌面歌词全局快捷键注册失败，可能已被其他程序占用");
            }
        }
    }

    fn unregister_all() {
        for id in [
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

    use super::{LyricsHotkeyAction, ServiceCommand};

    pub(super) fn run(command_rx: Receiver<ServiceCommand>, _event_tx: Sender<LyricsHotkeyAction>) {
        let mut warned = false;
        loop {
            match command_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(ServiceCommand::SetEnabled(true)) => {
                    if !warned {
                        tracing::warn!("系统级桌面歌词快捷键目前仅在 Windows 注册");
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
