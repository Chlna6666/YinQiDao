use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc::Sender},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    lyrics::{LyricsDocument, read_local},
    model::{Track, TrackId},
    online::EnrichmentResult,
};
use rayon::prelude::*;

mod metadata;

use metadata::read_track;

const SUPPORTED_EXTENSIONS: &[&str] = &[
    "aac", "aiff", "alac", "caf", "flac", "m4a", "mka", "mkv", "mp3", "mp4", "oga", "ogg", "opus",
    "wav", "webm",
];

#[derive(Clone, Debug)]
pub struct ScanReport {
    pub root: PathBuf,
    pub discovered: usize,
    pub imported: usize,
    pub removed: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

impl ScanReport {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            discovered: 0,
            imported: 0,
            removed: 0,
            failed: 0,
            errors: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Library {
    db_path: PathBuf,
    scan_gate: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Default)]
pub struct StoredEnrichment {
    pub checked_online: bool,
    pub lyrics: Option<LyricsDocument>,
}

impl Library {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建歌库目录失败: {}", parent.display()))?;
        }
        let library = Self {
            db_path,
            scan_gate: Arc::new(Mutex::new(())),
        };
        library.with_connection(initialize_schema)?;
        Ok(library)
    }

    pub fn add_root(&self, root: &Path) -> Result<()> {
        let root = root.to_string_lossy();
        self.with_connection(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO library_roots(path, scanned_at) VALUES (?1, NULL)",
                params![root.as_ref()],
            )?;
            Ok(())
        })
    }

    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        self.with_connection(|connection| {
            let mut statement =
                connection.prepare("SELECT path FROM library_roots ORDER BY path")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.map(|row| row.map(PathBuf::from))
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
    }

    pub fn scan_root(&self, root: &Path) -> Result<ScanReport> {
        let _scan_guard = self
            .scan_gate
            .lock()
            .map_err(|_| anyhow!("歌库扫描锁已损坏"))?;
        let root = root.to_path_buf();
        let mut report = ScanReport::new(root.clone());
        let mut files = Vec::new();
        collect_audio_files(&root, &mut files, &mut report.errors);
        report.discovered = files.len();

        // 1. 从数据库读取该根目录已收录文件的索引字典 (path -> (file_size, modified_at))
        let existing_index: HashMap<String, (i64, i64)> = self.with_connection(|conn| {
            let root_prefix = format!("{}/", normalize_path(&root));
            let mut stmt = conn.prepare(
                "SELECT path, COALESCE(file_size, 0), COALESCE(modified_at, 0)
                 FROM tracks WHERE path = ?1 OR path LIKE ?2",
            )?;
            let rows = stmt.query_map(
                params![normalize_path(&root), format!("{root_prefix}%")],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
                    ))
                },
            )?;
            let mut map = HashMap::new();
            for r in rows.flatten() {
                map.insert(r.0, r.1);
            }
            Ok(map)
        })?;

        // 2. 增量筛选：对已存在且 mtime、大小均未变动的文件直接跳过，零 CPU 浪费！
        let mut to_parse = Vec::new();
        let mut discovered = HashSet::new();

        for path in files {
            let normalized = normalize_path(&path);
            discovered.insert(normalized.clone());

            let (file_size, mtime) = match fs::metadata(&path) {
                Ok(meta) => {
                    let size = meta.len() as i64;
                    let mtime = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    (size, mtime)
                }
                Err(_) => (0, 0),
            };

            // 如果文件已存在数据库中，且大小与修改时间一致：增量命中，跳过昂贵的音频解码！
            if let Some(&(old_size, old_mtime)) = existing_index.get(&normalized)
                && old_size == file_size
                && old_mtime == mtime
                && old_size > 0
            {
                continue;
            }

            to_parse.push((path, normalized, file_size, mtime));
        }

        // 3. 仅对新增或修改的文件解析，并限制计算线程数，给 UI 和音频留出 CPU。
        let parsed_results: Vec<(PathBuf, String, i64, i64, Result<Track>)> =
            if let Ok(pool) = crate::runtime::compute_pool() {
                pool.install(|| {
                    to_parse
                        .into_par_iter()
                        .map(|(path, normalized, file_size, mtime)| {
                            let result = read_track(&path);
                            (path, normalized, file_size, mtime, result)
                        })
                        .collect()
                })
            } else {
                to_parse
                    .into_iter()
                    .map(|(path, normalized, file_size, mtime)| {
                        let result = read_track(&path);
                        (path, normalized, file_size, mtime, result)
                    })
                    .collect()
            };

        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            for (path, normalized, file_size, mtime, track_res) in parsed_results {
                match track_res {
                    Ok(track) => {
                        upsert_track(&transaction, &track, file_size, mtime)?;
                        report.imported += 1;
                    }
                    Err(error) => {
                        report.failed += 1;
                        let message = format!("{}: {error:#}", path.display());
                        report.errors.push(message.clone());
                        transaction.execute(
                            "INSERT INTO scan_errors(path, error, scanned_at) VALUES (?1, ?2, ?3)
                             ON CONFLICT(path) DO UPDATE SET error=excluded.error, scanned_at=excluded.scanned_at",
                            params![normalized, message, now_unix()],
                        )?;
                    }
                }
            }

            let root_prefix = format!("{}/", normalize_path(&root));
            let mut statement = transaction.prepare("SELECT id, path FROM tracks WHERE path = ?1 OR path LIKE ?2")?;
            let rows = statement.query_map(params![normalize_path(&root), format!("{root_prefix}%")], |row| {
                Ok((row.get::<_, TrackId>(0)?, row.get::<_, String>(1)?))
            })?;
            let stale = rows
                .filter_map(|row| row.ok())
                .filter(|(_, path)| !Path::new(path).exists() || !discovered.contains(path))
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            drop(statement);
            for id in stale {
                transaction.execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
                report.removed += 1;
            }
            transaction.execute(
                "INSERT INTO library_roots(path, scanned_at) VALUES (?1, ?2)
                 ON CONFLICT(path) DO UPDATE SET scanned_at=excluded.scanned_at",
                params![normalize_path(&root), now_unix()],
            )?;
            transaction.commit()?;
            Ok(())
        })?;
        Ok(report)
    }

    pub fn reset_index(&self) -> Result<()> {
        self.with_connection(|connection| {
            connection.execute("DELETE FROM tracks", [])?;
            connection.execute("DELETE FROM scan_errors", [])?;
            connection.execute("UPDATE library_roots SET scanned_at = NULL", [])?;
            Ok(())
        })
    }

    pub fn scan_all(&self, roots: &[PathBuf]) -> Result<Vec<ScanReport>> {
        roots.iter().map(|root| self.scan_root(root)).collect()
    }

    pub fn tracks(&self, search: Option<&str>) -> Result<Vec<Track>> {
        self.with_connection(|connection| {
            let mut statement = if search.is_some_and(|value| !value.trim().is_empty()) {
                connection.prepare(
                    "SELECT id, path, title, artist, album, year, genre, duration_ms, codec,
                     sample_rate, channels, artwork_key FROM tracks
                     WHERE title LIKE ?1 OR artist LIKE ?1 OR album LIKE ?1 ORDER BY title COLLATE NOCASE",
                )?
            } else {
                connection.prepare(
                    "SELECT id, path, title, artist, album, year, genre, duration_ms, codec,
                     sample_rate, channels, artwork_key FROM tracks ORDER BY title COLLATE NOCASE",
                )?
            };
            let rows = if let Some(search) = search.filter(|value| !value.trim().is_empty()) {
                let pattern = format!("%{}%", search.trim());
                statement.query_map(params![pattern], track_from_row)?
            } else {
                statement.query_map([], track_from_row)?
            };
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub fn enrichment(&self, track: &Track) -> Result<StoredEnrichment> {
        let stored = self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT checked_online, lyrics_plain, lyrics_synced, lyrics_source
                     FROM track_enrichment WHERE track_id = ?1",
                    params![track.id],
                    |row| {
                        let plain: Option<String> = row.get(1)?;
                        let synced: Option<String> = row.get(2)?;
                        let source: Option<String> = row.get(3)?;
                        Ok(StoredEnrichment {
                            checked_online: row.get::<_, i64>(0)? != 0,
                            lyrics: (plain.is_some() || synced.is_some()).then(|| {
                                LyricsDocument::from_sources(
                                    plain.clone(),
                                    synced,
                                    None,
                                    source.unwrap_or_else(|| "缓存".into()),
                                )
                            }),
                        })
                    },
                )
                .optional()
                .map_err(Into::into)
        })?;
        Ok(stored.unwrap_or_else(|| StoredEnrichment {
            checked_online: false,
            lyrics: read_local(&track.path),
        }))
    }

    pub fn apply_enrichment(
        &self,
        track_id: TrackId,
        enrichment: &EnrichmentResult,
        artwork_key: Option<&str>,
    ) -> Result<()> {
        self.with_connection(|connection| {
            let transaction = connection.transaction()?;
            if let Some(metadata) = &enrichment.metadata {
                transaction.execute(
                    "UPDATE tracks SET title = ?1, artist = ?2, album = ?3,
                     year = COALESCE(?4, year), artwork_key = COALESCE(?5, artwork_key)
                     WHERE id = ?6",
                    params![
                        metadata.title,
                        metadata.artist,
                        metadata.album,
                        metadata.release_date.as_deref().and_then(|date| {
                            date.get(..4).and_then(|year| year.parse::<i32>().ok())
                        }),
                        artwork_key,
                        track_id
                    ],
                )?;
            } else if artwork_key.is_some() {
                transaction.execute(
                    "UPDATE tracks SET artwork_key = ?1 WHERE id = ?2",
                    params![artwork_key, track_id],
                )?;
            }
            let lyrics = enrichment.lyrics.as_ref();
            transaction.execute(
                "INSERT INTO track_enrichment(
                    track_id, checked_online, recording_mbid, release_mbid,
                    lyrics_plain, lyrics_synced, lyrics_source, updated_at
                 ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(track_id) DO UPDATE SET
                    checked_online = 1,
                    recording_mbid = COALESCE(excluded.recording_mbid, recording_mbid),
                    release_mbid = COALESCE(excluded.release_mbid, release_mbid),
                    lyrics_plain = COALESCE(excluded.lyrics_plain, lyrics_plain),
                    lyrics_synced = COALESCE(excluded.lyrics_synced, lyrics_synced),
                    lyrics_source = COALESCE(excluded.lyrics_source, lyrics_source),
                    updated_at = excluded.updated_at",
                params![
                    track_id,
                    enrichment
                        .metadata
                        .as_ref()
                        .map(|metadata| metadata.recording_mbid.as_str()),
                    enrichment
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.release_mbid.as_deref()),
                    lyrics.and_then(|lyrics| lyrics.plain.as_deref()),
                    lyrics.and_then(|lyrics| lyrics.synced.as_deref()),
                    lyrics.map(|lyrics| lyrics.source.as_str()),
                    now_unix(),
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
    }

    pub fn start_watching_with_signal(
        &self,
        root: &Path,
        signal: Sender<()>,
    ) -> notify::Result<RecommendedWatcher> {
        let library = self.clone();
        let root = root.to_path_buf();
        let watch_root = root.clone();
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel(1);
        let worker_root = root.clone();
        thread::Builder::new()
            .name("yinqidao-library-watcher".into())
            .spawn(move || {
                while event_rx.recv().is_ok() {
                    // 合并一个短窗口内的文件系统事件；扫描期间到来的事件会留在有界队列中。
                    while event_rx.recv_timeout(Duration::from_millis(300)).is_ok() {}
                    let _ = library.scan_root(&worker_root);
                    let _ = signal.send(());
                }
            })
            .map_err(|error| notify::Error::generic(&format!("创建歌库监听线程失败: {error}")))?;
        let mut watcher = RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    let _ = event_tx.try_send(());
                }
            },
            Config::default(),
        )?;
        watcher.watch(watch_root.as_path(), RecursiveMode::Recursive)?;
        Ok(watcher)
    }

    fn with_connection<T>(&self, function: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = Connection::open(&self.db_path)
            .with_context(|| format!("打开歌库数据库失败: {}", self.db_path.display()))?;
        connection.busy_timeout(Duration::from_millis(500))?;
        function(&mut connection)
    }
}

