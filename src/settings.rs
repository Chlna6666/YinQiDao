use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::model::{EqSettings, RepeatMode, SpatialSettings, TrackId};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default)]
    pub music_dirs: Vec<PathBuf>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub output_device: Option<String>,
    #[serde(default)]
    pub eq: EqSettings,
    #[serde(default)]
    pub spatial: SpatialSettings,
    #[serde(default)]
    pub repeat: RepeatMode,
    #[serde(default)]
    pub shuffle: bool,
    #[serde(default)]
    pub queue: Arc<Vec<TrackId>>,
    #[serde(default)]
    pub current_track: Option<TrackId>,
    #[serde(default)]
    pub position_ms: u64,
    #[serde(default = "default_dynamic_blur")]
    pub dynamic_blur: bool,
    #[serde(default = "default_blur_radius")]
    pub blur_radius: f32,
    #[serde(default = "default_enabled")]
    pub online_metadata: bool,
    #[serde(default = "default_enabled")]
    pub online_lyrics: bool,
    #[serde(default)]
    pub acoustid_api_key: Option<String>,
    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_enabled")]
    pub file_logging: bool,
    #[serde(default = "default_max_archives")]
    pub max_archives: usize,
}

fn default_log_level() -> String {
    "info".into()
}

fn default_max_archives() -> usize {
    7
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file_logging: true,
            max_archives: default_max_archives(),
        }
    }
}

fn default_volume() -> f32 {
    0.85
}

fn default_dynamic_blur() -> bool {
    true
}

fn default_blur_radius() -> f32 {
    16.0
}

fn default_enabled() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            music_dirs: Vec::new(),
            volume: default_volume(),
            output_device: None,
            eq: EqSettings::default(),
            spatial: SpatialSettings::default(),
            repeat: RepeatMode::Off,
            shuffle: false,
            queue: Arc::new(Vec::new()),
            current_track: None,
            position_ms: 0,
            dynamic_blur: default_dynamic_blur(),
            blur_radius: default_blur_radius(),
            online_metadata: default_enabled(),
            online_lyrics: default_enabled(),
            acoustid_api_key: None,
            log: LogConfig::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn discover() -> Result<Self> {
        let application_name = if cfg!(target_os = "linux") {
            "yinqidao"
        } else {
            "YinQiDao"
        };
        let dirs = ProjectDirs::from("", "", application_name).context("无法定位音栖岛配置目录")?;
        Ok(Self::from_path(dirs.config_dir().join("config.toml")))
    }

    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn load(&self) -> Result<AppConfig> {
        if !self.path.exists() {
            return Ok(AppConfig::default());
        }
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("读取配置失败: {}", self.path.display()))?;
        toml::from_str(&content).with_context(|| format!("解析配置失败: {}", self.path.display()))
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(config).context("序列化配置失败")?;
        fs::write(&self.path, content)
            .with_context(|| format!("写入配置失败: {}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, time::SystemTime};

    use super::*;

    #[test]
    fn config_round_trip_restores_playback_preferences() {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = env::temp_dir().join(format!("yinqidao-config-{suffix}.toml"));
        let store = ConfigStore::from_path(path.clone());
        let eq = EqSettings {
            enabled: true,
            bands_db: [0.0, 0.0, 0.0, 0.0, 5.5, 0.0, 0.0, 0.0, 0.0, 0.0],
            ..EqSettings::default()
        };
        let config = AppConfig {
            volume: 0.42,
            queue: Arc::new(vec![3, 8, 13]),
            current_track: Some(8),
            position_ms: 12_345,
            eq,
            ..AppConfig::default()
        };
        store.save(&config).expect("save");

        let restored = store.load().expect("load");
        assert!((restored.volume - 0.42).abs() < f32::EPSILON);
        assert_eq!(restored.queue.as_slice(), &[3, 8, 13]);
        assert_eq!(restored.current_track, Some(8));
        assert_eq!(restored.position_ms, 12_345);
        assert_eq!(restored.eq.bands_db[4], 5.5);
        fs::remove_file(path).expect("cleanup");
    }
}
