use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow, bail};

use crate::plugin_security::SecretSlot;

pub const DEFAULT_MAX_SECRET_BYTES: usize = 64 * 1024;
pub const DEFAULT_MAX_SECRET_ENTRIES: usize = 4_096;

/// Security properties of one Host-owned Secret backend.
///
/// This is deliberately explicit so account/session code can distinguish a development-only
/// process-local store from a backend that can safely restore refresh tokens across launches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretStoreProtection {
    /// Values exist only in the current process and disappear on shutdown.
    Ephemeral,
    /// Values are persisted by an operating-system credential/keychain facility.
    OsProtected,
    /// Values are persisted by a Host-owned authenticated-encryption store with external key
    /// protection. No such implementation exists yet; never report this for plaintext storage.
    HostEncrypted,
}

impl SecretStoreProtection {
    pub const fn is_persistent(self) -> bool {
        matches!(self, Self::OsProtected | Self::HostEncrypted)
    }
}

/// Host-owned secret storage boundary used by WASM music-service plugins.
///
/// Implementations must never expose filesystem/keychain paths to guest components. The guest only
/// sees logical provider/account scopes, while the Host resolves them into a `SecretSlot`.
pub trait PluginSecretStore: Send + Sync {
    fn get(&self, slot: &SecretSlot) -> Result<Option<Vec<u8>>>;
    fn set(&self, slot: &SecretSlot, value: &[u8]) -> Result<()>;
    fn delete(&self, slot: &SecretSlot) -> Result<bool>;
    /// Delete every Secret in one exact account namespace while preserving provider-scope secrets
    /// and sibling accounts. Host logout owns this operation; it is never exposed as a guest import.
    fn delete_account(&self, plugin_id: &str, provider_id: &str, account_id: &str) -> Result<usize>;
    /// Delete provider-scope Secrets plus every account namespace belonging to one exact Provider,
    /// while preserving sibling Providers owned by the same plugin.
    fn delete_provider(&self, plugin_id: &str, provider_id: &str) -> Result<usize>;
    fn delete_plugin(&self, plugin_id: &str) -> Result<usize>;

    /// Human-readable backend identifier for diagnostics. It must not contain paths, account ids or
    /// any other secret-bearing information.
    fn backend_name(&self) -> &'static str;

    /// Persistence/protection level used by session restoration policy.
    fn protection(&self) -> SecretStoreProtection;
}

static PLUGIN_SECRET_STORE: OnceLock<Arc<dyn PluginSecretStore>> = OnceLock::new();

/// Publish the process-wide Host Secret backend used by runtime imports and account lifecycle code.
/// The guest never receives this handle. Keeping one shared Arc prevents logout/session cleanup from
/// accidentally targeting a different backend instance than `PluginHostServices`.
pub fn initialize(store: Arc<dyn PluginSecretStore>) -> Arc<dyn PluginSecretStore> {
    PLUGIN_SECRET_STORE.get_or_init(|| store).clone()
}

pub fn global() -> Option<Arc<dyn PluginSecretStore>> {
    PLUGIN_SECRET_STORE.get().cloned()
}

/// In-memory backend for tests and early Host wiring.
///
/// This backend is intentionally non-persistent. Production login sessions must move to an OS
/// credential store or an authenticated encrypted Host store before real provider plugins rely on
/// cross-process refresh tokens.
#[derive(Debug)]
pub struct MemorySecretStore {
    values: RwLock<HashMap<SecretSlot, Vec<u8>>>,
    max_secret_bytes: usize,
    max_entries: usize,
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SECRET_BYTES)
    }
}

impl MemorySecretStore {
    pub fn new(max_secret_bytes: usize) -> Self {
        Self::with_limits(max_secret_bytes, DEFAULT_MAX_SECRET_ENTRIES)
    }

    pub fn with_limits(max_secret_bytes: usize, max_entries: usize) -> Self {
        Self {
            values: RwLock::new(HashMap::new()),
            max_secret_bytes: max_secret_bytes.max(1),
            max_entries: max_entries.max(1),
        }
    }

    pub fn len(&self) -> Result<usize> {
        Ok(self
            .values
            .read()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?
            .len())
    }

    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    fn wipe_all(values: &mut HashMap<SecretSlot, Vec<u8>>) {
        for value in values.values_mut() {
            value.fill(0);
        }
        values.clear();
    }

