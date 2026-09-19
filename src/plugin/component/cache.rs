use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};

use super::registry::PluginComponentSnapshot;

static PLUGIN_COMPILED_CACHE: OnceLock<Arc<PluginCompiledArtifactStore>> = OnceLock::new();

/// Host-owned storage boundary for Wasmtime serialized component artifacts.
///
/// This type does not deserialize anything. The future Wasmtime adapter may only pass bytes returned
/// by `load` to its deserialize path after it has also verified the Engine/config compatibility that
/// Wasmtime requires. Guest components never receive these paths or bytes.
#[derive(Debug)]
pub struct PluginCompiledArtifactStore {
    max_artifact_bytes: usize,
    write_lock: Mutex<()>,
}

impl PluginCompiledArtifactStore {
    pub fn new(max_artifact_bytes: usize) -> Self {
        Self {
            max_artifact_bytes: max_artifact_bytes.max(1),
            write_lock: Mutex::new(()),
        }
    }

    pub fn max_artifact_bytes(&self) -> usize {
        self.max_artifact_bytes
    }

    /// Read one cached serialized artifact under a hard size bound.
    ///
    /// Cache corruption is treated as a miss whenever possible: callers can safely fall back to
    /// compiling the immutable source snapshot instead of making plugin availability depend on a
    /// disposable optimization cache.
    pub fn load(&self, snapshot: &PluginComponentSnapshot) -> Result<Option<Vec<u8>>> {
        if !cache_dir_is_safe(snapshot)? || !cache_entry_files_are_regular(snapshot) {
            return Ok(None);
        }
        match snapshot.cached_artifact_is_reusable() {
            Ok(true) => {}
            Ok(false) => return Ok(None),
            Err(error) => {
                tracing::warn!(
                    plugin_id = %snapshot.plugin_id,
                    cache = %snapshot.compiled_cache_path.display(),
                    %error,
                    "插件 compiled cache verifier 读取失败，按 cache miss 处理"
                );
                return Ok(None);
            }
        }

        let file = match File::open(&snapshot.compiled_cache_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                tracing::warn!(
                    plugin_id = %snapshot.plugin_id,
                    cache = %snapshot.compiled_cache_path.display(),
                    %error,
                    "打开插件 compiled cache 失败，按 cache miss 处理"
                );
                return Ok(None);
            }
        };
        let metadata = file.metadata().with_context(|| {
            format!(
                "读取 compiled artifact metadata 失败: {}",
                snapshot.compiled_cache_path.display()
            )
        })?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > self.max_artifact_bytes as u64
        {
            return Ok(None);
        }

        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take((self.max_artifact_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .with_context(|| {
                format!(
                    "读取 compiled artifact 失败: {}",
                    snapshot.compiled_cache_path.display()
                )
            })?;
        if bytes.is_empty() || bytes.len() > self.max_artifact_bytes {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    /// Atomically replace the serialized artifact, then publish the exact source verifier.
    ///
    /// The old verifier is removed before replacing the artifact. Therefore a process crash at any
    /// intermediate point leaves either the old complete entry or an entry that is ineligible for
    /// reuse. The verifier is always committed last.
    pub fn store(&self, snapshot: &PluginComponentSnapshot, artifact: &[u8]) -> Result<()> {
        if artifact.is_empty() {
            bail!("插件 compiled artifact 不能为空");
        }
        if artifact.len() > self.max_artifact_bytes {
            bail!(
                "插件 compiled artifact 超过 {} bytes 限制",
                self.max_artifact_bytes
            );
        }

        let _guard = self
            .write_lock
            .lock()
            .map_err(|error| anyhow!("插件 compiled cache 写锁已损坏: {error}"))?;
        ensure_safe_cache_dir(snapshot)?;

        // Revoke trust in the previous entry before replacing any compiled bytes.
        remove_file_if_present(&snapshot.source_verifier_path)?;

        let temp_path = unique_temp_path(&snapshot.cache_dir, "component.cwasm");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .with_context(|| {
                format!(
                    "创建 compiled artifact 临时文件失败: {}",
                    temp_path.display()
                )
            })?;
        if let Err(error) = file.write_all(artifact).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&temp_path);
            return Err(error).context("写入 compiled artifact 失败");
        }
        drop(file);

        if let Err(error) = remove_file_if_present(&snapshot.compiled_cache_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        if let Err(error) = fs::rename(&temp_path, &snapshot.compiled_cache_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error).with_context(|| {
                format!(
                    "提交 compiled artifact 失败: {}",
                    snapshot.compiled_cache_path.display()
                )
            });
        }

        if let Err(error) = snapshot.persist_source_verifier() {
            // Never leave a compiled artifact trusted without its final verifier commit.
            let _ = fs::remove_file(&snapshot.compiled_cache_path);
            return Err(error).context("提交 compiled artifact source verifier 失败");
        }
        Ok(())
    }

    pub fn invalidate(&self, snapshot: &PluginComponentSnapshot) -> Result<bool> {
        let _guard = self
            .write_lock
            .lock()
            .map_err(|error| anyhow!("插件 compiled cache 写锁已损坏: {error}"))?;
        let verifier_removed = remove_file_if_present(&snapshot.source_verifier_path)?;
        let artifact_removed = remove_file_if_present(&snapshot.compiled_cache_path)?;
        if snapshot.cache_dir.is_dir() {
            match fs::remove_dir(&snapshot.cache_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "清理 compiled cache 目录失败: {}",
                            snapshot.cache_dir.display()
                        )
                    });
                }
            }
        }
        Ok(verifier_removed || artifact_removed)
    }
}

