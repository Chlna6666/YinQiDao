use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::plugins::{
    AccountState, AuthMethod, PluginAccount, PluginCapability, PluginManifest, PluginServiceRouter,
    ProviderDescriptor, PLUGIN_ABI_VERSION,
};

pub const PLUGIN_PACKAGE_SCHEMA_VERSION: u32 = 1;
const PLUGIN_PACKAGE_FILE: &str = "plugin.toml";
const PLUGIN_ACCOUNT_STORE_SCHEMA_VERSION: u32 = 1;
const PLUGIN_ACCOUNT_STORE_FILE: &str = "plugin-accounts.json";
const MAX_IDENTIFIER_BYTES: usize = 128;
const MAX_MANIFEST_NAME_BYTES: usize = 256;
const MAX_MANIFEST_VERSION_BYTES: usize = 128;
const MAX_MANIFEST_DESCRIPTION_BYTES: usize = 8 * 1_024;
const MAX_MANIFEST_HOMEPAGE_BYTES: usize = 2 * 1_024;
const MAX_PROVIDERS_PER_PLUGIN: usize = 64;
const MAX_PROVIDER_DISPLAY_NAME_BYTES: usize = 256;
const MAX_NETWORK_DOMAINS_PER_PLUGIN: usize = 128;
const MAX_NETWORK_DOMAIN_BYTES: usize = 255;
const MAX_NETWORK_DOMAIN_LABEL_BYTES: usize = 63;
const MAX_PERSISTED_PLUGIN_ACCOUNTS: usize = 4_096;
const MAX_ACCOUNT_ID_BYTES: usize = 512;
const MAX_ACCOUNT_TEXT_BYTES: usize = 8 * 1_024;

static PLUGIN_HOST: OnceLock<Arc<RwLock<PluginHostState>>> = OnceLock::new();

#[derive(Clone, Debug, Deserialize)]
struct PluginPackageFile {
    #[serde(default = "default_package_schema_version")]
    package_schema: u32,
    component: String,
    #[serde(flatten)]
    manifest: PluginManifest,
}