    fn remove_matching(
        values: &mut HashMap<SecretSlot, Vec<u8>>,
        predicate: impl Fn(&SecretSlot) -> bool,
    ) -> usize {
        let mut removed = 0usize;
        values.retain(|slot, value| {
            if predicate(slot) {
                value.fill(0);
                removed = removed.saturating_add(1);
                false
            } else {
                true
            }
        });
        removed
    }
}

impl PluginSecretStore for MemorySecretStore {
    fn get(&self, slot: &SecretSlot) -> Result<Option<Vec<u8>>> {
        Ok(self
            .values
            .read()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?
            .get(slot)
            .cloned())
    }

    fn set(&self, slot: &SecretSlot, value: &[u8]) -> Result<()> {
        if value.is_empty() {
            bail!("插件 Secret 不能为空");
        }
        if value.len() > self.max_secret_bytes {
            bail!(
                "插件 Secret 超过 {} bytes 限制",
                self.max_secret_bytes
            );
        }
        let mut values = self
            .values
            .write()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?;
        if !values.contains_key(slot) && values.len() >= self.max_entries {
            bail!("插件 Secret 条目数达到 {} 上限", self.max_entries);
        }
        if let Some(mut previous) = values.insert(slot.clone(), value.to_vec()) {
            previous.fill(0);
        }
        Ok(())
    }

    fn delete(&self, slot: &SecretSlot) -> Result<bool> {
        let mut values = self
            .values
            .write()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?;
        let Some(mut value) = values.remove(slot) else {
            return Ok(false);
        };
        value.fill(0);
        Ok(true)
    }

    fn delete_account(&self, plugin_id: &str, provider_id: &str, account_id: &str) -> Result<usize> {
        let mut values = self
            .values
            .write()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?;
        Ok(Self::remove_matching(&mut values, |slot| {
            slot.plugin_id() == plugin_id
                && slot.provider_id() == provider_id
                && slot.account_id() == Some(account_id)
        }))
    }

    fn delete_provider(&self, plugin_id: &str, provider_id: &str) -> Result<usize> {
        let mut values = self
            .values
            .write()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?;
        Ok(Self::remove_matching(&mut values, |slot| {
            slot.plugin_id() == plugin_id && slot.provider_id() == provider_id
        }))
    }

    fn delete_plugin(&self, plugin_id: &str) -> Result<usize> {
        let mut values = self
            .values
            .write()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?;
        Ok(Self::remove_matching(&mut values, |slot| {
            slot.plugin_id() == plugin_id
        }))
    }

    fn backend_name(&self) -> &'static str {
        "memory-ephemeral"
    }

    fn protection(&self) -> SecretStoreProtection {
        SecretStoreProtection::Ephemeral
    }
}