fn ensure_safe_cache_dir(snapshot: &PluginComponentSnapshot) -> Result<()> {
    let root = snapshot
        .cache_dir
        .parent()
        .ok_or_else(|| anyhow!("插件 compiled cache 缺少 root"))?;
    fs::create_dir_all(root)
        .with_context(|| format!("创建 compiled cache root 失败: {}", root.display()))?;

    if let Ok(metadata) = fs::symlink_metadata(&snapshot.cache_dir)
        && metadata.file_type().is_symlink()
    {
        bail!("插件 compiled cache 目录不能是符号链接");
    }
    fs::create_dir_all(&snapshot.cache_dir).with_context(|| {
        format!(
            "创建 compiled cache 目录失败: {}",
            snapshot.cache_dir.display()
        )
    })?;

    let canonical_root = fs::canonicalize(root)
        .with_context(|| format!("规范化 compiled cache root 失败: {}", root.display()))?;
    let canonical_dir = fs::canonicalize(&snapshot.cache_dir).with_context(|| {
        format!(
            "规范化 compiled cache 目录失败: {}",
            snapshot.cache_dir.display()
        )
    })?;
    if !canonical_dir.starts_with(&canonical_root) {
        bail!("插件 compiled cache 目录逃逸出 cache root");
    }
    Ok(())
}

fn cache_dir_is_safe(snapshot: &PluginComponentSnapshot) -> Result<bool> {
    if !snapshot.cache_dir.exists() {
        return Ok(false);
    }
    let metadata = match fs::symlink_metadata(&snapshot.cache_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("读取 compiled cache 目录 metadata 失败"),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }

    let Some(root) = snapshot.cache_dir.parent() else {
        return Ok(false);
    };
    let canonical_root = match fs::canonicalize(root) {
        Ok(root) => root,
        Err(_) => return Ok(false),
    };
    let canonical_dir = match fs::canonicalize(&snapshot.cache_dir) {
        Ok(dir) => dir,
        Err(_) => return Ok(false),
    };
    Ok(canonical_dir.starts_with(canonical_root))
}

fn cache_entry_files_are_regular(snapshot: &PluginComponentSnapshot) -> bool {
    [
        &snapshot.compiled_cache_path,
        &snapshot.source_verifier_path,
    ]
    .into_iter()
    .all(|path| {
        fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false)
    })
}

fn remove_file_if_present(path: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取 cache entry metadata 失败: {}", path.display()));
        }
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        bail!("cache entry 预期为文件但实际为目录: {}", path.display());
    }
    fs::remove_file(path)
        .with_context(|| format!("删除旧 cache entry 失败: {}", path.display()))?;
    Ok(true)
}

fn unique_temp_path(cache_dir: &Path, label: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    cache_dir.join(format!(".{label}.tmp-{}-{nonce}", std::process::id()))
}

pub fn initialize(max_artifact_bytes: usize) -> Arc<PluginCompiledArtifactStore> {
    PLUGIN_COMPILED_CACHE
        .get_or_init(|| Arc::new(PluginCompiledArtifactStore::new(max_artifact_bytes)))
        .clone()
}

pub fn global() -> Option<Arc<PluginCompiledArtifactStore>> {
    PLUGIN_COMPILED_CACHE.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{
        abi::{PLUGIN_ABI_VERSION, PluginManifest},
        component::registry::{PluginComponentLimits, PluginComponentRegistry},
        host::catalog::InstalledPlugin,
    };

    fn unique_temp_root(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("yinqidao-{name}-{}-{nonce}", std::process::id()))
    }

    fn snapshot(root: &Path) -> Arc<PluginComponentSnapshot> {
        let package_dir = root.join("plugin.test");
        fs::create_dir_all(&package_dir).expect("package dir");
        let component_path = package_dir.join("provider.wasm");
        fs::write(&component_path, b"\0asm\x0d\x00\x01\x00").expect("component");
        let plugin = InstalledPlugin {
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
        };
        PluginComponentRegistry::new(root, PluginComponentLimits::default())
            .snapshot(&plugin)
            .expect("snapshot")
    }

    #[test]
    fn store_then_load_round_trips_bounded_artifact() {
        let root = unique_temp_root("compiled-roundtrip");
        let snapshot = snapshot(&root);
        let store = PluginCompiledArtifactStore::new(1024);
        assert_eq!(store.load(&snapshot).expect("initial load"), None);
        store
            .store(&snapshot, b"serialized-wasmtime-component")
            .expect("store");
        assert_eq!(
            store.load(&snapshot).expect("load"),
            Some(b"serialized-wasmtime-component".to_vec())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_artifact_is_rejected() {
        let root = unique_temp_root("compiled-limit");
        let snapshot = snapshot(&root);
        let store = PluginCompiledArtifactStore::new(4);
        assert!(store.store(&snapshot, b"12345").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verifier_mismatch_becomes_cache_miss() {
        let root = unique_temp_root("compiled-verifier");
        let snapshot = snapshot(&root);
        let store = PluginCompiledArtifactStore::new(1024);
        store.store(&snapshot, b"compiled").expect("store");
        fs::write(&snapshot.source_verifier_path, b"wrong-source").expect("tamper verifier");
        assert_eq!(store.load(&snapshot).expect("load"), None);
        let _ = fs::remove_dir_all(root);
    }
}