fn initialize_schema(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS library_roots (
            path TEXT PRIMARY KEY NOT NULL,
            scanned_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS tracks (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            title TEXT NOT NULL,
            artist TEXT NOT NULL,
            album TEXT NOT NULL,
            year INTEGER,
            genre TEXT,
            duration_ms INTEGER NOT NULL,
            codec TEXT NOT NULL,
            sample_rate INTEGER NOT NULL,
            channels INTEGER NOT NULL,
            artwork_key TEXT,
            file_size INTEGER,
            modified_at INTEGER
         );
         CREATE TABLE IF NOT EXISTS scan_errors (
            path TEXT PRIMARY KEY NOT NULL,
            error TEXT NOT NULL,
            scanned_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS track_enrichment (
            track_id INTEGER PRIMARY KEY NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            checked_online INTEGER NOT NULL DEFAULT 0,
            recording_mbid TEXT,
            release_mbid TEXT,
            lyrics_plain TEXT,
            lyrics_synced TEXT,
            lyrics_source TEXT,
            updated_at INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS playlists (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE
         );
         CREATE TABLE IF NOT EXISTS playlist_tracks (
            playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            PRIMARY KEY (playlist_id, track_id)
         );",
    )?;
    // 兼容已有数据库模式升级
    let _ = connection.execute("ALTER TABLE tracks ADD COLUMN file_size INTEGER", []);
    let _ = connection.execute("ALTER TABLE tracks ADD COLUMN modified_at INTEGER", []);
    Ok(())
}

fn collect_audio_files(root: &Path, files: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        errors.push(format!("无法读取目录: {}", root.display()));
        return;
    };
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_dir() {
                    collect_audio_files(&path, files, errors);
                } else if is_supported_audio_path(&path) {
                    files.push(path);
                }
            }
            Err(error) => errors.push(format!("读取目录项失败: {error}")),
        }
    }
}

