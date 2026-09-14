use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    plugin_host::PluginCatalog,
    plugin_security::PluginPermissionGrant,
    plugins::{PluginCapability, PluginManifest},
};

const PLUGIN_PERMISSION_STORE_SCHEMA_VERSION: u32 = 1;
const PLUGIN_PERMISSION_STORE_FILE: &str = "plugin-permissions.json";

static PLUGIN_PERMISSIONS: OnceLock<Arc<RwLock<PluginPermissionState>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct PluginPermissionStore {
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PluginPermissionStoreFile {
    schema_version: u32,
    #[serde(default)]
    grants: Vec<PluginPermissionGrant>,
}

impl PluginPermissionStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn load(&self) -> Result<Vec<PluginPermissionGrant>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("读取插件权限索引失败: {}", self.path.display()))?;
        let store: PluginPermissionStoreFile = serde_json::from_str(&content)
            .with_context(|| format!("解析插件权限索引失败: {}", self.path.display()))?;
        if store.schema_version != PLUGIN_PERMISSION_STORE_SCHEMA_VERSION {
            bail!(
                "不支持的插件权限索引版本 v{}，当前仅支持 v{}",
                store.schema_version,
                PLUGIN_PERMISSION_STORE_SCHEMA_VERSION
            );
        }
        Ok(store.grants)
    }

    fn save(&self, grants: &[PluginPermissionGrant]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建插件权限目录失败: {}", parent.display()))?;
        }
        let payload = serde_json::to_vec_pretty(&PluginPermissionStoreFile {
            schema_version: PLUGIN_PERMISSION_STORE_SCHEMA_VERSION,
            grants: grants.to_vec(),
        })
        .context("序列化插件权限索引失败")?;
        fs::write(&self.path, payload)
            .with_context(|| format!("写入插件权限索引失败: {}", self.path.display()))
    }
}

#[derive(Clone, Debug)]
pub struct PluginPermissionState {
    store: PluginPermissionStore,
    grants: Vec<PluginPermissionGrant>,
    startup_errors: Vec<String>,
}

impl PluginPermissionState {
    pub fn load(base_dir: &Path, catalog: &PluginCatalog) -> Self {
        let store = PluginPermissionStore::new(base_dir.join(PLUGIN_PERMISSION_STORE_FILE));
        let raw_grants = match store.load() {
            Ok(grants) => grants,
            Err(error) => {
                return Self {
                    store,
                    grants: Vec::new(),
                    startup_errors: vec![format!("插件权限索引加载失败: {error:#}")],
                };
            }
        };

        let mut grants = Vec::with_capacity(raw_grants.len());
        let mut startup_errors = Vec::new();
        let mut seen_plugins = HashSet::new();
        for raw_grant in raw_grants {
            let plugin_id = raw_grant.plugin_id.clone();
            let Some(plugin) = catalog.plugin(&plugin_id) else {
                startup_errors.push(format!(
                    "忽略未安装插件的权限记录: {}",
                    raw_grant.plugin_id
                ));
                continue;
            };
            if !seen_plugins.insert(plugin_id.clone()) {
                startup_errors.push(format!("插件权限记录重复，忽略后续项: {plugin_id}"));
                continue;
            }
            match normalize_and_validate_grant(&plugin.manifest, raw_grant) {
                Ok(grant) => grants.push(grant),
                Err(error) => startup_errors.push(format!(
                    "忽略非法插件权限记录 {plugin_id}: {error:#}"
                )),
            }
        }
        grants.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));

        Self {
            store,
            grants,
            startup_errors,
        }
    }

    pub fn grants(&self) -> &[PluginPermissionGrant] {
        &self.grants
    }

    pub fn grant_for(&self, plugin_id: &str) -> Option<&PluginPermissionGrant> {
        self.grants.iter().find(|grant| grant.plugin_id == plugin_id)
    }

    pub fn startup_errors(&self) -> &[String] {
        &self.startup_errors
    }

    pub fn set_grant(
        &mut self,
        catalog: &PluginCatalog,
        grant: PluginPermissionGrant,
    ) -> Result<()> {
        let plugin = catalog
            .plugin(&grant.plugin_id)
            .with_context(|| format!("不能授权未安装插件: {}", grant.plugin_id))?;
        let grant = normalize_and_validate_grant(&plugin.manifest, grant)?;

        let mut next = self.grants.clone();
        if let Some(existing) = next
            .iter_mut()
            .find(|existing| existing.plugin_id == grant.plugin_id)
        {
            *existing = grant;
        } else {
            next.push(grant);
        }
        next.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
        self.store.save(&next)?;
        self.grants = next;
        Ok(())
    }

    pub fn revoke(&mut self, plugin_id: &str) -> Result<bool> {
        let mut next = self.grants.clone();
        let previous_len = next.len();
        next.retain(|grant| grant.plugin_id != plugin_id);
        if next.len() == previous_len {
            return Ok(false);
        }
        self.store.save(&next)?;
        self.grants = next;
        Ok(true)
    }
}

