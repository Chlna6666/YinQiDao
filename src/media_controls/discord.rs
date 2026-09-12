use std::{
    env,
    io::{self, Read, Write},
    process,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};

use crate::model::{PlaybackState, Track, TrackId};

const DISCORD_CLIENT_ID_ENV: &str = "YINQIDAO_DISCORD_CLIENT_ID";
const RETRY_BACKOFF: Duration = Duration::from_secs(30);
const SEEK_RESYNC_THRESHOLD_MS: u64 = 3_000;
const IPC_OPCODE_HANDSHAKE: u32 = 0;
const IPC_OPCODE_FRAME: u32 = 1;
const RESPONSE_BUFFER_SIZE: usize = 4_096;

#[cfg(windows)]
type PlatformIpc = std::fs::File;
#[cfg(unix)]
type PlatformIpc = std::os::unix::net::UnixStream;

struct IpcStream(PlatformIpc);

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl IpcStream {
    fn drain_responses(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let mut buffer = [0_u8; RESPONSE_BUFFER_SIZE];
            loop {
                match self.0.read(&mut buffer) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "Discord IPC 已断开",
                        ));
                    }
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) =>
                    {
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                }
            }
        }

        #[cfg(windows)]
        {
            use std::{ffi::c_void, os::windows::io::AsRawHandle, ptr::null_mut};

            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn PeekNamedPipe(
                    pipe: *mut c_void,
                    buffer: *mut c_void,
                    buffer_size: u32,
                    bytes_read: *mut u32,
                    bytes_available: *mut u32,
                    bytes_left_this_message: *mut u32,
                ) -> i32;
            }

            let mut buffer = [0_u8; RESPONSE_BUFFER_SIZE];
            loop {
                let mut available = 0_u32;
                let ok = unsafe {
                    PeekNamedPipe(
                        self.0.as_raw_handle() as *mut c_void,
                        null_mut(),
                        0,
                        null_mut(),
                        &mut available,
                        null_mut(),
                    )
                };
                if ok == 0 {
                    return Err(io::Error::last_os_error());
                }
                if available == 0 {
                    return Ok(());
                }
                let amount = usize::try_from(available)
                    .unwrap_or(usize::MAX)
                    .min(buffer.len());
                self.0.read_exact(&mut buffer[..amount])?;
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscordTrack {
    id: TrackId,
    title: String,
    artist: String,
    album: String,
    duration_ms: u64,
}

impl From<&Track> for DiscordTrack {
    fn from(track: &Track) -> Self {
        Self {
            id: track.id,
            title: track.title.clone(),
            artist: track.artist.clone(),
            album: track.album.clone(),
            duration_ms: track.duration_ms,
        }
    }
}

#[derive(Clone, Copy)]
struct PublishedPlayback {
    track_id: TrackId,
    metadata_revision: u64,
    state: PlaybackState,
    position_ms: u64,
    published_at: Instant,
}

pub(super) struct DiscordPresence {
    client_id: String,
    stream: Option<IpcStream>,
    retry_after: Instant,
    nonce: u64,
    track: Option<DiscordTrack>,
    metadata_revision: u64,
    state: PlaybackState,
    position_ms: u64,
    published: Option<PublishedPlayback>,
}

impl DiscordPresence {
    pub(super) fn from_env() -> Option<Self> {
        let client_id = env::var(DISCORD_CLIENT_ID_ENV).ok()?;
        let client_id = client_id.trim();
        if client_id.is_empty() || !client_id.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(Self {
            client_id: client_id.to_owned(),
            stream: None,
            retry_after: Instant::now(),
            nonce: 0,
            track: None,
            metadata_revision: 0,
            state: PlaybackState::Stopped,
            position_ms: 0,
            published: None,
        })
    }

    pub(super) fn update_metadata(&mut self, track: Option<&Track>) {
        let next = track.map(DiscordTrack::from);
        if self.track == next {
            return;
        }
        self.track = next;
        self.metadata_revision = self.metadata_revision.wrapping_add(1);
    }

    pub(super) fn update_playback(&mut self, state: PlaybackState, position_ms: u64) {
        self.check_connection();
        let state_changed = self.state != state;
        self.state = state;
        self.position_ms = position_ms;

        let current_track_id = self.track.as_ref().map_or(i64::MIN, |track| track.id);
        let context_changed = self.published.is_none_or(|published| {
            published.track_id != current_track_id
                || published.metadata_revision != self.metadata_revision
        });
        let seeked = self.published.is_some_and(|published| {
            if published.track_id != current_track_id
                || published.state != state
                || published.metadata_revision != self.metadata_revision
            {
                return false;
            }
            let expected = if state == PlaybackState::Playing {
                published.position_ms.saturating_add(
                    published
                        .published_at
                        .elapsed()
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64,
                )
            } else {
                published.position_ms
            };
            position_ms.abs_diff(expected) >= SEEK_RESYNC_THRESHOLD_MS
        });

        if state_changed || context_changed || seeked {
            self.publish();
        }
    }

    fn check_connection(&mut self) {
        let disconnected = self
            .stream
            .as_mut()
            .is_some_and(|stream| stream.drain_responses().is_err());
        if disconnected {
            self.disconnect_with_backoff();
        }
    }

    fn publish(&mut self) {
        let Some(track) = self.track.clone() else {
            if self.published.take().is_some() {
                let _ = self.send_activity(Value::Null);
            }
            return;
        };

        if self.state == PlaybackState::Stopped || self.state == PlaybackState::Error {
            if self.published.take().is_some() {
                let _ = self.send_activity(Value::Null);
            }
            return;
        }

        let mut state_text = if track.album.trim().is_empty() {
            track.artist.clone()
        } else {
            format!("{} · {}", track.artist, track.album)
        };
        match self.state {
            PlaybackState::Paused => state_text.push_str(" · 已暂停"),
            PlaybackState::Buffering | PlaybackState::Loading => state_text.push_str(" · 缓冲中"),
            _ => {}
        }
        let mut activity = json!({
            "details": track.title.clone(),
            "state": state_text,
            "instance": false
        });
        if self.state == PlaybackState::Playing
            && let Some((start, end)) = activity_timestamps(self.position_ms, track.duration_ms)
        {
            activity["timestamps"] = json!({ "start": start, "end": end });
        }

        if self.send_activity(activity) {
            self.published = Some(PublishedPlayback {
                track_id: track.id,
                metadata_revision: self.metadata_revision,
                state: self.state,
                position_ms: self.position_ms,
                published_at: Instant::now(),
            });
        }
    }

    fn send_activity(&mut self, activity: Value) -> bool {
        if !self.ensure_connected() {
            return false;
        }
        self.nonce = self.nonce.wrapping_add(1);
        let payload = json!({
            "cmd": "SET_ACTIVITY",
            "args": {
                "pid": process::id(),
                "activity": activity
            },
            "nonce": self.nonce.to_string()
        });
        if self.write_json_frame(IPC_OPCODE_FRAME, &payload).is_ok() {
            true
        } else {
            self.disconnect_with_backoff();
            false
        }
    }

    fn ensure_connected(&mut self) -> bool {
        if let Some(stream) = &mut self.stream {
            if stream.drain_responses().is_ok() {
                return true;
            }
            self.disconnect_with_backoff();
        }
        if Instant::now() < self.retry_after {
            return false;
        }

        let Ok(stream) = connect_ipc() else {
            self.retry_after = Instant::now() + RETRY_BACKOFF;
            return false;
        };
        self.stream = Some(stream);
        let handshake = json!({ "v": 1, "client_id": self.client_id.as_str() });
        if self.write_json_frame(IPC_OPCODE_HANDSHAKE, &handshake).is_err() {
            self.disconnect_with_backoff();
            return false;
        }
        true
    }

    fn write_json_frame(&mut self, opcode: u32, payload: &Value) -> io::Result<()> {
        let bytes = serde_json::to_vec(payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let len = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Discord IPC 帧过大"))?;
        let Some(stream) = self.stream.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Discord IPC 未连接",
            ));
        };
        stream.drain_responses()?;
        stream.write_all(&opcode.to_le_bytes())?;
        stream.write_all(&len.to_le_bytes())?;
        stream.write_all(&bytes)?;
        stream.flush()?;
        stream.drain_responses()
    }

    fn disconnect_with_backoff(&mut self) {
        self.stream = None;
        self.published = None;
        self.retry_after = Instant::now() + RETRY_BACKOFF;
    }
}