pub fn is_supported_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn upsert_track(
    connection: &Connection,
    track: &Track,
    file_size: i64,
    modified_at: i64,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO tracks(path, title, artist, album, year, genre, duration_ms, codec, sample_rate, channels, artwork_key, file_size, modified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(path) DO UPDATE SET
         title=CASE WHEN EXISTS(SELECT 1 FROM track_enrichment WHERE track_id=tracks.id AND checked_online=1)
                    THEN tracks.title ELSE excluded.title END,
         artist=CASE WHEN EXISTS(SELECT 1 FROM track_enrichment WHERE track_id=tracks.id AND checked_online=1)
                     THEN tracks.artist ELSE excluded.artist END,
         album=CASE WHEN EXISTS(SELECT 1 FROM track_enrichment WHERE track_id=tracks.id AND checked_online=1)
                    THEN tracks.album ELSE excluded.album END,
         year=CASE WHEN EXISTS(SELECT 1 FROM track_enrichment WHERE track_id=tracks.id AND checked_online=1)
                   THEN tracks.year ELSE excluded.year END,
         genre=excluded.genre, duration_ms=excluded.duration_ms,
         codec=excluded.codec, sample_rate=excluded.sample_rate, channels=excluded.channels,
         artwork_key=COALESCE(tracks.artwork_key, excluded.artwork_key),
         file_size=excluded.file_size,
         modified_at=excluded.modified_at",
        params![
            normalize_path(&track.path),
            &track.title,
            &track.artist,
            &track.album,
            track.year,
            track.genre.as_deref(),
            track.duration_ms,
            &track.codec,
            track.sample_rate,
            track.channels,
            track.artwork_key.as_deref(),
            file_size,
            modified_at,
        ],
    )?;
    Ok(())
}

