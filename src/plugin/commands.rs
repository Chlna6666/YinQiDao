use anyhow::{Context, Result, anyhow, bail};

use super::{
    extensions::PluginCommandSurface,
    host::package_manager,
    ui::{
        client::{
            self, UiCommandContext, UiCommandPlaylistContext, UiCommandResponse, UiCommandSurface,
            UiCommandTrackContext,
        },
        manifest::{UiCommandPlacement, qualified_ui_id},
        registry,
    },
};

const MAX_COMMAND_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_COMMAND_TEXT_BYTES: usize = 8 * 1024;
const MAX_COMMAND_ARTISTS: usize = 64;
const MAX_COMMAND_TOAST_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginTrackCommandContext {
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: Option<u64>,
    pub provider_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginPlaylistCommandContext {
    pub name: Option<String>,
    pub provider_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommandContext {
    pub surface: PluginCommandSurface,
    pub page_id: Option<String>,
    pub track: Option<PluginTrackCommandContext>,
    pub playlist: Option<PluginPlaylistCommandContext>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginCommandOpenPage {
    pub plugin_id: String,
    pub page_id: String,
    pub qualified_id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PluginCommandResult {
    pub toast: Option<String>,
    pub open_page: Option<PluginCommandOpenPage>,
}

/// Invoke one registered plugin command on an ordinary async controller path.
///
/// The command contribution is the authority for allowed surfaces. Context is surface-minimized,
/// bounded before it reaches guest code, and the published UI client is already wrapped by the
/// Host runtime call budget. Returned navigation is a local page id and is validated against the
/// same plugin namespace before being exposed to the application.
pub async fn invoke_command(
    qualified_id: &str,
    context: PluginCommandContext,
) -> Result<PluginCommandResult> {
    let registered = registered_command(qualified_id)?;
    let manager = package_manager::global().ok_or_else(|| anyhow!("插件包管理器尚未初始化"))?;
    if !manager.is_enabled(&registered.plugin_id) {
        bail!("插件已禁用，拒绝执行 Command: {}", registered.plugin_id);
    }

    let placement = command_placement(context.surface);
    if !registered.contribution.placements.contains(&placement) {
        bail!("插件 Command 未声明当前执行 surface: {qualified_id}");
    }
    validate_context(&registered.plugin_id, &context)?;
    let wire_context = to_wire_context(context);

    let clients = client::global().unwrap_or_else(client::initialize);
    let client = clients
        .client()?
        .ok_or_else(|| anyhow!("插件 Component UI runtime 尚未就绪"))?;
    let response = client
        .invoke_command(
            &registered.plugin_id,
            &registered.contribution.id,
            wire_context,
        )
        .await
        .with_context(|| format!("执行插件 Command 失败: {qualified_id}"))?;

    // Re-check registration after guest execution. Disable/update/uninstall during the call makes
    // the result stale even when the old component returned success.
    if !manager.is_enabled(&registered.plugin_id) {
        bail!("插件在 Command 执行期间已禁用，拒绝应用旧结果");
    }
    let current = registered_command(qualified_id)?;
    if current.plugin_id != registered.plugin_id
        || current.contribution.id != registered.contribution.id
    {
        bail!("插件 Command 在执行期间已更新，拒绝应用旧结果");
    }

    validate_response(&registered.plugin_id, response)
}

fn registered_command(qualified_id: &str) -> Result<registry::RegisteredUiCommand> {
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    registry
        .commands()
        .find(|command| command.qualified_id == qualified_id)
        .cloned()
        .ok_or_else(|| anyhow!("插件 Command 未注册: {qualified_id}"))
}

fn validate_context(plugin_id: &str, context: &PluginCommandContext) -> Result<()> {
    match context.surface {
        PluginCommandSurface::CommandPalette => {
            if context.page_id.is_some() || context.track.is_some() || context.playlist.is_some() {
                bail!("CommandPalette 不接受隐式页面/曲目/歌单上下文");
            }
        }
        PluginCommandSurface::TrackContext => {
            if context.track.is_none() || context.page_id.is_some() || context.playlist.is_some() {
                bail!("TrackContext 必须且只能携带曲目上下文");
            }
        }
        PluginCommandSurface::PlaylistContext => {
            if context.playlist.is_none() || context.page_id.is_some() || context.track.is_some() {
                bail!("PlaylistContext 必须且只能携带歌单上下文");
            }
        }
        PluginCommandSurface::PageLocal => {
            let page_id = context
                .page_id
                .as_deref()
                .ok_or_else(|| anyhow!("PageLocal 必须携带 page_id"))?;
            if context.track.is_some() || context.playlist.is_some() {
                bail!("PageLocal 不接受隐式曲目/歌单上下文");
            }
            ensure_plugin_page(plugin_id, page_id)?;
        }
    }

    let mut total = 0usize;
    if let Some(page_id) = context.page_id.as_deref() {
        total = add_text_budget(total, page_id, "page id")?;
    }
    if let Some(track) = context.track.as_ref() {
        if track.artists.len() > MAX_COMMAND_ARTISTS {
            bail!("插件 Command 曲目艺术家数量超过 {MAX_COMMAND_ARTISTS}");
        }
        total = add_text_budget(total, &track.title, "track title")?;
        total = add_text_budget(total, &track.album, "track album")?;
        for artist in &track.artists {
            total = add_text_budget(total, artist, "track artist")?;
        }
        for value in [track.provider_id.as_deref(), track.source_id.as_deref()]
            .into_iter()
            .flatten()
        {
            total = add_text_budget(total, value, "track source")?;
        }
    }
    if let Some(playlist) = context.playlist.as_ref() {
        for value in [
            playlist.name.as_deref(),
            playlist.provider_id.as_deref(),
            playlist.source_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            total = add_text_budget(total, value, "playlist context")?;
        }
    }
    if total > MAX_COMMAND_CONTEXT_BYTES {
        bail!("插件 Command context 超过 {} bytes Host 上限", MAX_COMMAND_CONTEXT_BYTES);
    }
    Ok(())
}

fn add_text_budget(current: usize, value: &str, label: &str) -> Result<usize> {
    if value.len() > MAX_COMMAND_TEXT_BYTES || value.contains('\0') {
        bail!("插件 Command {label} 超过 Host 文本限制");
    }
    Ok(current.saturating_add(value.len()))
}

fn validate_response(plugin_id: &str, response: UiCommandResponse) -> Result<PluginCommandResult> {
    let toast = match response.toast {
        Some(toast) => {
            if toast.len() > MAX_COMMAND_TOAST_BYTES || toast.contains('\0') {
                bail!("插件 Command toast 超过 Host 文本限制");
            }
            Some(toast)
        }
        None => None,
    };
    let open_page = match response.open_page_id {
        Some(page_id) => {
            let qualified_id = ensure_plugin_page(plugin_id, &page_id)?;
            Some(PluginCommandOpenPage {
                plugin_id: plugin_id.to_owned(),
                page_id,
                qualified_id,
            })
        }
        None => None,
    };
    Ok(PluginCommandResult { toast, open_page })
}

fn ensure_plugin_page(plugin_id: &str, page_id: &str) -> Result<String> {
    let qualified_id = qualified_ui_id(plugin_id, page_id)?;
    let registry = registry::global().ok_or_else(|| anyhow!("插件 UI registry 尚未初始化"))?;
    let registry = registry
        .read()
        .map_err(|error| anyhow!("插件 UI registry 锁已损坏: {error}"))?;
    if registry.page(&qualified_id).is_none() {
        bail!("插件 Command 引用了未注册页面: {qualified_id}");
    }
    Ok(qualified_id)
}

fn command_placement(surface: PluginCommandSurface) -> UiCommandPlacement {
    match surface {
        PluginCommandSurface::CommandPalette => UiCommandPlacement::CommandPalette,
        PluginCommandSurface::TrackContext => UiCommandPlacement::TrackContext,
        PluginCommandSurface::PlaylistContext => UiCommandPlacement::PlaylistContext,
        PluginCommandSurface::PageLocal => UiCommandPlacement::PageLocal,
    }
}

fn wire_surface(surface: PluginCommandSurface) -> UiCommandSurface {
    match surface {
        PluginCommandSurface::CommandPalette => UiCommandSurface::CommandPalette,
        PluginCommandSurface::TrackContext => UiCommandSurface::TrackContext,
        PluginCommandSurface::PlaylistContext => UiCommandSurface::PlaylistContext,
        PluginCommandSurface::PageLocal => UiCommandSurface::PageLocal,
    }
}

fn to_wire_context(context: PluginCommandContext) -> UiCommandContext {
    UiCommandContext {
        surface: wire_surface(context.surface),
        page_id: context.page_id,
        track: context.track.map(|track| UiCommandTrackContext {
            title: track.title,
            artists: track.artists,
            album: track.album,
            duration_ms: track.duration_ms,
            provider_id: track.provider_id,
            source_id: track.source_id,
        }),
        playlist: context.playlist.map(|playlist| UiCommandPlaylistContext {
            name: playlist.name,
            provider_id: playlist.provider_id,
            source_id: playlist.source_id,
        }),
    }
}
