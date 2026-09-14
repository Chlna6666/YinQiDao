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
    pub page: Option<UiPageModel>,
    pub toast: Option<String>,
    pub close: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiCommandSurface {
    CommandPalette,
    TrackContext,
    PlaylistContext,
    PageLocal,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiCommandTrackContext {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub provider_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiCommandPlaylistContext {
    pub name: Option<String>,
    pub provider_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiCommandContext {
    pub surface: UiCommandSurface,
    pub page_id: Option<String>,
    pub track: Option<UiCommandTrackContext>,
    pub playlist: Option<UiCommandPlaylistContext>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UiCommandResponse {
    pub toast: Option<String>,
    /// Local page id in the same plugin namespace. Host validates it before navigation.
    pub open_page_id: Option<String>,
}

/// Runtime-neutral semantic boundary for Component UI exports.
///
/// The Wasmtime adapter implements this trait. GPUI must never invoke it from `render`/paint;
/// ordinary async controller code loads a page, dispatches an event, or invokes a command. The
/// runtime-port layer wraps this adapter with Host call budgets before publishing it to UI callers.
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

    fn invoke_command<'a>(
        &'a self,
        plugin_id: &'a str,
        command_id: &'a str,
        context: UiCommandContext,
    ) -> PluginUiFuture<'a, UiCommandResponse>;
}

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
