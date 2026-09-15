use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::catalog::PluginCatalog;

const PLUGIN_STATE_SCHEMA_VERSION: u32 = 1;
const PLUGIN_STATE_FILE: &str = "plugin-state.json";
const PLUGIN_ACCOUNT_STORE_FILE: &str = "plugin-accounts.json";
const PLUGIN_PERMISSION_STORE_FILE: &str = "plugin-permissions.json";
const MAX_PLUGIN_STATE_BYTES: u64 = 1024 * 1024;
const MAX_PLUGIN_ACCOUNT_STORE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PLUGIN_PERMISSION_STORE_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PluginStateFile {
    schema_version: u32,
    #[serde(default)]
    disabled_plugins: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginStateRecovery {
    pub reason: String,
    pub disabled_plugins: usize,
}

/// Validate security-sensitive persisted indexes before their loaders allocate or parse contents.
///
/// Account and permission loaders already fail closed on parse/schema errors, but historically read
/// the complete file first. A corrupted multi-gigabyte file could therefore force an unbounded
/// allocation during application startup. This preflight rejects oversized files before any read,
/// and refuses symlink/non-regular paths instead of following them into arbitrary filesystem data.
pub fn validate_preload_state_files(base_dir: &Path) -> Result<()> {
    validate_bounded_regular_file(
        base_dir,
        PLUGIN_ACCOUNT_STORE_FILE,
        MAX_PLUGIN_ACCOUNT_STORE_BYTES,
        "插件账号索引",
    )?;
    validate_bounded_regular_file(
        base_dir,
        PLUGIN_PERMISSION_STORE_FILE,
        MAX_PLUGIN_PERMISSION_STORE_BYTES,
        "插件权限索引",
    )?;
    Ok(())
}

fn validate_bounded_regular_file(
    base_dir: &Path,
    file_name: &str,
    max_bytes: u64,
    label: &str,
) -> Result<()> {
    let path = base_dir.join(file_name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("读取{label} metadata 失败: {}", path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label}必须是普通文件且不能是符号链接: {}", path.display());
    }
    if metadata.len() > max_bytes {
        bail!(
            "{label}超过 {max_bytes} bytes Host 启动上限: {}",
            path.display()
        );
    }
    Ok(())
}

/// Validate the persisted package enable/disable state before `PluginPackageManager` reads it.
///
/// `PluginPackageManager` historically treats a load error as an empty disabled set. That is a
/// dangerous fail-open default: a truncated/corrupted state file would silently re-enable every
/// plugin. This guard runs after catalog discovery but before any executable plugin runtime/UI is
/// initialized. Invalid state is replaced with a bounded, valid state that disables every currently
/// installed plugin. The user can then explicitly re-enable trusted plugins from Settings.
///
/// A symlink or non-regular state path is never followed or overwritten. Such a filesystem layout
/// is treated as a hard startup error rather than risking an arbitrary-file write.
pub fn repair_fail_closed_state(
    base_dir: &Path,
    catalog: &PluginCatalog,
) -> Result<Option<PluginStateRecovery>> {
    let state_path = base_dir.join(PLUGIN_STATE_FILE);
    let metadata = match fs::symlink_metadata(&state_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("读取插件启停状态 metadata 失败: {}", state_path.display()));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "插件启停状态必须是普通文件且不能是符号链接: {}",
            state_path.display()
        );
    }

    let invalid_reason = if metadata.len() > MAX_PLUGIN_STATE_BYTES {
        Some(format!(
            "状态文件超过 {MAX_PLUGIN_STATE_BYTES} bytes Host 上限"
        ))
    } else {
        let bytes = fs::read(&state_path)
            .with_context(|| format!("读取插件启停状态失败: {}", state_path.display()))?;
        match std::str::from_utf8(&bytes) {
            Ok(content) => match serde_json::from_str::<PluginStateFile>(content) {
                Ok(state) if state.schema_version == PLUGIN_STATE_SCHEMA_VERSION => None,
                Ok(state) => Some(format!(
                    "不支持的插件启停状态版本 v{}，当前仅支持 v{}",
                    state.schema_version, PLUGIN_STATE_SCHEMA_VERSION
                )),
                Err(error) => Some(format!("解析插件启停状态失败: {error}")),
            },
            Err(error) => Some(format!("插件启停状态不是有效 UTF-8: {error}")),
        }
    };

    let Some(reason) = invalid_reason else {
        return Ok(None);
    };

    let mut disabled_plugins = catalog
        .plugins()
        .iter()
        .map(|plugin| plugin.manifest.id.clone())
        .collect::<Vec<_>>();
    disabled_plugins.sort();
    disabled_plugins.dedup();
    let disabled_count = disabled_plugins.len();
    let payload = serde_json::to_vec_pretty(&PluginStateFile {
        schema_version: PLUGIN_STATE_SCHEMA_VERSION,
        disabled_plugins,
    })
    .context("序列化 fail-closed 插件启停状态失败")?;

    // A direct replacement is deliberate here. If the process crashes during this recovery write,
    // the next startup runs this guard again before PackageManager/runtime initialization and again
    // converges to all-disabled. We must not rename the corrupt file away first because that would
    // create a crash window where a missing state file means "all enabled".
    fs::write(&state_path, payload)
        .with_context(|| format!("写入 fail-closed 插件启停状态失败: {}", state_path.display()))?;

    Ok(Some(PluginStateRecovery {
        reason,
        disabled_plugins: disabled_count,
    }))
}

