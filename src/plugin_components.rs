use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use md5::{Digest, Md5};

use crate::{
    plugin_host::InstalledPlugin,
    plugins::PLUGIN_ABI_VERSION,
};

pub const SELECTED_WASMTIME_VERSION: &str = "48.0.1";
const COMPONENT_CACHE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_MAX_COMPONENT_BYTES: usize = 64 * 1024 * 1024;
const WASM_MAGIC: [u8; 4] = [0x00, 0x61, 0x73, 0x6d];
const COMPILED_ARTIFACT_FILE: &str = "component.cwasm";
const SOURCE_VERIFIER_FILE: &str = "source.wasm";

static PLUGIN_COMPONENTS: OnceLock<Arc<PluginComponentRegistry>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PluginComponentLimits {
    pub max_component_bytes: usize,
}

impl Default for PluginComponentLimits {
    fn default() -> Self {
        Self {
            max_component_bytes: DEFAULT_MAX_COMPONENT_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceStamp {
    len: u64,
    modified_ns: Option<u128>,
}

#[derive(Clone, Debug)]
struct CachedSnapshot {
    stamp: SourceStamp,
    snapshot: Arc<PluginComponentSnapshot>,
}

/// Immutable bytes and cache location for one installed component.
///
/// The MD5-derived locator below is deliberately *not* a trust boundary. A future serialized
/// Wasmtime artifact may be reused only when `source_verifier_path` exists and its bytes are exactly
/// equal to this snapshot. That byte-for-byte verifier prevents an attacker-controlled MD5 collision
/// from making a different component eligible for deserialization.
#[derive(Clone, Debug)]
pub struct PluginComponentSnapshot {
    pub plugin_id: String,
    pub plugin_version: String,
    pub source_path: PathBuf,
    pub bytes: Arc<[u8]>,
    /// Diagnostic/cache-bucketing digest only. Never use this value as proof of component identity.
    pub locator_digest_hex: String,
    pub compiled_cache_key: String,
    pub cache_dir: PathBuf,
    pub source_verifier_path: PathBuf,
    pub compiled_cache_path: PathBuf,
}

impl PluginComponentSnapshot {
    /// A serialized Wasmtime artifact is reusable only if both files exist and the sidecar source is
    /// byte-for-byte identical to the already validated immutable snapshot.
    pub fn cached_artifact_is_reusable(&self) -> Result<bool> {
        if !self.compiled_cache_path.is_file() || !self.source_verifier_path.is_file() {
            return Ok(false);
        }
        file_contents_equal(&self.source_verifier_path, self.bytes.as_ref())
    }

    /// Persist the exact source bytes next to a successfully written compiled artifact.
    ///
    /// The future Wasmtime cache writer must write/flush the `.cwasm` first, then call this method.
    /// A crash before the verifier rename simply makes that artifact ineligible for reuse.
    pub fn persist_source_verifier(&self) -> Result<()> {
        fs::create_dir_all(&self.cache_dir).with_context(|| {
            format!("创建插件 compiled cache 目录失败: {}", self.cache_dir.display())
        })?;

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp_path = self.cache_dir.join(format!(
            ".{SOURCE_VERIFIER_FILE}.tmp-{}-{nonce}",
            std::process::id()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| format!("创建插件 source verifier 临时文件失败: {}", temp_path.display()))?;
        if let Err(error) = file.write_all(self.bytes.as_ref()).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&temp_path);
            return Err(error).context("写入插件 source verifier 失败");
        }
        drop(file);

        if self.source_verifier_path.exists() {
            fs::remove_file(&self.source_verifier_path).with_context(|| {
                format!(
                    "替换插件 source verifier 前删除旧文件失败: {}",
                    self.source_verifier_path.display()
                )
            })?;
        }
        if let Err(error) = fs::rename(&temp_path, &self.source_verifier_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error).with_context(|| {
                format!(
                    "提交插件 source verifier 失败: {}",
                    self.source_verifier_path.display()
                )
            });
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct PluginComponentRegistry {
    cache_root: PathBuf,
    limits: PluginComponentLimits,
    snapshots: Mutex<HashMap<String, CachedSnapshot>>,
}

impl PluginComponentRegistry {
    pub fn new(base_dir: &Path, limits: PluginComponentLimits) -> Self {
        Self {
            cache_root: base_dir.join("plugin-cache").join("wasmtime"),
            limits: PluginComponentLimits {
                max_component_bytes: limits.max_component_bytes.max(1),
            },
            snapshots: Mutex::new(HashMap::new()),
        }
    }

    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    pub fn selected_runtime_version(&self) -> &'static str {
        SELECTED_WASMTIME_VERSION
    }

    /// Lazily load one component. Startup catalog scanning remains metadata-only; bytes are read
    /// only when a plugin is actually needed by a service route.
    pub fn snapshot(&self, plugin: &InstalledPlugin) -> Result<Arc<PluginComponentSnapshot>> {
        let stamp = source_stamp(&plugin.component_path, self.limits.max_component_bytes)?;
        {
            let snapshots = self
                .snapshots
                .lock()
                .map_err(|error| anyhow!("插件 Component snapshot 锁已损坏: {error}"))?;
            if let Some(cached) = snapshots.get(&plugin.manifest.id)
                && cached.stamp == stamp
            {
                return Ok(cached.snapshot.clone());
            }
        }

        let snapshot = Arc::new(load_snapshot(
            plugin,
            &self.cache_root,
            self.limits.max_component_bytes,
        )?);
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|error| anyhow!("插件 Component snapshot 锁已损坏: {error}"))?;
        snapshots.insert(
            plugin.manifest.id.clone(),
            CachedSnapshot {
                stamp,
                snapshot: snapshot.clone(),
            },
        );
        Ok(snapshot)
    }

    pub fn invalidate(&self, plugin_id: &str) -> Result<bool> {
        Ok(self
            .snapshots
            .lock()
            .map_err(|error| anyhow!("插件 Component snapshot 锁已损坏: {error}"))?
            .remove(plugin_id)
            .is_some())
    }

    pub fn clear(&self) -> Result<usize> {
        let mut snapshots = self
            .snapshots
            .lock()
            .map_err(|error| anyhow!("插件 Component snapshot 锁已损坏: {error}"))?;
        let count = snapshots.len();
        snapshots.clear();
        Ok(count)
    }
}

fn source_stamp(path: &Path, max_component_bytes: usize) -> Result<SourceStamp> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("读取插件 Component metadata 失败: {}", path.display()))?;
    if !metadata.is_file() {
        bail!("插件 Component 不是普通文件: {}", path.display());
    }
    if metadata.len() > max_component_bytes as u64 {
        bail!(
            "插件 Component 超过 {} bytes 限制: {}",
            max_component_bytes,
            path.display()
        );
    }
    Ok(SourceStamp {
        len: metadata.len(),
        modified_ns: modified_ns(metadata.modified().ok()),
    })
}

