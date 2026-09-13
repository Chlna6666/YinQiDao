use std::net::IpAddr;

use anyhow::{Result, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::plugins::PluginManifest;

const SECRET_KEY_SCHEMA_VERSION: &str = "v1";

/// User-approved permissions are deliberately separate from the capabilities requested by a
/// plugin manifest. A component only receives a network route when both sets allow the target.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginPermissionGrant {
    pub plugin_id: String,
    #[serde(default)]
    pub network_domains: Vec<String>,
    #[serde(default)]
    pub playback_events: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedHttpTarget {
    pub plugin_id: String,
    pub host: String,
    pub url: Url,
}

/// Validate a plugin HTTP target before any Host-owned request is created.
///
/// This is only the first SSRF boundary. The eventual HTTP executor must run the same check for
/// every redirect and must reject loopback/private/link-local addresses after DNS resolution.
pub fn authorize_http_target(
    manifest: &PluginManifest,
    grant: &PluginPermissionGrant,
    url: &Url,
) -> Result<AuthorizedHttpTarget> {
    if grant.plugin_id != manifest.id {
        bail!("网络授权不属于当前插件");
    }
    if url.scheme() != "https" {
        bail!("插件网络请求首版仅允许 HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("插件网络请求禁止 URL userinfo");
    }
    if url.port().is_some_and(|port| port != 443) {
        bail!("插件网络请求禁止非 443 端口");
    }

    let host = url
        .host_str()
        .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty())
        .ok_or_else(|| anyhow::anyhow!("插件网络请求缺少 host"))?;

    if host.parse::<IpAddr>().is_ok() {
        bail!("插件网络请求禁止直接访问 IP literal");
    }
    if obvious_local_hostname(&host) {
        bail!("插件网络请求禁止访问本机/局域命名域");
    }

    let requested = manifest
        .network_domains
        .iter()
        .any(|pattern| domain_pattern_matches(pattern, &host));
    if !requested {
        bail!("目标域名未在插件 manifest 中声明: {host}");
    }

    let granted = grant
        .network_domains
        .iter()
        .any(|pattern| domain_pattern_matches(pattern, &host));
    if !granted {
        bail!("目标域名尚未获得用户授权: {host}");
    }

    Ok(AuthorizedHttpTarget {
        plugin_id: manifest.id.clone(),
        host,
        url: url.clone(),
    })
}

pub fn authorize_redirect(
    manifest: &PluginManifest,
    grant: &PluginPermissionGrant,
    redirect_target: &Url,
) -> Result<AuthorizedHttpTarget> {
    authorize_http_target(manifest, grant, redirect_target)
}

fn domain_pattern_matches(pattern: &str, host: &str) -> bool {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Some(base) = pattern.strip_prefix("*.") {
        return host != base
            && host.len() > base.len()
            && host.ends_with(base)
            && host.as_bytes().get(host.len() - base.len() - 1) == Some(&b'.');
    }
    pattern == host
}

fn obvious_local_hostname(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "local"
        || host.ends_with(".local")
        || host == "home.arpa"
        || host.ends_with(".home.arpa")
}

/// Structured secret location owned by the Host. Guest components receive logical slots and never
/// a filesystem/keychain path. The serialized storage key is length-prefixed, so provider/account
/// identifiers cannot escape their namespace with separators.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SecretSlot {
    plugin_id: String,
    provider_id: String,
    account_id: String,
    key: String,
}