fn default_package_schema_version() -> u32 {
    PLUGIN_PACKAGE_SCHEMA_VERSION
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstalledPlugin {
    pub package_dir: PathBuf,
    pub component_path: PathBuf,
    pub manifest: PluginManifest,
}

impl InstalledPlugin {
    pub fn provider(&self, provider_id: &str) -> Option<&ProviderDescriptor> {
        self.manifest
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginLoadFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Clone, Debug, Default)]
pub struct PluginCatalog {
    root: PathBuf,
    plugins: Vec<InstalledPlugin>,
    failures: Vec<PluginLoadFailure>,
}

impl PluginCatalog {
    pub fn discover(root: PathBuf) -> Self {
        let mut catalog = Self {
            root: root.clone(),
            plugins: Vec::new(),
            failures: Vec::new(),
        };

        if let Err(error) = fs::create_dir_all(&root) {
            catalog.failures.push(PluginLoadFailure {
                path: root,
                error: format!("创建插件目录失败: {error}"),
            });
            return catalog;
        }

        let mut entries = match fs::read_dir(&root) {
            Ok(entries) => entries.filter_map(|entry| entry.ok()).collect::<Vec<_>>(),
            Err(error) => {
                catalog.failures.push(PluginLoadFailure {
                    path: root,
                    error: format!("读取插件目录失败: {error}"),
                });
                return catalog;
            }
        };
        entries.sort_by_key(|entry| entry.file_name());

        let mut plugin_ids = HashSet::new();
        for entry in entries {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }

            let package_dir = entry.path();
            let descriptor_path = package_dir.join(PLUGIN_PACKAGE_FILE);
            if !descriptor_path.is_file() {
                continue;
            }

            match load_plugin_package(&package_dir, &descriptor_path) {
                Ok(plugin) => {
                    if plugin_ids.insert(plugin.manifest.id.clone()) {
                        catalog.plugins.push(plugin);
                    } else {
                        catalog.failures.push(PluginLoadFailure {
                            path: descriptor_path,
                            error: "插件 id 与已加载插件重复".into(),
                        });
                    }
                }
                Err(error) => catalog.failures.push(PluginLoadFailure {
                    path: descriptor_path,
                    error: format!("{error:#}"),
                }),
            }
        }

        catalog
            .plugins
            .sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        catalog
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn plugins(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    pub fn failures(&self) -> &[PluginLoadFailure] {
        &self.failures
    }

    pub fn plugin(&self, plugin_id: &str) -> Option<&InstalledPlugin> {
        self.plugins
            .iter()
            .find(|plugin| plugin.manifest.id == plugin_id)
    }

    pub fn provider(&self, plugin_id: &str, provider_id: &str) -> Option<&ProviderDescriptor> {
        self.plugin(plugin_id)?.provider(provider_id)
    }
}

fn load_plugin_package(package_dir: &Path, descriptor_path: &Path) -> Result<InstalledPlugin> {
    let content = fs::read_to_string(descriptor_path)
        .with_context(|| format!("读取插件清单失败: {}", descriptor_path.display()))?;
    let package: PluginPackageFile = toml::from_str(&content)
        .with_context(|| format!("解析插件清单失败: {}", descriptor_path.display()))?;

    if package.package_schema != PLUGIN_PACKAGE_SCHEMA_VERSION {
        bail!(
            "不支持的 plugin.toml schema v{}，当前仅支持 v{}",
            package.package_schema,
            PLUGIN_PACKAGE_SCHEMA_VERSION
        );
    }
    validate_manifest(&package.manifest)?;

    let package_dir_name = package_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if package_dir_name != package.manifest.id {
        bail!(
            "插件目录名必须与 manifest id 一致: directory={package_dir_name:?}, id={:?}",
            package.manifest.id
        );
    }

    let component = Path::new(&package.component);
    if component.as_os_str().is_empty()
        || component.is_absolute()
        || component.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("component 必须是插件目录内的相对路径");
    }
    if component.extension().and_then(|ext| ext.to_str()) != Some("wasm") {
        bail!("component 必须使用 .wasm 扩展名");
    }

    let canonical_package_dir = fs::canonicalize(package_dir)
        .with_context(|| format!("规范化插件目录失败: {}", package_dir.display()))?;
    let component_path = package_dir.join(component);
    if !component_path.is_file() {
        bail!("找不到插件 component: {}", component_path.display());
    }
    let canonical_component = fs::canonicalize(&component_path)
        .with_context(|| format!("规范化 component 路径失败: {}", component_path.display()))?;
    if !canonical_component.starts_with(&canonical_package_dir) {
        bail!("component 通过软链接逃逸出插件目录，已拒绝加载");
    }

    Ok(InstalledPlugin {
        package_dir: canonical_package_dir,
        component_path: canonical_component,
        manifest: package.manifest,
    })
}

fn validate_manifest(manifest: &PluginManifest) -> Result<()> {
    if !manifest.supports_host_abi() {
        bail!(
            "插件 ABI v{} 与 Host ABI v{} 不兼容",
            manifest.abi_version,
            PLUGIN_ABI_VERSION
        );
    }
    if !valid_identifier(&manifest.id) {
        bail!("manifest id 非法: {:?}", manifest.id);
    }
    if manifest.name.trim().is_empty()
        || manifest.name.len() > MAX_MANIFEST_NAME_BYTES
        || manifest.name.contains('\0')
    {
        bail!("manifest name 为空或超过 Host 文本上限");
    }
    if manifest.version.trim().is_empty()
        || manifest.version.len() > MAX_MANIFEST_VERSION_BYTES
        || manifest.version.contains('\0')
    {
        bail!("manifest version 为空或超过 Host 文本上限");
    }
    if manifest.description.len() > MAX_MANIFEST_DESCRIPTION_BYTES
        || manifest.description.contains('\0')
    {
        bail!("manifest description 超过 Host 文本上限或包含 NUL");
    }
    if let Some(homepage) = manifest.homepage.as_deref()
        && (homepage.trim().is_empty()
            || homepage.len() > MAX_MANIFEST_HOMEPAGE_BYTES
            || homepage.contains('\0'))
    {
        bail!("manifest homepage 为空或超过 Host 文本上限");
    }
    if manifest.providers.is_empty() {
        bail!("插件必须声明至少一个 provider");
    }
    if manifest.providers.len() > MAX_PROVIDERS_PER_PLUGIN {
        bail!("插件 provider 数量超过 {MAX_PROVIDERS_PER_PLUGIN} Host 上限");
    }
    if manifest.network_domains.len() > MAX_NETWORK_DOMAINS_PER_PLUGIN {
        bail!("插件 network domain 数量超过 {MAX_NETWORK_DOMAINS_PER_PLUGIN} Host 上限");
    }

    let mut provider_ids = HashSet::new();
    for provider in &manifest.providers {
        if !valid_identifier(&provider.id) {
            bail!("provider id 非法: {:?}", provider.id);
        }
        if !provider_ids.insert(provider.id.as_str()) {
            bail!("provider id 重复: {}", provider.id);
        }
        if provider.display_name.trim().is_empty()
            || provider.display_name.len() > MAX_PROVIDER_DISPLAY_NAME_BYTES
            || provider.display_name.contains('\0')
        {
            bail!("provider {} 的 display_name 为空或超过 Host 文本上限", provider.id);
        }
        if provider.capabilities.is_empty() {
            bail!("provider {} 未声明任何 capability", provider.id);
        }

        let mut capabilities = HashSet::new();
        for capability in &provider.capabilities {
            if !capabilities.insert(*capability) {
                bail!("provider {} 重复声明 capability {capability:?}", provider.id);
            }
        }
        let mut auth_methods = HashSet::new();
        for method in &provider.auth_methods {
            let key = auth_method_key(*method);
            if !auth_methods.insert(key) {
                bail!("provider {} 重复声明 auth method {method:?}", provider.id);
            }
        }
        if !provider.auth_methods.is_empty()
            && !provider
                .capabilities
                .contains(&PluginCapability::Authentication)
        {
            bail!(
                "provider {} 声明了登录方式但缺少 authentication capability",
                provider.id
            );
        }
    }

    let mut domains = HashSet::new();
    for domain in &manifest.network_domains {
        if !valid_network_domain(domain) {
            bail!("network domain 非法: {domain:?}");
        }
        if !domains.insert(domain.to_ascii_lowercase()) {
            bail!("network domain 重复: {domain}");
        }
    }
    Ok(())
}

fn auth_method_key(method: AuthMethod) -> u8 {
    match method {
        AuthMethod::QrCode => 0,
        AuthMethod::BrowserOAuth => 1,
        AuthMethod::DeviceCode => 2,
        AuthMethod::CookieImport => 3,
        AuthMethod::CustomForm => 4,
    }
}

fn valid_identifier(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'_')
        })
}

