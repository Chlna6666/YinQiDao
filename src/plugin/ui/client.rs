use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow};

use super::schema::UiPageModel;

pub type PluginUiFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiFieldValue {
    Text(String),
    Bool(bool),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginUiEvent {
    Action {
        action_id: String,
        fields: BTreeMap<String, UiFieldValue>,
    },
    FieldChanged {
        field_id: String,
        value: UiFieldValue,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginUiResponse {
    /// Replacement page model. `None` means the current Host snapshot remains valid.
    pub page: Option<UiPageModel>,
    /// Optional short status text for the Host UI surface.
    pub toast: Option<String>,
    /// Request that the Host leave the plugin route after applying the response.
    pub close: bool,
}

/// Runtime-neutral semantic boundary for Component UI exports.
///
/// The Wasmtime adapter implements this trait. GPUI must never invoke it from `render`/paint;
/// ordinary async controller code loads a page or dispatches an event, validates the returned model,
/// and publishes an immutable snapshot into `PluginUiPageCache`.
pub trait PluginUiClient: Send + Sync {
    fn load_page<'a>(
        &'a self,
        plugin_id: &'a str,
        page_id: &'a str,
    ) -> PluginUiFuture<'a, UiPageModel>;

    fn handle_event<'a>(
        &'a self,
        plugin_id: &'a str,
        page_id: &'a str,
        event: PluginUiEvent,
    ) -> PluginUiFuture<'a, PluginUiResponse>;
}

/// Process-wide hot-swappable UI client slot. A runtime reload swaps the `Arc`; existing async
/// operations retain their old client until completion and page-cache generation checks prevent an
/// obsolete result from being published after plugin update/uninstall. During the coordinated
/// Provider/UI swap window, reads fail closed so no caller observes mismatched adapters.
#[derive(Default)]
pub struct PluginUiClientRegistry {
    client: RwLock<Option<Arc<dyn PluginUiClient>>>,
}

impl std::fmt::Debug for PluginUiClientRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginUiClientRegistry")
            .field("ready", &self.is_ready().unwrap_or(false))
            .finish()
    }
}

impl PluginUiClientRegistry {
    pub fn client(&self) -> Result<Option<Arc<dyn PluginUiClient>>> {
        if crate::plugin::runtime_ports::is_swapping() {
            return Err(anyhow!("插件 Component runtime 正在切换，UI 调用暂不可用"));
        }
        Ok(self
            .client
            .read()
            .map_err(|error| anyhow!("插件 UI client registry 锁已损坏: {error}"))?
            .clone())
    }

    pub fn is_ready(&self) -> Result<bool> {
        if crate::plugin::runtime_ports::is_swapping() {
            return Ok(false);
        }
        Ok(self
            .client
            .read()
            .map_err(|error| anyhow!("插件 UI client registry 锁已损坏: {error}"))?
            .is_some())
    }

    pub(in crate::plugin) fn install(
        &self,
        client: Arc<dyn PluginUiClient>,
    ) -> Result<Option<Arc<dyn PluginUiClient>>> {
        Ok(self
            .client
            .write()
            .map_err(|error| anyhow!("插件 UI client registry 锁已损坏: {error}"))?
            .replace(client))
    }

    pub(in crate::plugin) fn clear(&self) -> Result<Option<Arc<dyn PluginUiClient>>> {
        Ok(self
            .client
            .write()
            .map_err(|error| anyhow!("插件 UI client registry 锁已损坏: {error}"))?
            .take())
    }
}

static PLUGIN_UI_CLIENTS: OnceLock<Arc<PluginUiClientRegistry>> = OnceLock::new();

pub fn initialize() -> Arc<PluginUiClientRegistry> {
    PLUGIN_UI_CLIENTS
        .get_or_init(|| Arc::new(PluginUiClientRegistry::default()))
        .clone()
}

pub fn global() -> Option<Arc<PluginUiClientRegistry>> {
    PLUGIN_UI_CLIENTS.get().cloned()
}