fn activity_timestamps(position_ms: u64, duration_ms: u64) -> Option<(u64, u64)> {
    if duration_ms == 0 {
        return None;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let position_sec = position_ms.min(duration_ms) / 1_000;
    let duration_sec = duration_ms / 1_000;
    let start = now.saturating_sub(position_sec);
    Some((start, start.saturating_add(duration_sec)))
}

#[cfg(windows)]
fn connect_ipc() -> io::Result<IpcStream> {
    use std::fs::OpenOptions;

    let mut last_error = None;
    for slot in 0..10_u8 {
        let path = format!(r"\\.\pipe\discord-ipc-{slot}");
        match OpenOptions::new().read(true).write(true).open(path) {
            Ok(stream) => return Ok(IpcStream(stream)),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Discord IPC 不可用")))
}

#[cfg(unix)]
fn connect_ipc() -> io::Result<IpcStream> {
    use std::{collections::HashSet, os::unix::net::UnixStream, path::PathBuf};

    const SANDBOX_DIRS: [&str; 3] = [
        "app/com.discordapp.Discord",
        "app/com.discordapp.DiscordCanary",
        "app/dev.vencord.Vesktop",
    ];

    let mut roots = Vec::new();
    for key in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Some(value) = env::var_os(key) {
            roots.push(PathBuf::from(value));
        }
    }
    roots.push(PathBuf::from("/tmp"));

    let mut seen = HashSet::new();
    let mut last_error = None;
    for root in roots {
        if !seen.insert(root.clone()) {
            continue;
        }
        for slot in 0..10_u8 {
            let socket = format!("discord-ipc-{slot}");
            let direct = root.join(&socket);
            match UnixStream::connect(&direct) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(Duration::from_millis(2)))?;
                    return Ok(IpcStream(stream));
                }
                Err(error) => last_error = Some(error),
            }
            for sandbox in SANDBOX_DIRS {
                let path = root.join(sandbox).join(&socket);
                match UnixStream::connect(path) {
                    Ok(stream) => {
                        stream.set_read_timeout(Some(Duration::from_millis(2)))?;
                        return Ok(IpcStream(stream));
                    }
                    Err(error) => last_error = Some(error),
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Discord IPC 不可用")))
}

#[cfg(not(any(windows, unix)))]
compile_error!("Discord IPC implementation requires Windows or Unix");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_timestamps_preserve_transport_offset() {
        let (start, end) = activity_timestamps(30_000, 120_000).expect("timestamps");
        assert_eq!(end.saturating_sub(start), 120);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs();
        assert!(now.abs_diff(start.saturating_add(30)) <= 1);
    }
}
