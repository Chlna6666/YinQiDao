use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::model::{
    EqSettings, RepeatMode, SmartAudioSettings, SpatialSettings, TrackId, TrackTransitionSettings,
};

/// Schema versions are reserved for breaking configuration changes only.
/// Adding fields with serde defaults is backward-compatible and keeps the existing version.
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
    pub smart_audio: SmartAudioSettings,
    #[serde(default)]
    pub transition: TrackTransitionSettings,
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
            smart_audio: SmartAudioSettings::default(),
            transition: TrackTransitionSettings::default(),
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
            smart_audio: SmartAudioSettings::default(),
            transition: TrackTransitionSettings::default(),
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
            CURRENT_CONFIG_SCHEMA_VERSION => toml::from_str(&content)
                .with_context(|| format!("解析配置失败: {}", self.path.display())),
            1 => {
                // v1 -> v2 changed the spatial configuration structure, so this remains a real
                // breaking migration with backup + rewrite.
                let legacy: AppConfigV1 = toml::from_str(&content)
                    .with_context(|| format!("解析 v1 配置失败: {}", self.path.display()))?;
                let migrated = legacy.migrate();
                self.backup_legacy_config(&content, 1)?;
                self.save(&migrated)?;
                Ok(migrated)
            }
            version => {
                tracing::error!(
                    schema_version = version,
                    supported = CURRENT_CONFIG_SCHEMA_VERSION,
                    "检测到当前程序不支持的 config.toml 版本，进入只读配置保护态"
                );
                let mut guarded = AppConfig::default();
                guarded.schema_version = version;
                Ok(guarded)
            }
        }
    }

    pub fn save(&self, config: &AppConfig) -> Result<()> {
        if config.schema_version != CURRENT_CONFIG_SCHEMA_VERSION {
            bail!(
                "拒绝写入 config.toml：运行时配置版本为 v{}，当前程序只允许写入 v{}",
                config.schema_version,
                CURRENT_CONFIG_SCHEMA_VERSION
            );
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建配置目录失败: {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(config).context("序列化配置失败")?;
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
    use crate::model::{SpatialMotionMode, TransitionMode};

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
        let mut config = AppConfig::default();
        config.volume = 0.42;
        config.queue = Arc::new(vec![3, 8, 13]);
        config.current_track = Some(8);
        config.position_ms = 12_345;
        config.eq.enabled = true;
        config.eq.preamp_db = -2.5;
        config.eq.bands_db[4] = 5.5;
        config.spatial.enabled = true;
        config.spatial.motion_mode = SpatialMotionMode::Orbit360;
        config.spatial.motion_speed_hz = 0.07;
        config.spatial.motion_intensity = 0.9;
        config.smart_audio.enabled = true;
        config.smart_audio.intensity = 0.72;
        config.transition.enabled = true;
        config.transition.mode = TransitionMode::Crossfade;
        config.transition.duration_ms = 4_200;
        store.save(&config).expect("save");

        let restored = store.load().expect("load");
        assert_eq!(restored.schema_version, 2);
        assert!(restored.smart_audio.enabled);
        assert!((restored.smart_audio.intensity - 0.72).abs() < f32::EPSILON);
        assert!(restored.transition.enabled);
        assert_eq!(restored.transition.mode, TransitionMode::Crossfade);
        assert_eq!(restored.transition.duration_ms, 4_200);
        assert_eq!(restored.eq.bands_db[4], 5.5);
        assert_eq!(restored.spatial.motion_mode, SpatialMotionMode::Orbit360);
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn additive_fields_do_not_bump_or_rewrite_v2_config() {
        let path = temp_path("v2-additive");
        let store = ConfigStore::from_path(path.clone());
        let content = r#"schema_version = 2
volume = 0.66
online_metadata = true
online_lyrics = true

[spatial]
enabled = true
width = 0.7
depth = 0.4
distance = 0.2
mix = 0.6
crossfeed = 0.08
room_size = 0.15
immersive_3d = 0.1
motion_mode = "static"
motion_speed_hz = 0.08
motion_radius = 0.65
motion_intensity = 0.0
clockwise = true
"#;
        fs::write(&path, content).expect("v2 write");

        let loaded = store.load().expect("load v2");
        assert_eq!(loaded.schema_version, 2);
        assert_eq!(loaded.spatial.width, 0.7);
        assert!(!loaded.smart_audio.enabled);
        assert!((loaded.smart_audio.intensity - 0.85).abs() < f32::EPSILON);
        assert!(!loaded.transition.enabled);
        assert_eq!(loaded.transition.mode, TransitionMode::Crossfade);
        assert_eq!(loaded.transition.duration_ms, 3_500);
        assert_eq!(fs::read_to_string(&path).expect("unchanged"), content);
        let backup = path.with_file_name(format!(
            "{}.v2.bak",
            path.file_name().and_then(|name| name.to_str()).unwrap()
        ));
        assert!(!backup.exists());
        fs::remove_file(path).expect("cleanup config");
    }

    #[test]
    fn future_config_version_is_preserved_in_read_only_mode() {
        let path = temp_path("future");
        let store = ConfigStore::from_path(path.clone());
        fs::write(&path, "schema_version = 999\n").expect("future write");
        let guarded = store.load().expect("guarded load");
        assert_eq!(guarded.schema_version, 999);
        assert!(store.save(&guarded).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "schema_version = 999\n");
        fs::remove_file(path).expect("cleanup");
    }
}