fn valid_network_domain(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_NETWORK_DOMAIN_BYTES
        || value.contains("://")
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
        || value.chars().any(char::is_whitespace)
    {
        return false;
    }

    let host = value.strip_prefix("*.").unwrap_or(value);
    !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= MAX_NETWORK_DOMAIN_LABEL_BYTES
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[derive(Clone, Debug)]
pub struct PluginAccountStore {
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PluginAccountStoreFile {
    schema_version: u32,
    #[serde(default)]
    accounts: Vec<PluginAccount>,
}

impl PluginAccountStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Vec<PluginAccount>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)
            .with_context(|| format!("读取插件账号索引失败: {}", self.path.display()))?;
        let mut store: PluginAccountStoreFile = serde_json::from_str(&content)
            .with_context(|| format!("解析插件账号索引失败: {}", self.path.display()))?;
        if store.schema_version != PLUGIN_ACCOUNT_STORE_SCHEMA_VERSION {
            bail!(
                "不支持的插件账号索引版本 v{}，当前仅支持 v{}",
                store.schema_version,
                PLUGIN_ACCOUNT_STORE_SCHEMA_VERSION
            );
        }
        validate_accounts(&store.accounts)?;
        normalize_provider_defaults(&mut store.accounts);
        Ok(store.accounts)
    }

    pub fn save(&self, accounts: &[PluginAccount]) -> Result<()> {
        validate_accounts(accounts)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建插件账号目录失败: {}", parent.display()))?;
        }
        let payload = serde_json::to_vec_pretty(&PluginAccountStoreFile {
            schema_version: PLUGIN_ACCOUNT_STORE_SCHEMA_VERSION,
            accounts: accounts.to_vec(),
        })
        .context("序列化插件账号索引失败")?;
        fs::write(&self.path, payload)
            .with_context(|| format!("写入插件账号索引失败: {}", self.path.display()))
    }
}