impl SecretSlot {
    pub fn new(
        plugin_id: impl Into<String>,
        provider_id: impl Into<String>,
        account_id: impl Into<String>,
        key: impl Into<String>,
    ) -> Result<Self> {
        let slot = Self {
            plugin_id: plugin_id.into(),
            provider_id: provider_id.into(),
            account_id: account_id.into(),
            key: key.into(),
        };
        slot.validate()?;
        Ok(slot)
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn storage_key(&self) -> String {
        format!(
            "{SECRET_KEY_SCHEMA_VERSION}:{}:{}:{}:{}:{}:{}:{}:{}",
            self.plugin_id.len(),
            self.plugin_id,
            self.provider_id.len(),
            self.provider_id,
            self.account_id.len(),
            self.account_id,
            self.key.len(),
            self.key
        )
    }

    fn validate(&self) -> Result<()> {
        if !valid_namespace_identifier(&self.plugin_id)
            || !valid_namespace_identifier(&self.provider_id)
        {
            bail!("Secret namespace 的 plugin/provider id 非法");
        }
        if self.account_id.trim().is_empty()
            || self.account_id.len() > 512
            || self.account_id.contains('\0')
        {
            bail!("Secret namespace 的 account id 非法");
        }
        if self.key.is_empty()
            || self.key.len() > 128
            || !self.key.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            bail!("Secret key 非法");
        }
        Ok(())
    }
}

fn valid_namespace_identifier(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::PluginManifest;

    fn manifest(domains: &[&str]) -> PluginManifest {
        PluginManifest {
            id: "plugin.test".into(),
            name: "Test".into(),
            version: "0.1.0".into(),
            abi_version: crate::plugins::PLUGIN_ABI_VERSION,
            description: String::new(),
            homepage: None,
            providers: Vec::new(),
            network_domains: domains.iter().map(|domain| (*domain).into()).collect(),
        }
    }

    fn grant(domains: &[&str]) -> PluginPermissionGrant {
        PluginPermissionGrant {
            plugin_id: "plugin.test".into(),
            network_domains: domains.iter().map(|domain| (*domain).into()).collect(),
            playback_events: false,
        }
    }

    #[test]
    fn http_target_requires_manifest_and_user_grant() {
        let manifest = manifest(&["*.example.com"]);
        let grant = grant(&["api.example.com"]);
        let target = Url::parse("https://api.example.com/v1/search").expect("url");

        let authorized = authorize_http_target(&manifest, &grant, &target).expect("authorized");
        assert_eq!(authorized.host, "api.example.com");

        let other = Url::parse("https://cdn.example.com/cover").expect("url");
        assert!(authorize_http_target(&manifest, &grant, &other).is_err());
    }

    #[test]
    fn wildcard_never_grants_bare_parent_domain() {
        let manifest = manifest(&["*.example.com"]);
        let grant = grant(&["*.example.com"]);
        let target = Url::parse("https://example.com/").expect("url");
        assert!(authorize_http_target(&manifest, &grant, &target).is_err());
    }

    #[test]
    fn http_target_rejects_insecure_local_and_ip_destinations() {
        let manifest = manifest(&["localhost", "127.0.0.1", "api.example.com"]);
        let grant = grant(&["localhost", "127.0.0.1", "api.example.com"]);

        assert!(
            authorize_http_target(
                &manifest,
                &grant,
                &Url::parse("http://api.example.com/").expect("url")
            )
            .is_err()
        );
        assert!(
            authorize_http_target(
                &manifest,
                &grant,
                &Url::parse("https://localhost/").expect("url")
            )
            .is_err()
        );
        assert!(
            authorize_http_target(
                &manifest,
                &grant,
                &Url::parse("https://127.0.0.1/").expect("url")
            )
            .is_err()
        );
    }

    #[test]
    fn redirect_is_checked_against_the_same_permissions() {
        let manifest = manifest(&["api.example.com"]);
        let grant = grant(&["api.example.com"]);
        let redirect = Url::parse("https://evil.example.net/token").expect("url");
        assert!(authorize_redirect(&manifest, &grant, &redirect).is_err());
    }

    #[test]
    fn secret_storage_keys_are_namespace_safe() {
        let first = SecretSlot::new("plugin.test", "netease", "a/b", "refresh_token")
            .expect("slot");
        let second = SecretSlot::new("plugin.test", "netease", "a", "refresh_token")
            .expect("slot");
        assert_ne!(first.storage_key(), second.storage_key());
        assert!(first.storage_key().starts_with("v1:"));
    }
}
