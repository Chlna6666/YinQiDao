use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::model::{EqSettings, RepeatMode, SpatialSettings, TrackId};

pub const CURRENT_CONFIG_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
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

#[derive(Clone, Debug, Deserialize)]
struct AppConfigV1 {
    #[serde(default)]
    music_dirs: Vec<PathBuf>,
    #[serde(default = "default_volume")]
    volume: f32,
    #[serde(default)]
    output_device: Option<String>,
    #[serde(default)]
    eq: EqSettings,
    #[serde(default)]
    spatial: SpatialSettingsV1,
    #[serde(default)]
    repeat: RepeatMode,
    #[serde(default)]
    shuffle: bool,
    #[serde(default)]
    queue: Arc<Vec<TrackId>>,
    #[serde(default)]
    current_track: Option<TrackId>,
    #[serde(default)]
    position_ms: u64,
    #[serde(default = "default_dynamic_blur")]
    dynamic_blur: bool,
    #[serde(default = "default_blur_radius")]
    blur_radius: f32,
    #[serde(default = "default_enabled")]
    online_metadata: bool,
    #[serde(default = "default_enabled")]
    online_lyrics: bool,
    #[serde(default)]
    acoustid_api_key: Option<String>,
    #[serde(default)]
    log: LogConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct SpatialSettingsV1 {
    enabled: bool,
    width: f32,
    depth: f32,
    distance: f32,
    mix: f32,
}

impl Default for SpatialSettingsV1 {
    fn default() -> Self {
        Self {
            enabled: false,
            width: 0.5,
            depth: 0.35,
            distance: 0.2,
            mix: 0.5,
        }
    }
}

impl AppConfigV1 {
    fn migrate(self) -> AppConfig {
        let spatial_defaults = SpatialSettings::default();
        AppConfig {
            schema_version: CURRENT_CONFIG_SCHEMA_VERSION,
            music_dirs: self.music_dirs,
            volume: self.volume,
            output_device: self.output_device,
            eq: self.eq,
            spatial: SpatialSettings {
                enabled: self.spatial.enabled,
                width: self.spatial.width,
                depth: self.spatial.depth,
                distance: self.spatial.distance,
                mix: self.spatial.mix,
                ..spatial_defaults
            },
            repeat: self.repeat,
            shuffle: self.shuffle,
            queue: self.queue,
            current_track: self.current_track,
            position_ms: self.position_ms,
            dynamic_blur: self.dynamic_blur,
            blur_radius: self.blur_radius,
            online_metadata: self.online_metadata,
            online_lyrics: self.online_lyrics,
            acoustid_api_key: self.acoustid_api_key,
            log: self.log,
        }
    }
}

fn current_schema_version() -> u32 {
    CURRENT_CONFIG_SCHEMA_VERSION
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
            schema_version: CURRENT_CONFIG_SCHEMA_VERSION,
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
        let value: toml::Value = toml::from_str(&content)
            .with_context(|| format!("解析配置版本失败: {}", self.path.display()))?;
        let schema_version = value
            .get("schema_version")
            .and_then(toml::Value::as_integer)
            .map(|version| u32::try_from(version).unwrap_or(u32::MAX))
            .unwrap_or(1);

        match schema_version {
            CURRENT_CONFIG_SCHEMA_VERSION => {
                let config: AppConfig = toml::from_str(&content)
                    .with_context(|| format!("解析配置失败: {}", self.path.display()))?;
                Ok(config)
            }
            1 => {
                let legacy: AppConfigV1 = toml::from_str(&content)
                    .with_context(|| format!("解析 v1 配置失败: {}", self.path.display()))?;
                let migrated = legacy.migrate();
                self.backup_legacy_config(&content, 1)?;
                self.save(&migrated)?;
                Ok(migrated)
            }
            version if version > CURRENT_CONFIG_SCHEMA_VERSION => bail!(
                "配置版本 v{version} 高于当前程序支持的 v{}，请升级音栖岛后再打开",
                CURRENT_CONFIG_SCHEMA_VERSION
            ),
            version => bail!(
                "不支持的配置版本 v{version}；当前支持迁移 v1 -> v{}",
                CURRENT_CONFIG_SCHEMA_VERSION
            ),
        }
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
        }
        let mut persisted = config.clone();
        persisted.schema_version = CURRENT_CONFIG_SCHEMA_VERSION;
        let content = toml::to_string_pretty(&persisted).context("序列化配置失败")?;
        fs::write(&self.path, content)
            .with_context(|| format!("写入配置失败: {}", self.path.display()))
    }

    fn backup_legacy_config(&self, content: &str, version: u32) -> Result<()> {
        let file_name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config.toml");
        let backup_name = format!("{file_name}.v{version}.bak");
        let backup_path = self.path.with_file_name(backup_name);
        if !backup_path.exists() {
            fs::write(&backup_path, content)
                .with_context(|| format!("备份旧配置失败: {}", backup_path.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{env, fs, time::SystemTime};

    use super::*;
    use crate::model::SpatialMotionMode;

    fn temp_path(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        env::temp_dir().join(format!("yinqidao-{name}-{suffix}.toml"))
    }

    #[test]
    fn config_round_trip_restores_professional_audio_preferences() {
        let path = temp_path("config");
        let store = ConfigStore::from_path(path.clone());
        let eq = EqSettings {
            enabled: true,
            preamp_db: -2.5,
            bands_db: [0.0, 0.0, 0.0, 0.0, 5.5, 0.0, 0.0, 0.0, 0.0, 0.0],
        };
        let mut spatial = SpatialSettings::default();
        spatial.enabled = true;
        spatial.motion_mode = SpatialMotionMode::Orbit360;
        spatial.motion_speed_hz = 0.07;
        spatial.motion_intensity = 0.9;
        let config = AppConfig {
            volume: 0.42,
            queue: Arc::new(vec![3, 8, 13]),
            current_track: Some(8),
            position_ms: 12_345,
            eq,
            spatial,
            ..AppConfig::default()
        };
        store.save(&config).expect("save");

        let restored = store.load().expect("load");
        assert_eq!(restored.schema_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert!((restored.volume - 0.42).abs() < f32::EPSILON);
        assert_eq!(restored.queue.as_slice(), &[3, 8, 13]);
        assert_eq!(restored.current_track, Some(8));
        assert_eq!(restored.position_ms, 12_345);
        assert_eq!(restored.eq.bands_db[4], 5.5);
        assert_eq!(restored.eq.preamp_db, -2.5);
        assert_eq!(restored.spatial.motion_mode, SpatialMotionMode::Orbit360);
        assert!((restored.spatial.motion_intensity - 0.9).abs() < f32::EPSILON);
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn unversioned_v1_config_is_backed_up_migrated_and_rewritten() {
        let path = temp_path("legacy");
        let store = ConfigStore::from_path(path.clone());
        fs::write(
            &path,
            r#"volume = 0.66
online_metadata = true
online_lyrics = true

[spatial]
enabled = true
width = 0.7
depth = 0.4
distance = 0.2
mix = 0.6
"#,
        )
        .expect("legacy write");

        let migrated = store.load().expect("migrate");
        assert_eq!(migrated.schema_version, CURRENT_CONFIG_SCHEMA_VERSION);
        assert_eq!(migrated.spatial.motion_mode, SpatialMotionMode::Static);
        assert_eq!(migrated.spatial.width, 0.7);
        let rewritten = fs::read_to_string(&path).expect("rewritten");
        assert!(rewritten.contains("schema_version = 2"));
        let backup = path.with_file_name(format!(
            "{}.v1.bak",
            path.file_name().and_then(|name| name.to_str()).unwrap()
        ));
        assert!(backup.exists());
        fs::remove_file(path).expect("cleanup config");
        fs::remove_file(backup).expect("cleanup backup");
    }

    #[test]
    fn future_config_version_is_rejected() {
        let path = temp_path("future");
        let store = ConfigStore::from_path(path.clone());
        fs::write(&path, "schema_version = 999\n").expect("future write");
        let error = store.load().expect_err("future version must fail");
        assert!(error.to_string().contains("高于当前程序支持"));
        fs::remove_file(path).expect("cleanup");
    }
}