fn validate_accounts(accounts: &[PluginAccount]) -> Result<()> {
    if accounts.len() > MAX_PERSISTED_PLUGIN_ACCOUNTS {
        bail!(
            "插件账号数量超过 {} Host 上限",
            MAX_PERSISTED_PLUGIN_ACCOUNTS
        );
    }

    let mut identities = HashSet::new();
    for account in accounts {
        if !valid_identifier(&account.plugin_id)
            || !valid_identifier(&account.provider_id)
            || account.account_id.trim().is_empty()
            || account.account_id.len() > MAX_ACCOUNT_ID_BYTES
            || account.account_id.contains('\0')
        {
            bail!(
                "插件账号标识非法: {}/{}/{}",
                account.plugin_id,
                account.provider_id,
                account.account_id
            );
        }
        let text_bytes = account
            .display_name
            .len()
            .saturating_add(account.avatar_url.as_ref().map_or(0, String::len));
        if text_bytes > MAX_ACCOUNT_TEXT_BYTES {
            bail!("插件账号展示信息超过 {} bytes Host 上限", MAX_ACCOUNT_TEXT_BYTES);
        }
        let mut capabilities = HashSet::with_capacity(account.capabilities.len());
        for capability in &account.capabilities {
            if !capabilities.insert(*capability) {
                bail!(
                    "插件账号重复声明 capability: {}/{}/{capability:?}",
                    account.plugin_id,
                    account.provider_id
                );
            }
        }
        let identity = (
            account.plugin_id.clone(),
            account.provider_id.clone(),
            account.account_id.clone(),
        );
        if !identities.insert(identity) {
            bail!(
                "插件账号重复: {}/{}/{}",
                account.plugin_id,
                account.provider_id,
                account.account_id
            );
        }
    }
    Ok(())
}

fn normalize_provider_defaults(accounts: &mut [PluginAccount]) {
    let mut winners = HashMap::<(String, String), usize>::new();
    for (index, account) in accounts.iter().enumerate() {
        if !account.is_default {
            continue;
        }
        let key = (account.plugin_id.clone(), account.provider_id.clone());
        match winners.get(&key).copied() {
            None => {
                winners.insert(key, index);
            }
            Some(previous) => {
                let previous_account = &accounts[previous];
                let current_wins = account.priority > previous_account.priority
                    || (account.priority == previous_account.priority
                        && account.account_id < previous_account.account_id);
                if current_wins {
                    winners.insert(key, index);
                }
            }
        }
    }

    for (index, account) in accounts.iter_mut().enumerate() {
        if account.is_default {
            let key = (account.plugin_id.clone(), account.provider_id.clone());
            account.is_default = winners.get(&key).copied() == Some(index);
        }
    }
}

#[derive(Clone, Debug)]
pub struct PluginHostState {
    catalog: PluginCatalog,
    router: PluginServiceRouter,
    account_store: PluginAccountStore,
    startup_errors: Vec<String>,
}

impl PluginHostState {
    pub fn load(base_dir: &Path) -> Self {
        let catalog = PluginCatalog::discover(base_dir.join("plugins"));
        let account_store = PluginAccountStore::new(base_dir.join(PLUGIN_ACCOUNT_STORE_FILE));
        let mut startup_errors = catalog
            .failures()
            .iter()
            .map(|failure| format!("{}: {}", failure.path.display(), failure.error))
            .collect::<Vec<_>>();

        let mut accounts = match account_store.load() {
            Ok(accounts) => accounts,
            Err(error) => {
                startup_errors.push(format!("插件账号索引加载失败: {error:#}"));
                Vec::new()
            }
        };
        accounts.retain(|account| {
            catalog
                .provider(&account.plugin_id, &account.provider_id)
                .is_some()
        });
        normalize_provider_defaults(&mut accounts);

        let mut router = PluginServiceRouter::default();
        for account in accounts {
            router.upsert_account(account);
        }

        Self {
            catalog,
            router,
            account_store,
            startup_errors,
        }
    }

    pub fn catalog(&self) -> &PluginCatalog {
        &self.catalog
    }

    pub fn router(&self) -> &PluginServiceRouter {
        &self.router
    }

    pub fn startup_errors(&self) -> &[String] {
        &self.startup_errors
    }

    pub fn upsert_account(&mut self, account: PluginAccount) -> Result<()> {
        validate_account_against_catalog(&self.catalog, &account)?;

        let mut accounts = self.router.accounts().to_vec();
        if account.is_default {
            for existing in &mut accounts {
                if existing.plugin_id == account.plugin_id
                    && existing.provider_id == account.provider_id
                {
                    existing.is_default = false;
                }
            }
        }
        if let Some(existing) = accounts.iter_mut().find(|existing| {
            existing.plugin_id == account.plugin_id
                && existing.provider_id == account.provider_id
                && existing.account_id == account.account_id
        }) {
            *existing = account;
        } else {
            accounts.push(account);
        }
        normalize_provider_defaults(&mut accounts);
        self.account_store.save(&accounts)?;
        self.replace_router_accounts(accounts);
        Ok(())
    }