fn modified_ns(value: Option<SystemTime>) -> Option<u128> {
    value?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_nanos())
}

fn load_snapshot(
    plugin: &InstalledPlugin,
    cache_root: &Path,
    max_component_bytes: usize,
) -> Result<PluginComponentSnapshot> {
    // Revalidate both canonical paths at lazy-load time. The catalog may have been created much
    // earlier than the first plugin call, and installers/updaters can replace files in between.
    let package_dir = fs::canonicalize(&plugin.package_dir).with_context(|| {
        format!(
            "重新规范化插件目录失败: {}",
            plugin.package_dir.display()
        )
    })?;
    let component_path = fs::canonicalize(&plugin.component_path).with_context(|| {
        format!(
            "重新规范化插件 Component 失败: {}",
            plugin.component_path.display()
        )
    })?;
    if !component_path.starts_with(&package_dir) {
        bail!("插件 Component 在懒加载前逃逸出插件目录，已拒绝");
    }

    let file = File::open(&component_path)
        .with_context(|| format!("打开插件 Component 失败: {}", component_path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("读取已打开 Component metadata 失败: {}", component_path.display()))?;
    if !metadata.is_file() {
        bail!("插件 Component 不是普通文件: {}", component_path.display());
    }
    if metadata.len() > max_component_bytes as u64 {
        bail!(
            "插件 Component 超过 {} bytes 限制: {}",
            max_component_bytes,
            component_path.display()
        );
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((max_component_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("读取插件 Component 失败: {}", component_path.display()))?;
    if bytes.len() > max_component_bytes {
        bail!("插件 Component 在读取过程中超过大小限制");
    }
    if bytes.len() < WASM_MAGIC.len() || bytes[..WASM_MAGIC.len()] != WASM_MAGIC {
        bail!("插件 Component 缺少 WebAssembly magic header");
    }

    let locator_digest_hex = digest_hex(&bytes);
    let compiled_cache_key = compiled_cache_key(&plugin.manifest.id, &bytes);
    let cache_dir = cache_root.join(&compiled_cache_key);
    let source_verifier_path = cache_dir.join(SOURCE_VERIFIER_FILE);
    let compiled_cache_path = cache_dir.join(COMPILED_ARTIFACT_FILE);

    Ok(PluginComponentSnapshot {
        plugin_id: plugin.manifest.id.clone(),
        plugin_version: plugin.manifest.version.clone(),
        source_path: component_path,
        bytes: Arc::<[u8]>::from(bytes),
        locator_digest_hex,
        compiled_cache_key,
        cache_dir,
        source_verifier_path,
        compiled_cache_path,
    })
}

fn digest_hex(bytes: &[u8]) -> String {
    let digest = Md5::digest(bytes);
    to_hex(&digest)
}

/// Produce a compact directory locator. This intentionally is not a cryptographic identity check;
/// `PluginComponentSnapshot::cached_artifact_is_reusable` performs the authoritative byte equality
/// check before any future `.cwasm` deserialization.
fn compiled_cache_key(plugin_id: &str, bytes: &[u8]) -> String {
    let mut hasher = Md5::new();
    hasher.update(b"yinqidao-plugin-component-cache\0");
    hasher.update(COMPONENT_CACHE_SCHEMA_VERSION.to_le_bytes());
    hasher.update(PLUGIN_ABI_VERSION.to_le_bytes());
    hasher.update(SELECTED_WASMTIME_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update([0]);
    hasher.update(std::env::consts::ARCH.as_bytes());
    hasher.update([0]);
    hasher.update(plugin_id.as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    format!("component-v{COMPONENT_CACHE_SCHEMA_VERSION}-{}", to_hex(&hasher.finalize()))
}

fn file_contents_equal(path: &Path, expected: &[u8]) -> Result<bool> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("读取插件 cache verifier metadata 失败: {}", path.display()))?;
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return Ok(false);
    }

    let mut file = File::open(path)
        .with_context(|| format!("打开插件 cache verifier 失败: {}", path.display()))?;
    let mut buffer = [0u8; 16 * 1024];
    let mut offset = 0usize;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("读取插件 cache verifier 失败: {}", path.display()))?;
        if read == 0 {
            return Ok(offset == expected.len());
        }
        let end = offset.saturating_add(read);
        if end > expected.len() || expected[offset..end] != buffer[..read] {
            return Ok(false);
        }
        offset = end;
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub fn initialize(base_dir: &Path) -> Arc<PluginComponentRegistry> {
    PLUGIN_COMPONENTS
        .get_or_init(|| {
            Arc::new(PluginComponentRegistry::new(
                base_dir,
                PluginComponentLimits::default(),
            ))
        })
        .clone()
}

pub fn global() -> Option<Arc<PluginComponentRegistry>> {
    PLUGIN_COMPONENTS.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{PluginManifest, PLUGIN_ABI_VERSION};

    fn unique_temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("yinqidao-{name}-{}-{nonce}", std::process::id()))
    }

    fn installed_plugin(root: &Path, bytes: &[u8]) -> InstalledPlugin {
        let package_dir = root.join("plugin.test");
        fs::create_dir_all(&package_dir).expect("package dir");
        let component_path = package_dir.join("provider.wasm");
        fs::write(&component_path, bytes).expect("component");
        InstalledPlugin {
            package_dir: fs::canonicalize(package_dir).expect("canonical package"),
            component_path: fs::canonicalize(component_path).expect("canonical component"),
            manifest: PluginManifest {
                id: "plugin.test".into(),
                name: "Test".into(),
                version: "0.1.0".into(),
                abi_version: PLUGIN_ABI_VERSION,
                description: String::new(),
                homepage: None,
                providers: Vec::new(),
                network_domains: Vec::new(),
            },
        }
    }

    #[test]
    fn cache_key_changes_with_component_bytes() {
        let first = compiled_cache_key("plugin.test", b"\0asm-first");
        let second = compiled_cache_key("plugin.test", b"\0asm-second");
        assert_ne!(first, second);
    }

    #[test]
    fn lazy_snapshot_rejects_non_wasm_payload() {
        let root = unique_temp_root("component-invalid");
        let plugin = installed_plugin(&root, b"not-wasm");
        let registry = PluginComponentRegistry::new(&root, PluginComponentLimits::default());
        assert!(registry.snapshot(&plugin).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lazy_snapshot_builds_future_compiled_cache_paths() {
        let root = unique_temp_root("component-cache");
        let plugin = installed_plugin(&root, b"\0asm\x0d\x00\x01\x00");
        let registry = PluginComponentRegistry::new(&root, PluginComponentLimits::default());
        let snapshot = registry.snapshot(&plugin).expect("snapshot");
        assert_eq!(snapshot.plugin_id, "plugin.test");
        assert!(snapshot.compiled_cache_key.starts_with("component-v1-"));
        assert_eq!(snapshot.cache_dir.parent(), Some(registry.cache_root()));
        assert_eq!(snapshot.compiled_cache_path.parent(), Some(snapshot.cache_dir.as_path()));
        assert_eq!(snapshot.source_verifier_path.parent(), Some(snapshot.cache_dir.as_path()));
        assert_eq!(snapshot.bytes.as_ref(), b"\0asm\x0d\x00\x01\x00");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compiled_artifact_requires_exact_source_verifier() {
        let root = unique_temp_root("component-verifier");
        let plugin = installed_plugin(&root, b"\0asm\x0d\x00\x01\x00");
        let registry = PluginComponentRegistry::new(&root, PluginComponentLimits::default());
        let snapshot = registry.snapshot(&plugin).expect("snapshot");
        fs::create_dir_all(&snapshot.cache_dir).expect("cache dir");
        fs::write(&snapshot.compiled_cache_path, b"compiled").expect("compiled artifact");

        fs::write(&snapshot.source_verifier_path, b"different").expect("wrong verifier");
        assert!(!snapshot.cached_artifact_is_reusable().expect("wrong verifier check"));

        snapshot.persist_source_verifier().expect("persist verifier");
        assert!(snapshot.cached_artifact_is_reusable().expect("exact verifier check"));
        let _ = fs::remove_dir_all(root);
    }
}
