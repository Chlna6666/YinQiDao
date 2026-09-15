use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
};

use gpui::{App, AppContext, Entity, Focusable, Window};

use crate::{
    plugin::management,
    ui::components::input::{HostTextInput, HostTextInputCommitHandler},
};

const MAX_ACTIVE_PLUGIN_INPUTS: usize = 32;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PluginInputKey {
    plugin_id: String,
    page_id: String,
    revision: u64,
    field_id: String,
}

#[derive(Clone, Debug)]
struct PluginInputSurface {
    plugin_id: String,
    page_id: String,
    revision: u64,
}

thread_local! {
    static CURRENT_SURFACE: RefCell<Option<PluginInputSurface>> = const { RefCell::new(None) };
    static ACTIVE_INPUTS: RefCell<HashMap<PluginInputKey, Entity<HostTextInput>>> = RefCell::new(HashMap::new());
}

/// Scopes field identity to one Host-owned immutable page snapshot while the declarative renderer
/// walks the tree. The stable key is `plugin_id + page_id + revision + field_id`; it never depends
/// on allocator addresses and therefore cannot alias a later page model that reuses the same memory.
pub struct PluginInputSurfaceGuard {
    previous: Option<PluginInputSurface>,
}

impl Drop for PluginInputSurfaceGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_SURFACE.with(|surface| {
            *surface.borrow_mut() = previous;
        });
    }
}

pub fn begin_surface(
    plugin_id: &str,
    page_id: &str,
    revision: u64,
) -> PluginInputSurfaceGuard {
    // A page revision is immutable. Once a newer snapshot is rendered, editors belonging to any
    // older revision of that exact plugin/page can no longer publish against the current page cache
    // ticket and must be dropped together with their IME/selection/caret state.
    ACTIVE_INPUTS.with(|inputs| {
        inputs.borrow_mut().retain(|key, _| {
            key.plugin_id != plugin_id || key.page_id != page_id || key.revision == revision
        });
    });

    let next = PluginInputSurface {
        plugin_id: plugin_id.to_owned(),
        page_id: page_id.to_owned(),
        revision,
    };
    let previous = CURRENT_SURFACE.with(|surface| surface.borrow_mut().replace(next));
    PluginInputSurfaceGuard { previous }
}

pub fn key_for_field(field_id: &str) -> Option<PluginInputKey> {
    CURRENT_SURFACE.with(|surface| {
        let surface = surface.borrow();
        let surface = surface.as_ref()?;
        Some(PluginInputKey {
            plugin_id: surface.plugin_id.clone(),
            page_id: surface.page_id.clone(),
            revision: surface.revision,
            field_id: field_id.to_owned(),
        })
    })
}

pub fn active(key: &PluginInputKey) -> Option<Entity<HostTextInput>> {
    ACTIVE_INPUTS.with(|inputs| inputs.borrow().get(key).cloned())
}

/// Clear every plugin editor. This is used after package-management operations whose UI registry
/// generation may have changed; it is intentionally Host-only and never calls guest/WASM code.
pub fn invalidate_all() -> usize {
    let removed = ACTIVE_INPUTS.with(|inputs| {
        let mut inputs = inputs.borrow_mut();
        let removed = inputs.len();
        inputs.clear();
        removed
    });
    CURRENT_SURFACE.with(|surface| {
        surface.borrow_mut().take();
    });
    removed
}

/// Materialize one Host-owned input editor. IME composition, selection, clipboard and caret work
/// entirely inside GPUI; the supplied callback runs only when the user commits with Enter.
pub fn activate(
    key: PluginInputKey,
    value: String,
    placeholder: String,
    secret: bool,
    on_commit: HostTextInputCommitHandler,
    window: &mut Window,
    cx: &mut App,
) {
    crate::ui::components::input::ensure_initialized(cx);

    if let Some(input) = active(&key) {
        let focus_handle = input.read(cx).focus_handle(cx);
        window.focus(&focus_handle);
        return;
    }

    let commit_key = key.clone();
    let commit: HostTextInputCommitHandler = Rc::new(move |value, window, cx| {
        // A page may publish a newer revision between the guest response and the next GPUI render.
        // Revalidate against the Host page cache at the final Enter boundary so an editor from the
        // previous revision can never submit into a newer page generation during that one-frame gap.
        let revision_is_current = management::page_snapshot(
            &commit_key.plugin_id,
            &commit_key.page_id,
        )
        .ok()
        .flatten()
        .is_some_and(|snapshot| snapshot.revision == commit_key.revision);

        ACTIVE_INPUTS.with(|inputs| {
            inputs.borrow_mut().remove(&commit_key);
        });
        if revision_is_current {
            on_commit(value, window, cx);
        }
        window.request_animation_frame();
    });

    let input = cx.new(move |entity_cx| {
        HostTextInput::new(entity_cx, value, placeholder, secret, commit)
    });

    ACTIVE_INPUTS.with(|inputs| {
        let mut inputs = inputs.borrow_mut();
        if inputs.len() >= MAX_ACTIVE_PLUGIN_INPUTS
            && let Some(evicted) = inputs.keys().next().cloned()
        {
            inputs.remove(&evicted);
        }
        inputs.insert(key, input.clone());
    });

    let focus_handle = input.read(cx).focus_handle(cx);
    window.focus(&focus_handle);
    window.request_animation_frame();
}