impl Drop for MemorySecretStore {
    fn drop(&mut self) {
        match self.values.get_mut() {
            Ok(values) => Self::wipe_all(values),
            Err(poisoned) => Self::wipe_all(poisoned.into_inner()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_scopes_do_not_collide() {
        let store = MemorySecretStore::default();
        let first = SecretSlot::account("plugin.test", "qqmusic", "10001", "refresh_token")
            .expect("first slot");
        let second = SecretSlot::account("plugin.test", "qqmusic", "10002", "refresh_token")
            .expect("second slot");

        store.set(&first, b"first").expect("set first");
        store.set(&second, b"second").expect("set second");
        assert_eq!(store.get(&first).expect("get first"), Some(b"first".to_vec()));
        assert_eq!(store.get(&second).expect("get second"), Some(b"second".to_vec()));
    }

    #[test]
    fn provider_and_account_scopes_do_not_collide() {
        let store = MemorySecretStore::default();
        let provider = SecretSlot::provider("plugin.test", "netease", "device_secret")
            .expect("provider slot");
        let account = SecretSlot::account(
            "plugin.test",
            "netease",
            "device_secret",
            "device_secret",
        )
        .expect("account slot");
        store.set(&provider, b"provider").expect("set provider");
        store.set(&account, b"account").expect("set account");
        assert_ne!(provider.storage_key(), account.storage_key());
        assert_eq!(
            store.get(&provider).expect("get provider"),
            Some(b"provider".to_vec())
        );
        assert_eq!(
            store.get(&account).expect("get account"),
            Some(b"account".to_vec())
        );
    }

    #[test]
    fn account_delete_wipes_only_exact_account_namespace() {
        let store = MemorySecretStore::default();
        let provider = SecretSlot::provider("plugin.test", "qqmusic", "device_secret")
            .expect("provider slot");
        let first = SecretSlot::account("plugin.test", "qqmusic", "10001", "refresh_token")
            .expect("first account");
        let first_cookie = SecretSlot::account("plugin.test", "qqmusic", "10001", "cookie")
            .expect("first cookie");
        let second = SecretSlot::account("plugin.test", "qqmusic", "10002", "refresh_token")
            .expect("second account");
        store.set(&provider, b"provider").expect("set provider");
        store.set(&first, b"first").expect("set first");
        store.set(&first_cookie, b"cookie").expect("set first cookie");
        store.set(&second, b"second").expect("set second");

        assert_eq!(
            store
                .delete_account("plugin.test", "qqmusic", "10001")
                .expect("delete account"),
            2
        );
        assert!(store.get(&first).expect("get first").is_none());
        assert!(store.get(&first_cookie).expect("get first cookie").is_none());
        assert!(store.get(&provider).expect("get provider").is_some());
        assert!(store.get(&second).expect("get second").is_some());
    }

    #[test]
    fn provider_delete_wipes_provider_scope_and_accounts_only() {
        let store = MemorySecretStore::default();
        let first_provider = SecretSlot::provider("plugin.test", "qqmusic", "device_secret")
            .expect("first provider");
        let first_account = SecretSlot::account(
            "plugin.test",
            "qqmusic",
            "10001",
            "refresh_token",
        )
        .expect("first account");
        let sibling_provider = SecretSlot::provider("plugin.test", "netease", "device_secret")
            .expect("sibling provider");
        let sibling_account = SecretSlot::account(
            "plugin.test",
            "netease",
            "20001",
            "refresh_token",
        )
        .expect("sibling account");
        store.set(&first_provider, b"provider").expect("set provider");
        store.set(&first_account, b"account").expect("set account");
        store
            .set(&sibling_provider, b"sibling-provider")
            .expect("set sibling provider");
        store
            .set(&sibling_account, b"sibling-account")
            .expect("set sibling account");

        assert_eq!(
            store
                .delete_provider("plugin.test", "qqmusic")
                .expect("delete provider"),
            2
        );
        assert!(store.get(&first_provider).expect("get provider").is_none());
        assert!(store.get(&first_account).expect("get account").is_none());
        assert!(
            store
                .get(&sibling_provider)
                .expect("get sibling provider")
                .is_some()
        );
        assert!(
            store
                .get(&sibling_account)
                .expect("get sibling account")
                .is_some()
        );
    }

    #[test]
    fn empty_and_oversized_secrets_are_rejected() {
        let store = MemorySecretStore::new(4);
        let slot = SecretSlot::provider("plugin.test", "qqmusic", "token").expect("slot");
        assert!(store.set(&slot, b"").is_err());
        assert!(store.set(&slot, b"12345").is_err());
        assert!(store.get(&slot).expect("get").is_none());
    }

    #[test]
    fn secret_entry_limit_allows_overwrite_but_rejects_new_slot() {
        let store = MemorySecretStore::with_limits(64, 1);
        let first = SecretSlot::provider("plugin.test", "qqmusic", "token").expect("first slot");
        let second = SecretSlot::provider("plugin.test", "qqmusic", "cookie").expect("second slot");

        store.set(&first, b"one").expect("set first");
        store.set(&first, b"two").expect("overwrite first");
        assert!(store.set(&second, b"three").is_err());
        assert_eq!(store.len().expect("len"), 1);
        assert_eq!(store.get(&first).expect("get first"), Some(b"two".to_vec()));
    }

    #[test]
    fn memory_backend_is_explicitly_ephemeral() {
        let store = MemorySecretStore::default();
        assert_eq!(store.backend_name(), "memory-ephemeral");
        assert_eq!(store.protection(), SecretStoreProtection::Ephemeral);
        assert!(!store.protection().is_persistent());
    }
}
