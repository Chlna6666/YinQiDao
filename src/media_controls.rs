#![allow(unsafe_code)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::Sender,
    },
    time::Duration,
};

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};

use crate::model::{PlaybackState, Track};

mod discord;

use discord::DiscordPresence;

const NO_MPRIS_VOLUME_REQUEST: u64 = u64::MAX;

pub enum SystemMediaEvent {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    SeekBy(i64),
    SetPosition(Duration),
    SetVolume(f32),
}

pub struct SystemMediaBridge {
    controls: Option<MediaControls>,
    last_metadata_fingerprint: Option<u64>,
    last_state: Option<PlaybackState>,
    last_position_sec: u64,
    mpris_volume_request: Arc<AtomicU64>,
    discord: Option<DiscordPresence>,
    #[cfg(target_os = "windows")]
    _hwnd: Option<*mut std::ffi::c_void>,
}

// The bridge is moved as a single owner into the serialized blocking worker and moved back when
// the update finishes. It is never concurrently shared, so Send is required here but Sync is not.
unsafe impl Send for SystemMediaBridge {}

impl SystemMediaBridge {
    pub fn try_create(event_tx: Sender<SystemMediaEvent>) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        let hwnd = match win32::get_app_hwnd() {
            Some(h) => h,
            None => return Err("未找到可用窗口句柄 (HWND)".to_string()),
        };

        #[cfg(target_os = "windows")]
        let config = PlatformConfig {
            display_name: "音栖岛",
            dbus_name: "yinqidao",
            hwnd: Some(hwnd),
        };

        #[cfg(not(target_os = "windows"))]
        let config = PlatformConfig {
            display_name: "音栖岛",
            dbus_name: "yinqidao",
            hwnd: None,
        };

        let mut controls = MediaControls::new(config).map_err(|e| format!("{e:?}"))?;
        let mpris_volume_request = Arc::new(AtomicU64::new(NO_MPRIS_VOLUME_REQUEST));
        let requested_volume = mpris_volume_request.clone();
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
                    let delta_ms = duration.as_millis().min(i64::MAX as u128) as i64;
                    let signed_delta = match dir {
                        souvlaki::SeekDirection::Forward => delta_ms,
                        souvlaki::SeekDirection::Backward => -delta_ms,
                    };
                    Some(SystemMediaEvent::SeekBy(signed_delta))
                }
                MediaControlEvent::SetPosition(MediaPosition(pos)) => {
                    Some(SystemMediaEvent::SetPosition(pos))
                }
                MediaControlEvent::SetVolume(volume) if volume.is_finite() => {
                    let volume = volume.clamp(0.0, 1.0);
                    requested_volume.store(volume.to_bits(), Ordering::Release);
                    Some(SystemMediaEvent::SetVolume(volume as f32))
                }
                _ => None,
            };
            if let Some(ev) = mapped {
                let _ = tx.send(ev);
            }
        });

        Ok(Self {
            controls: Some(controls),
            last_metadata_fingerprint: None,
            last_state: None,
            last_position_sec: 0,
            mpris_volume_request,
            discord: DiscordPresence::from_env(),
            #[cfg(target_os = "windows")]
            _hwnd: Some(hwnd),
        })
    }

    pub fn new(event_tx: Sender<SystemMediaEvent>) -> Option<Self> {
        Self::try_create(event_tx).ok()
    }

    /// 向操作系统与 Discord 同步当前曲目元数据。元数据指纹包含标题/歌手/专辑/时长，
    /// 因此同一 TrackId 在联网补全后也会重新发布，而不是被旧的 ID-only 缓存吞掉。
    pub fn update_metadata(&mut self, track: Option<&Track>) {
        let fingerprint = metadata_fingerprint(track);
        if self.last_metadata_fingerprint == Some(fingerprint) {
            return;
        }

        if let Some(discord) = &mut self.discord {
            discord.update_metadata(track);
        }

        let Some(controls) = &mut self.controls else {
            return;
        };
        let result = if let Some(track) = track {
            let duration = Duration::from_millis(track.duration_ms);
            let metadata = MediaMetadata {
                title: Some(&track.title),
                album: Some(&track.album),
                artist: Some(&track.artist),
                duration: Some(duration),
                cover_url: None,
            };
            controls.set_metadata(metadata)
        } else {
            controls.set_metadata(MediaMetadata::default())
        };

        // Only commit the dedupe key after the platform backend accepted the update. A transient
        // COM/D-Bus/Now Playing failure must remain retryable on the next serialized media sync.
        if result.is_ok() {
            self.last_metadata_fingerprint = Some(fingerprint);
        }
    }

    /// 同步当前播放状态与进度。系统后端只在状态变化或时间跳变时更新；Discord 自己
    /// 使用 transport anchor 去除连续播放的 2 秒维护采样，只在曲目/状态/seek 改变时发 IPC。
    pub fn update_playback(&mut self, state: PlaybackState, position_ms: u64) {
        self.confirm_mpris_volume_request();
        if let Some(discord) = &mut self.discord {
            discord.update_playback(state, position_ms);
        }

        let position_sec = position_ms / 1000;
        let state_changed = self.last_state != Some(state);
        let time_jumped = position_sec.abs_diff(self.last_position_sec) >= 2;

        if !state_changed && !time_jumped {
            return;
        }

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
        // Do not poison the local cache when the platform backend rejects an update. Keeping the
        // previous state/position makes the same state retryable on the next bridge invocation.
        if controls.set_playback(playback).is_ok() {
            self.last_state = Some(state);
            self.last_position_sec = position_sec;
        }
    }

    fn confirm_mpris_volume_request(&mut self) {
        #[cfg(target_os = "linux")]
        {
            let bits = self
                .mpris_volume_request
                .swap(NO_MPRIS_VOLUME_REQUEST, Ordering::AcqRel);
            if bits == NO_MPRIS_VOLUME_REQUEST {
                return;
            }
            let volume = f64::from_bits(bits).clamp(0.0, 1.0);
            let Some(controls) = &mut self.controls else {
                self.mpris_volume_request.store(bits, Ordering::Release);
                return;
            };
            if controls.set_volume(volume).is_err() {
                self.mpris_volume_request.store(bits, Ordering::Release);
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = self
                .mpris_volume_request
                .swap(NO_MPRIS_VOLUME_REQUEST, Ordering::AcqRel);
        }
    }
}

