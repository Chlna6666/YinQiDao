//! Host-owned progressive media source for authenticated plugin streams.
//!
//! The provider never sees a path or socket. Only the cache writer publishes the
//! contiguous, already persisted byte prefix. Readers do not conflate a temporary
//! underrun with end-of-file. No network or filesystem wait enters a CPAL callback.
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    any::Any,
    time::Duration,
};
use symphonia::core::io::MediaSource;

const READ_POLL: Duration = Duration::from_millis(24);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DownloadState {
    Downloading { available: u64 },
    Complete { bytes: u64 },
    Failed(String),
}

#[derive(Debug)]
struct Progress {
    available: u64,
    complete: bool,
    error: Option<String>,
}

pub(crate) struct ProgressiveSession {
    file: PathBuf,
    progress: Mutex<Progress>,
    changed: Condvar,
    // Keep the Host disk reservation alive for the lifetime of any streaming reader.
    reservation: Mutex<Option<Arc<dyn Any + Send + Sync>>>,
}

static SESSIONS: OnceLock<Mutex<HashMap<PathBuf, Weak<ProgressiveSession>>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<PathBuf, Weak<ProgressiveSession>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl ProgressiveSession {
    pub(crate) fn register(file: PathBuf) -> Arc<Self> {
        let session = Arc::new(Self {
            file: file.clone(),
            progress: Mutex::new(Progress {
                available: 0,
                complete: false,
                error: None,
            }),
            changed: Condvar::new(),
            reservation: Mutex::new(None),
        });
        let mut entries = sessions().lock().unwrap_or_else(|e| e.into_inner());
        entries.retain(|_, weak| weak.strong_count() > 0);
        entries.insert(file, Arc::downgrade(&session));
        session
    }

    pub(crate) fn hold_reservation<T: Any + Send + Sync>(&self, reservation: Arc<T>) {
        *self.reservation.lock().unwrap_or_else(|e| e.into_inner()) = Some(reservation);
    }

    pub(crate) fn written(&self, available: u64) {
        let mut state = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        state.available = state.available.max(available);
        self.changed.notify_all();
    }

    pub(crate) fn finish(&self, error: Option<String>) {
        let mut state = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        state.error = error;
        state.complete = true;
        self.changed.notify_all();
    }

    pub(crate) fn snapshot(&self) -> DownloadState {
        let state = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        match &state.error {
            Some(error) => DownloadState::Failed(error.clone()),
            None if state.complete => DownloadState::Complete {
                bytes: state.available,
            },
            None => DownloadState::Downloading {
                available: state.available,
            },
        }
    }

    fn available_at(&self, pos: u64) -> io::Result<usize> {
        let mut state = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if pos < state.available {
                return Ok((state.available - pos).min(usize::MAX as u64) as usize);
            }
            if let Some(error) = &state.error {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, error.clone()));
            }
            if state.complete {
                return Ok(0);
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(state, READ_POLL)
                .unwrap_or_else(|err| err.into_inner());
            state = next;
            if timeout.timed_out() {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "authenticated stream is buffering",
                ));
            }
        }
    }
}

impl Drop for ProgressiveSession {
    fn drop(&mut self) {
        if let Some(registry) = SESSIONS.get()
            && let Ok(mut sessions) = registry.lock()
        {
            sessions.remove(&self.file);
        }
        // The cache writer also owns the session until it has finished. In
        // particular the last reader cannot delete a live download.
        if let Some(dir) = self.file.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

pub(crate) fn lookup(path: &Path) -> Option<Arc<ProgressiveSession>> {
    sessions()
        .lock()
        .ok()?
        .get(path)
        .and_then(Weak::upgrade)
}

pub(crate) struct ProgressiveMediaSource {
    file: File,
    session: Arc<ProgressiveSession>,
    pos: u64,
}

impl ProgressiveMediaSource {
    pub(crate) fn open(path: &Path, session: Arc<ProgressiveSession>) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            session,
            pos: 0,
        })
    }
}

impl Read for ProgressiveMediaSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let count = self.session.available_at(self.pos)?.min(out.len());
        if count == 0 {
            return Ok(0);
        }
        self.file.seek(SeekFrom::Start(self.pos))?;
        let n = self.file.read(&mut out[..count])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "writer-published bytes have not become readable",
            ));
        }
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for ProgressiveMediaSource {
    fn seek(&mut self, target: SeekFrom) -> io::Result<u64> {
        let next = match target {
            SeekFrom::Start(value) => value,
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "negative or overflowing seek")
            })?,
            SeekFrom::End(delta) => {
                let bytes = match self.session.snapshot() {
                    DownloadState::Complete { bytes } => bytes,
                    DownloadState::Failed(message) => {
                        return Err(io::Error::new(io::ErrorKind::BrokenPipe, message));
                    }
                    DownloadState::Downloading { .. } => {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "end-relative seek needs the completed stream length",
                        ));
                    }
                };
                bytes.checked_add_signed(delta).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "negative or overflowing seek")
                })?
            }
        };
        self.pos = next;
        Ok(next)
    }
}

impl MediaSource for ProgressiveMediaSource {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        match self.session.snapshot() {
            DownloadState::Complete { bytes } => Some(bytes),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reader_never_treats_temporary_underrun_as_eof() {
        let root = std::env::temp_dir().join(format!(
            "yqd-progressive-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("audio.mp3");
        let mut writer = File::create(&file).unwrap();
        let session = ProgressiveSession::register(file.clone());
        let mut reader = ProgressiveMediaSource::open(&file, session.clone()).unwrap();
        assert_eq!(reader.read(&mut [0u8; 4]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        writer.write_all(b"abc").unwrap();
        session.written(3);
        let mut bytes = [0; 4];
        assert_eq!(reader.read(&mut bytes).unwrap(), 3);
        assert_eq!(&bytes[..3], b"abc");
        assert_eq!(reader.read(&mut bytes).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        session.finish(None);
        assert_eq!(reader.read(&mut bytes).unwrap(), 0);
        drop(reader);
        drop(writer);
        drop(session);
        assert!(!root.exists());
    }

    #[test]
    fn failures_are_not_reported_as_successful_eof() {
        let root = std::env::temp_dir().join(format!("yqd-failed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("audio.flac");
        File::create(&path).unwrap();
        let session = ProgressiveSession::register(path.clone());
        let mut reader = ProgressiveMediaSource::open(&path, session.clone()).unwrap();
        session.finish(Some("network closed".into()));
        assert_eq!(reader.read(&mut [0u8; 1]).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        drop(reader);
        drop(session);
    }
}