    pub fn remove_account(
        &mut self,
        plugin_id: &str,
        provider_id: &str,
        account_id: &str,
    ) -> Result<bool> {
        let mut accounts = self.router.accounts().to_vec();
        let old_len = accounts.len();
        accounts.retain(|account| {
            account.plugin_id != plugin_id
                || account.provider_id != provider_id
                || account.account_id != account_id
        });
        if accounts.len() == old_len {
            return Ok(false);
        }
        self.account_store.save(&accounts)?;
        self.replace_router_accounts(accounts);
        Ok(true)
    }

    pub fn mark_account_state(
        &mut self,
        plugin_id: &str,
        provider_id: &str,
        account_id: &str,
        state: AccountState,
    ) -> Result<bool> {
        let mut accounts = self.router.accounts().to_vec();
        let Some(account) = accounts.iter_mut().find(|account| {
            account.plugin_id == plugin_id
                && account.provider_id == provider_id
                && account.account_id == account_id
        }) else {
            return Ok(false);
        };
        account.state = state;
        self.account_store.save(&accounts)?;
        self.replace_router_accounts(accounts);
        Ok(true)
    }

    fn replace_router_accounts(&mut self, accounts: Vec<PluginAccount>) {
        let mut router = PluginServiceRouter::default();
        for account in accounts {
            router.upsert_account(account);
        }
        self.router = router;
    }
}

fn validate_account_against_catalog(catalog: &PluginCatalog, account: &PluginAccount) -> Result<()> {
    let provider = catalog
        .provider(&account.plugin_id, &account.provider_id)
        .with_context(|| {
            format!(
                "账号引用了未安装的 provider: {}/{}",
                account.plugin_id, account.provider_id
            )
        })?;
    for capability in &account.capabilities {
        if !provider.capabilities.contains(capability) {
            bail!(
                "账号声明了 provider 未提供的 capability: {}/{}/{capability:?}",
                account.plugin_id,
                account.provider_id
            );
        }
    }
    Ok(())
}

pub fn initialize(base_dir: &Path) -> Arc<RwLock<PluginHostState>> {
    PLUGIN_HOST
        .get_or_init(|| Arc::new(RwLock::new(PluginHostState::load(base_dir))))
        .clone()
}

