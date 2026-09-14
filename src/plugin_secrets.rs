use std::{
    collections::HashMap,
    sync::RwLock,
};

use anyhow::{Result, anyhow, bail};

use crate::plugin_security::SecretSlot;

pub const DEFAULT_MAX_SECRET_BYTES: usize = 64 * 1024;

/// Host-owned secret storage boundary used by WASM music-service plugins.
///
/// Implementations must never expose filesystem/keychain paths to guest components. The guest only
/// sees logical provider/account scopes, while the Host resolves them into a `SecretSlot`.
pub trait PluginSecretStore: Send + Sync {
    fn get(&self, slot: &SecretSlot) -> Result<Option<Vec<u8>>>;
    fn set(&self, slot: &SecretSlot, value: &[u8]) -> Result<()>;
    fn delete(&self, slot: &SecretSlot) -> Result<bool>;
    fn delete_plugin(&self, plugin_id: &str) -> Result<usize>;
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
}

impl Default for MemorySecretStore {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SECRET_BYTES)
    }
}

impl MemorySecretStore {
    pub fn new(max_secret_bytes: usize) -> Self {
        Self {
            values: RwLock::new(HashMap::new()),
            max_secret_bytes: max_secret_bytes.max(1),
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

    fn delete_plugin(&self, plugin_id: &str) -> Result<usize> {
        let mut values = self
            .values
            .write()
            .map_err(|error| anyhow!("插件 Secret 内存存储锁已损坏: {error}"))?;
        let keys = values
            .keys()
            .filter(|slot| slot.plugin_id() == plugin_id)
            .cloned()
            .collect::<Vec<_>>();
        let removed = keys.len();
        for key in keys {
            if let Some(mut value) = values.remove(&key) {
                value.fill(0);
            }
        }
        Ok(removed)
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
    fn oversized_secret_is_rejected() {
        let store = MemorySecretStore::new(4);
        let slot = SecretSlot::provider("plugin.test", "qqmusic", "token").expect("slot");
        assert!(store.set(&slot, b"12345").is_err());
    }
}
