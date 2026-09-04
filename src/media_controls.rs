#![allow(unsafe_code)]

use std::sync::mpsc::Sender;
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};

use crate::model::{PlaybackState, Track};

pub enum SystemMediaEvent {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    SeekBy(i64),
    SetPosition(Duration),
}

pub struct SystemMediaBridge {
    controls: Option<MediaControls>,
    last_track_id: Option<i64>,
    last_state: Option<PlaybackState>,
    last_position_sec: u64,
    #[cfg(target_os = "windows")]
    _hwnd: Option<*mut std::ffi::c_void>,
}

unsafe impl Send for SystemMediaBridge {}
unsafe impl Sync for SystemMediaBridge {}

impl SystemMediaBridge {
    pub fn try_create(event_tx: Sender<SystemMediaEvent>) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        let hwnd = match win32::get_app_or_host_hwnd() {
            Some(h) => h,
            None => return Err("未找到可用窗口句柄 (HWND)".to_string()),
        };

        #[cfg(target_os = "windows")]
        let config = PlatformConfig {
            display_name: "音栖岛",
            dbus_name: "org.mpris.MediaPlayer2.yinqidao",
            hwnd: Some(hwnd),
        };

        #[cfg(not(target_os = "windows"))]
        let config = PlatformConfig {
            display_name: "音栖岛",
            dbus_name: "org.mpris.MediaPlayer2.yinqidao",
            hwnd: None,
        };

        let mut controls = MediaControls::new(config).map_err(|e| format!("{e:?}"))?;

        let tx = event_tx.clone();
        let _ = controls.attach(move |event| {
            let mapped = match event {
                MediaControlEvent::Play => Some(SystemMediaEvent::Play),
                MediaControlEvent::Pause => Some(SystemMediaEvent::Pause),
                MediaControlEvent::Toggle => Some(SystemMediaEvent::Toggle),
                MediaControlEvent::Next => Some(SystemMediaEvent::Next),
                MediaControlEvent::Previous => Some(SystemMediaEvent::Previous),
                MediaControlEvent::Stop => Some(SystemMediaEvent::Stop),
                MediaControlEvent::SeekBy(dir, duration) => {
                    let delta_ms = duration.as_millis() as i64;
                    let signed_delta = match dir {
                        souvlaki::SeekDirection::Forward => delta_ms,
                        souvlaki::SeekDirection::Backward => -delta_ms,
                    };
                    Some(SystemMediaEvent::SeekBy(signed_delta))
                }
                MediaControlEvent::SetPosition(MediaPosition(pos)) => {
                    Some(SystemMediaEvent::SetPosition(pos))
                }
                _ => None,
            };
            if let Some(ev) = mapped {
                let _ = tx.send(ev);
            }
        });

        Ok(Self {
            controls: Some(controls),
            last_track_id: None,
            last_state: None,
            last_position_sec: 0,
            #[cfg(target_os = "windows")]
            _hwnd: Some(hwnd),
        })
    }

    pub fn new(event_tx: Sender<SystemMediaEvent>) -> Option<Self> {
        Self::try_create(event_tx).ok()
    }

    /// 向操作系统同步当前曲目元数据（带状态缓存防抖，避免高频跨进程 COM 调用堵塞 UI 线程）
    pub fn update_metadata(&mut self, track: Option<&Track>) {
        let current_id = track.map(|t| t.id);
        if current_id == self.last_track_id && self.last_track_id.is_some() {
            return;
        }
        self.last_track_id = current_id;

        let Some(controls) = &mut self.controls else {
            return;
        };
        if let Some(track) = track {
            let duration = Duration::from_millis(track.duration_ms);
            let metadata = MediaMetadata {
                title: Some(&track.title),
                album: Some(&track.album),
                artist: Some(&track.artist),
                duration: Some(duration),
                cover_url: None,
            };
            let _ = controls.set_metadata(metadata);
        } else {
            let _ = controls.set_metadata(MediaMetadata::default());
        }
    }

    /// 向操作系统同步当前播放状态与进度时间戳（仅在状态变动或间隔 2 秒以上时同步）
    pub fn update_playback(&mut self, state: PlaybackState, position_ms: u64) {
        let position_sec = position_ms / 1000;
        let state_changed = self.last_state != Some(state);
        let time_jumped = position_sec.abs_diff(self.last_position_sec) >= 2;

        if !state_changed && !time_jumped {
            return;
        }

        self.last_state = Some(state);
        self.last_position_sec = position_sec;

        let Some(controls) = &mut self.controls else {
            return;
        };
        let progress = Some(MediaPosition(Duration::from_millis(position_ms)));
        let playback = match state {
            PlaybackState::Playing => MediaPlayback::Playing { progress },
            PlaybackState::Paused => MediaPlayback::Paused { progress },
            PlaybackState::Stopped
            | PlaybackState::Error
            | PlaybackState::Loading
            | PlaybackState::Buffering => MediaPlayback::Stopped,
        };
        let _ = controls.set_playback(playback);
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetActiveWindow() -> *mut c_void;
        fn GetForegroundWindow() -> *mut c_void;
        fn GetCurrentThreadId() -> u32;
        fn EnumThreadWindows(
            thread_id: u32,
            callback: unsafe extern "system" fn(*mut c_void, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn IsWindow(hwnd: *mut c_void) -> i32;
        fn IsWindowVisible(hwnd: *mut c_void) -> i32;
    }

    unsafe extern "system" fn enum_proc(hwnd: *mut c_void, lparam: isize) -> i32 {
        unsafe {
            let out = lparam as *mut *mut c_void;
            if IsWindow(hwnd) != 0 {
                *out = hwnd;
                if IsWindowVisible(hwnd) != 0 {
                    return 0;
                }
            }
        }
        1
    }

    pub fn get_app_or_host_hwnd() -> Option<*mut c_void> {
        unsafe {
            let active = GetActiveWindow();
            if !active.is_null() && IsWindow(active) != 0 {
                return Some(active);
            }
            let fg = GetForegroundWindow();
            if !fg.is_null() && IsWindow(fg) != 0 {
                return Some(fg);
            }
            let mut found: *mut c_void = null_mut();
            let thread_id = GetCurrentThreadId();
            EnumThreadWindows(thread_id, enum_proc, &mut found as *mut _ as isize);
            if !found.is_null() && IsWindow(found) != 0 {
                return Some(found);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a working interactive desktop media-control bridge"]
    fn test_media_controls_init() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let bridge = SystemMediaBridge::try_create(tx);
        #[cfg(target_os = "windows")]
        {
            if win32::get_app_or_host_hwnd().is_some() {
                assert!(
                    bridge.is_ok(),
                    "有可用窗口时 SystemMediaBridge 应初始化成功"
                );
            } else {
                assert!(bridge.is_err(), "无可用窗口时应优雅返回错误而非 panic");
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            assert!(
                bridge.is_ok(),
                "SystemMediaBridge 初始化失败: {:?}",
                bridge.err()
            );
        }
    }
}