pub fn global() -> Option<Arc<RwLock<PluginHostState>>> {
    PLUGIN_HOST.get().cloned()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("yinqidao-plugin-{name}-{suffix}"))
    }

    fn write_plugin(root: &Path, id: &str, component: &str) -> PathBuf {
        let package_dir = root.join(id);
        fs::create_dir_all(&package_dir).expect("package dir");
        fs::write(package_dir.join("provider.wasm"), b"\0asm").expect("wasm");
        let manifest = format!(
            r#"package_schema = 1
component = "{component}"
id = "{id}"
name = "Test Provider"
version = "0.1.0"
abi_version = 1
network_domains = ["api.example.com"]

[[providers]]
id = "test"
display_name = "Test"
capabilities = ["authentication", "search", "lyrics"]
auth_methods = ["qr_code"]
"#
        );
        fs::write(package_dir.join("plugin.toml"), manifest).expect("manifest");
        package_dir
    }

    fn manifest() -> PluginManifest {
        PluginManifest {
            id: "plugin.test".into(),
            name: "Test Provider".into(),
            version: "0.1.0".into(),
            abi_version: PLUGIN_ABI_VERSION,
            description: "test".into(),
            homepage: Some("https://example.com".into()),
            providers: vec![ProviderDescriptor {
                id: "test".into(),
                display_name: "Test".into(),
                capabilities: vec![PluginCapability::Search],
                auth_methods: Vec::new(),
            }],
            network_domains: vec!["api.example.com".into()],
        }
    }

    fn account(account_id: &str, priority: i32, is_default: bool) -> PluginAccount {
        PluginAccount {
            plugin_id: "plugin.test".into(),
            provider_id: "test".into(),
            account_id: account_id.into(),
            display_name: account_id.into(),
            avatar_url: None,
            state: AccountState::Authenticated,
            capabilities: vec![PluginCapability::Search, PluginCapability::Lyrics],
            priority,
            is_default,
        }
    }

    #[test]
    fn catalog_discovers_valid_manifest_without_loading_component() {
        let root = temp_dir("catalog");
        write_plugin(&root, "plugin.test", "provider.wasm");

        let catalog = PluginCatalog::discover(root.clone());
        assert!(catalog.failures().is_empty());
        assert_eq!(catalog.plugins().len(), 1);
        assert_eq!(catalog.plugins()[0].manifest.id, "plugin.test");
        assert!(catalog.plugins()[0].component_path.ends_with("provider.wasm"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn catalog_rejects_component_path_escape() {
        let root = temp_dir("escape");
        write_plugin(&root, "plugin.test", "../provider.wasm");
        fs::write(root.join("provider.wasm"), b"\0asm").expect("outside wasm");

        let catalog = PluginCatalog::discover(root.clone());
        assert!(catalog.plugins().is_empty());
        assert_eq!(catalog.failures().len(), 1);
        assert!(catalog.failures()[0].error.contains("相对路径"));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn manifest_has_bounded_metadata_and_contribution_counts() {
        let mut oversized_name = manifest();
        oversized_name.name = "x".repeat(MAX_MANIFEST_NAME_BYTES + 1);
        assert!(validate_manifest(&oversized_name).is_err());

        let mut too_many_providers = manifest();
        too_many_providers.providers = (0..=MAX_PROVIDERS_PER_PLUGIN)
            .map(|index| ProviderDescriptor {
                id: format!("provider{index}"),
                display_name: format!("Provider {index}"),
                capabilities: vec![PluginCapability::Search],
                auth_methods: Vec::new(),
            })
            .collect();
        assert!(validate_manifest(&too_many_providers).is_err());

        let mut too_many_domains = manifest();
        too_many_domains.network_domains = (0..=MAX_NETWORK_DOMAINS_PER_PLUGIN)
            .map(|index| format!("api{index}.example.com"))
            .collect();
        assert!(validate_manifest(&too_many_domains).is_err());

        assert!(!valid_network_domain(&format!(
            "{}.example.com",
            "a".repeat(MAX_NETWORK_DOMAIN_LABEL_BYTES + 1)
        )));
    }

    #[test]
    fn account_store_keeps_multiple_platform_accounts_and_one_default_per_provider() {
        let root = temp_dir("accounts");
        fs::create_dir_all(&root).expect("root");
        let store = PluginAccountStore::new(root.join(PLUGIN_ACCOUNT_STORE_FILE));
        let accounts = vec![account("b", 1, true), account("a", 10, true)];
        store.save(&accounts).expect("save");

        let restored = store.load().expect("load");
        assert_eq!(restored.len(), 2);
        assert!(restored.iter().any(|account| account.account_id == "a" && account.is_default));
        assert!(restored.iter().any(|account| account.account_id == "b" && !account.is_default));

        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn account_store_rejects_oversized_identity_and_duplicate_capabilities() {
        let mut oversized = account(&"a".repeat(MAX_ACCOUNT_ID_BYTES + 1), 0, true);
        assert!(validate_accounts(std::slice::from_ref(&oversized)).is_err());

        oversized.account_id = "valid".into();
        oversized.capabilities = vec![PluginCapability::Search, PluginCapability::Search];
        assert!(validate_accounts(std::slice::from_ref(&oversized)).is_err());
    }

    #[test]
    fn identifiers_have_a_host_length_budget() {
        assert!(valid_identifier("plugin.valid"));
        assert!(!valid_identifier(&"a".repeat(MAX_IDENTIFIER_BYTES + 1)));
    }

    #[test]
    fn host_persists_fused_accounts_without_storing_secrets() {
        let root = temp_dir("host");
        let plugin_root = root.join("plugins");
        write_plugin(&plugin_root, "plugin.test", "provider.wasm");
        let mut host = PluginHostState::load(&root);

        host.upsert_account(account("primary", 10, true))
            .expect("upsert");
        host.upsert_account(account("secondary", 5, false))
            .expect("upsert");
        assert_eq!(host.router().accounts().len(), 2);

        let restored = PluginHostState::load(&root);
        assert_eq!(restored.router().accounts().len(), 2);
        assert!(root.join(PLUGIN_ACCOUNT_STORE_FILE).is_file());

        fs::remove_dir_all(root).expect("cleanup");
    }
}
