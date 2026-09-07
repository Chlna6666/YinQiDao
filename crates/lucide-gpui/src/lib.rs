use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub struct Assets;

pub(crate) mod registry {
    use super::*;

    fn registry() -> &'static RwLock<HashMap<&'static str, &'static [u8]>> {
        static REGISTRY: OnceLock<RwLock<HashMap<&'static str, &'static [u8]>>> = OnceLock::new();
        REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
    }

    pub fn register(path: &'static str, bytes: &'static [u8]) {
        if let Ok(mut map) = registry().write() {
            map.insert(path, bytes);
        }
    }

    pub fn get(path: &str) -> Option<&'static [u8]> {
        let map = registry().read().ok()?;
        map.get(path).copied()
    }

    pub fn list(prefix: &str) -> Vec<SharedString> {
        let map = match registry().read() {
            Ok(map) => map,
            Err(_) => return Vec::new(),
        };
        map.keys()
            .filter(|key| key.starts_with(prefix))
            .map(|key| SharedString::from(*key))
            .collect()
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(registry::get(path).map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path.is_empty() || path == "lucide" || path == "lucide/" {
            return Ok(registry::list("lucide/"));
        }

        if registry::get(path).is_some() {
            return Ok(vec![SharedString::from(path.to_string())]);
        }

        if path.starts_with("lucide/") || path == "lucide" {
            return Err(anyhow!("could not find asset at path \"{path}\"").into());
        }

        Ok(Vec::new())
    }
}

// The proc-macro only resolves one requested identifier to one SVG at each `icon!(...)` call site.
// It does not scan the icon directory or emit a full icon API, so unused Lucide assets never become
// rustc inputs. Keep the public macro in this crate; callers do not depend on the implementation
// crate directly.
#[doc(hidden)]
pub use lucide_gpui_macros::__icon_asset;

#[doc(hidden)]
pub fn __register_icon(path: &'static str, bytes: &'static [u8]) {
    registry::register(path, bytes);
}

/// Embed and register exactly one Lucide SVG at the call site.
///
/// Underscores in the Rust identifier map to Lucide's hyphenated file names, for example
/// `icon!(folder_plus)` resolves `icons/folder-plus.svg`.
#[macro_export]
macro_rules! icon {
    ($name:ident) => {{
        const ASSET: (&'static str, &'static [u8]) = $crate::__icon_asset!($name);
        static ONCE: ::std::sync::Once = ::std::sync::Once::new();
        ONCE.call_once(|| $crate::__register_icon(ASSET.0, ASSET.1));
        ASSET.0
    }};
}

// Transitional compatibility boundary while application call sites migrate to `icon!(name)`.
// `icons_gen.rs` is still emitted by build.rs for now and is removed once no `icon_xxx()` calls
// remain. Generated code stays isolated; hand-written modules must use normal module declarations.
mod generated {
    include!(concat!(env!("OUT_DIR"), "/icons_gen.rs"));
}

pub use generated::icons;