fn metadata_fingerprint(track: Option<&Track>) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    #[inline]
    fn mix(hash: &mut u64, bytes: &[u8]) {
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(PRIME);
        }
    }

    let Some(track) = track else {
        return OFFSET.wrapping_mul(PRIME);
    };
    let mut hash = OFFSET;
    mix(&mut hash, &track.id.to_le_bytes());
    mix(&mut hash, track.title.as_bytes());
    mix(&mut hash, &[0xff]);
    mix(&mut hash, track.artist.as_bytes());
    mix(&mut hash, &[0xfe]);
    mix(&mut hash, track.album.as_bytes());
    mix(&mut hash, &track.duration_ms.to_le_bytes());
    hash
}

#[cfg(target_os = "windows")]
mod win32 {
    use std::{ffi::c_void, ptr::null_mut};

    struct WindowSearch {
        process_id: u32,
        visible: *mut c_void,
        fallback: *mut c_void,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn EnumWindows(
            callback: unsafe extern "system" fn(*mut c_void, isize) -> i32,
            lparam: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(hwnd: *mut c_void, process_id: *mut u32) -> u32;
        fn IsWindow(hwnd: *mut c_void) -> i32;
        fn IsWindowVisible(hwnd: *mut c_void) -> i32;
    }

    unsafe extern "system" fn enum_proc(hwnd: *mut c_void, lparam: isize) -> i32 {
        unsafe {
            if IsWindow(hwnd) == 0 {
                return 1;
            }

            let search = &mut *(lparam as *mut WindowSearch);
            let mut process_id = 0_u32;
            GetWindowThreadProcessId(hwnd, &mut process_id);
            if process_id != search.process_id {
                return 1;
            }

            if search.fallback.is_null() {
                search.fallback = hwnd;
            }
            if IsWindowVisible(hwnd) != 0 {
                search.visible = hwnd;
                return 0;
            }
        }
        1
    }

    pub fn get_app_hwnd() -> Option<*mut c_void> {
        let mut search = WindowSearch {
            process_id: std::process::id(),
            visible: null_mut(),
            fallback: null_mut(),
        };
        unsafe {
            EnumWindows(enum_proc, &mut search as *mut _ as isize);
        }

        let hwnd = if search.visible.is_null() {
            search.fallback
        } else {
            search.visible
        };
        (!hwnd.is_null()).then_some(hwnd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_fingerprint_changes_when_enrichment_changes_text() {
        let first = Track::new(crate::model::TrackData {
            id: 7,
            path: std::path::PathBuf::from("track.flac"),
            title: "旧标题".into(),
            artist: "歌手".into(),
            album: "专辑".into(),
            year: None,
            genre: None,
            duration_ms: 10_000,
            codec: "flac".into(),
            sample_rate: 48_000,
            channels: 2,
            artwork_key: None,
        });
        let mut second = first.clone();
        second.title = "新标题".into();
        assert_ne!(
            metadata_fingerprint(Some(&first)),
            metadata_fingerprint(Some(&second))
        );
    }

    #[test]
    #[ignore = "requires a working interactive desktop media-control bridge"]
    fn test_media_controls_init() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let bridge = SystemMediaBridge::try_create(tx);
        #[cfg(target_os = "windows")]
        {
            if win32::get_app_hwnd().is_some() {
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