#[cfg(test)]
mod tests {
    use std::{fs::File, time::{SystemTime, UNIX_EPOCH}};

    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("yinqidao-plugin-state-{name}-{nonce}"))
    }

    fn write_plugin(root: &Path) {
        let package = root.join("plugin.test");
        fs::create_dir_all(&package).expect("package dir");
        fs::write(package.join("provider.wasm"), b"\0asm").expect("wasm");
        fs::write(
            package.join("plugin.toml"),
            r#"package_schema = 1
component = "provider.wasm"
id = "plugin.test"
name = "Test Provider"
version = "0.1.0"
abi_version = 1
network_domains = ["api.example.com"]

[[providers]]
id = "test"
display_name = "Test"
capabilities = ["authentication", "search", "lyrics"]
auth_methods = ["qr_code"]
"#,
        )
        .expect("manifest");
    }

    #[test]
    fn valid_state_is_left_unchanged() {
        let root = temp_dir("valid");
        fs::create_dir_all(&root).expect("root");
        let path = root.join(PLUGIN_STATE_FILE);
        let content = r#"{"schema_version":1,"disabled_plugins":["plugin.test"]}"#;
        fs::write(&path, content).expect("write state");
        let catalog = PluginCatalog::discover(root.join("plugins"));

        assert!(repair_fail_closed_state(&root, &catalog).expect("guard").is_none());
        assert_eq!(fs::read_to_string(&path).expect("read"), content);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn corrupt_state_disables_every_discovered_plugin() {
        let root = temp_dir("corrupt");
        let plugin_root = root.join("plugins");
        write_plugin(&plugin_root);
        fs::write(root.join(PLUGIN_STATE_FILE), b"{broken").expect("write corrupt");

        let catalog = PluginCatalog::discover(plugin_root);
        assert_eq!(catalog.plugins().len(), 1);
        let recovery = repair_fail_closed_state(&root, &catalog)
            .expect("guard")
            .expect("recovered");
        assert_eq!(recovery.disabled_plugins, 1);
        let repaired: PluginStateFile =
            serde_json::from_str(&fs::read_to_string(root.join(PLUGIN_STATE_FILE)).expect("read"))
                .expect("parse repaired");
        assert_eq!(repaired.schema_version, PLUGIN_STATE_SCHEMA_VERSION);
        assert_eq!(repaired.disabled_plugins, vec!["plugin.test"]);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn preload_rejects_oversized_account_index_before_reading_it() {
        let root = temp_dir("oversized-account");
        fs::create_dir_all(&root).expect("root");
        let file = File::create(root.join(PLUGIN_ACCOUNT_STORE_FILE)).expect("account file");
        file.set_len(MAX_PLUGIN_ACCOUNT_STORE_BYTES + 1)
            .expect("extend account file");
        let error = validate_preload_state_files(&root).expect_err("oversized index must fail");
        assert!(error.to_string().contains("启动上限"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn preload_accepts_missing_or_bounded_regular_indexes() {
        let root = temp_dir("bounded-indexes");
        fs::create_dir_all(&root).expect("root");
        validate_preload_state_files(&root).expect("missing files");
        fs::write(root.join(PLUGIN_ACCOUNT_STORE_FILE), b"{}").expect("accounts");
        fs::write(root.join(PLUGIN_PERMISSION_STORE_FILE), b"{}").expect("permissions");
        validate_preload_state_files(&root).expect("bounded files");
        fs::remove_dir_all(root).expect("cleanup");
    }
}