fn track_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        title: row.get(2)?,
        artist: row.get(3)?,
        album: row.get(4)?,
        year: row.get(5)?,
        genre: row.get(6)?,
        duration_ms: row.get(7)?,
        codec: row.get(8)?,
        sample_rate: row.get(9)?,
        channels: row.get(10)?,
        artwork_key: row.get(11)?,
    })
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, time::SystemTime};

    use super::*;

    #[test]
    fn extension_probe_is_case_insensitive_and_rejects_unknown_files() {
        assert!(is_supported_audio_path(Path::new("song.FLAC")));
        assert!(is_supported_audio_path(Path::new("song.m4a")));
        assert!(!is_supported_audio_path(Path::new("cover.png")));
        assert!(!is_supported_audio_path(Path::new("song.exe")));
    }

    #[test]
    fn database_starts_empty_and_keeps_roots() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = env::temp_dir().join(format!("yinqidao-library-{suffix}.db"));
        let library = Library::new(path.clone()).expect("database");
        let root = env::temp_dir().join(format!("yinqidao-root-{suffix}"));
        fs::create_dir_all(&root).expect("root");
        library.add_root(&root).expect("add root");
        assert_eq!(library.roots().expect("roots"), vec![root.clone()]);
        fs::remove_dir_all(root).expect("cleanup root");
        fs::remove_file(path).expect("cleanup db");
    }
}