fn normalize_and_validate_grant(
    manifest: &PluginManifest,
    mut grant: PluginPermissionGrant,
) -> Result<PluginPermissionGrant> {
    if grant.plugin_id != manifest.id {
        bail!("权限记录 plugin_id 与插件 manifest 不一致");
    }

    let mut domains = Vec::with_capacity(grant.network_domains.len());
    let mut seen_domains = HashSet::new();
    for raw_domain in grant.network_domains.drain(..) {
        let domain = normalize_domain_pattern(&raw_domain)?;
        if !manifest
            .network_domains
            .iter()
            .any(|requested| pattern_is_within(&domain, requested))
        {
            bail!("用户授权域名超出插件 manifest 声明范围: {domain}");
        }
        if seen_domains.insert(domain.clone()) {
            domains.push(domain);
        }
    }
    domains.sort();
    grant.network_domains = domains;

    if grant.playback_events
        && !manifest.providers.iter().any(|provider| {
            provider
                .capabilities
                .contains(&PluginCapability::PlaybackEvents)
        })
    {
        bail!("插件未声明 playback_events capability，不能授予播放行为权限");
    }

    Ok(grant)
}

fn normalize_domain_pattern(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if value.is_empty()
        || value.contains("://")
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
        || value.chars().any(char::is_whitespace)
    {
        bail!("非法网络权限域名: {value:?}");
    }
    let host = value.strip_prefix("*.").unwrap_or(&value);
    if host.is_empty()
        || host.starts_with('.')
        || host.ends_with('.')
        || !host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        bail!("非法网络权限域名: {value:?}");
    }
    Ok(value)
}

/// Return true only when every host represented by `granted` is also represented by `requested`.
/// This allows a user to narrow `*.example.com` to `api.example.com` or `*.api.example.com`, while
/// preventing a plugin update/UI bug from silently expanding an exact manifest request.
fn pattern_is_within(granted: &str, requested: &str) -> bool {
    let granted = granted.trim().trim_end_matches('.').to_ascii_lowercase();
    let requested = requested.trim().trim_end_matches('.').to_ascii_lowercase();

    match (
        granted.strip_prefix("*."),
        requested.strip_prefix("*."),
    ) {
        (None, None) => granted == requested,
        (Some(_), None) => false,
        (None, Some(requested_base)) => strict_subdomain_of(&granted, requested_base),
        (Some(granted_base), Some(requested_base)) => {
            granted_base == requested_base
                || strict_subdomain_of(granted_base, requested_base)
        }
    }
}

fn strict_subdomain_of(host: &str, base: &str) -> bool {
    host != base
        && host.len() > base.len()
        && host.ends_with(base)
        && host.as_bytes().get(host.len() - base.len() - 1) == Some(&b'.')
}

pub fn initialize(
    base_dir: &Path,
    catalog: &PluginCatalog,
) -> Arc<RwLock<PluginPermissionState>> {
    PLUGIN_PERMISSIONS
        .get_or_init(|| Arc::new(RwLock::new(PluginPermissionState::load(base_dir, catalog))))
        .clone()
}

pub fn global() -> Option<Arc<RwLock<PluginPermissionState>>> {
    PLUGIN_PERMISSIONS.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{PluginCapability, ProviderDescriptor, PLUGIN_ABI_VERSION};

    fn manifest(domains: &[&str], playback_events: bool) -> PluginManifest {
        PluginManifest {
            id: "plugin.test".into(),
            name: "Test".into(),
            version: "0.1.0".into(),
            abi_version: PLUGIN_ABI_VERSION,
            description: String::new(),
            homepage: None,
            providers: vec![ProviderDescriptor {
                id: "test".into(),
                display_name: "Test".into(),
                capabilities: if playback_events {
                    vec![PluginCapability::PlaybackEvents]
                } else {
                    vec![PluginCapability::Search]
                },
                auth_methods: Vec::new(),
            }],
            network_domains: domains.iter().map(|domain| (*domain).into()).collect(),
        }
    }

    #[test]
    fn wildcard_manifest_can_be_narrowed_but_not_expanded() {
        assert!(pattern_is_within("api.example.com", "*.example.com"));
        assert!(pattern_is_within("*.api.example.com", "*.example.com"));
        assert!(pattern_is_within("*.example.com", "*.example.com"));
        assert!(!pattern_is_within("example.com", "*.example.com"));
        assert!(!pattern_is_within("*.example.com", "api.example.com"));
        assert!(!pattern_is_within("*.com", "*.example.com"));
    }

    #[test]
    fn grant_is_normalized_and_deduplicated() {
        let grant = PluginPermissionGrant {
            plugin_id: "plugin.test".into(),
            network_domains: vec!["API.EXAMPLE.COM".into(), "api.example.com".into()],
            playback_events: false,
        };
        let normalized = normalize_and_validate_grant(
            &manifest(&["*.example.com"], false),
            grant,
        )
        .expect("grant");
        assert_eq!(normalized.network_domains, ["api.example.com"]);
    }

    #[test]
    fn playback_event_permission_requires_capability() {
        let grant = PluginPermissionGrant {
            plugin_id: "plugin.test".into(),
            network_domains: Vec::new(),
            playback_events: true,
        };
        assert!(normalize_and_validate_grant(&manifest(&[], false), grant).is_err());
    }
}
