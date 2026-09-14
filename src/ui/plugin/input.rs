use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
};

use gpui::{App, AppContext, Entity, Focusable, Window};

use crate::{
    plugin::ui::schema::UiPageModel,
    ui::components::input::{HostTextInput, HostTextInputCommitHandler},
};

const MAX_ACTIVE_PLUGIN_INPUTS: usize = 32;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PluginInputKey {
    plugin_id: String,
    surface_key: usize,
    field_id: String,
}

#[derive(Clone, Debug)]
struct PluginInputSurface {
    plugin_id: String,
    surface_key: usize,
}

thread_local! {
    static CURRENT_SURFACE: RefCell<Option<PluginInputSurface>> = const { RefCell::new(None) };
    static ACTIVE_INPUTS: RefCell<HashMap<PluginInputKey, Entity<HostTextInput>>> = RefCell::new(HashMap::new());
}

/// Scopes field identity to one immutable page-model allocation while the declarative renderer
/// walks the tree. The guard never stores the model itself and does not execute guest code.
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

pub fn begin_surface(plugin_id: &str, model: &UiPageModel) -> PluginInputSurfaceGuard {
    let next = PluginInputSurface {
        plugin_id: plugin_id.to_owned(),
        surface_key: model as *const UiPageModel as usize,
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
            surface_key: surface.surface_key,
            field_id: field_id.to_owned(),
        })
    })
}

pub fn active(key: &PluginInputKey) -> Option<Entity<HostTextInput>> {
    ACTIVE_INPUTS.with(|inputs| inputs.borrow().get(key).cloned())
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
        ACTIVE_INPUTS.with(|inputs| {
            inputs.borrow_mut().remove(&commit_key);
        });
        on_commit(value, window, cx);
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
