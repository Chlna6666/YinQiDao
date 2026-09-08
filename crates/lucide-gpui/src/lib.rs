use anyhow::anyhow;
use gpui::{AssetSource, Result, SharedString};
use std::{borrow::Cow, sync::OnceLock};

pub struct StaticAsset {
    pub path: &'static str,
    pub bytes: &'static [u8],
}

impl StaticAsset {
    #[must_use]
    pub const fn new(path: &'static str, bytes: &'static [u8]) -> Self {
        Self { path, bytes }
    }
}

static ASSETS: OnceLock<&'static [StaticAsset]> = OnceLock::new();

pub fn install_assets(assets: &'static [StaticAsset]) {
    let _ = ASSETS.set(assets);
}

fn assets() -> &'static [StaticAsset] {
    ASSETS.get().copied().unwrap_or(&[])
}

fn asset(path: &str) -> Option<&'static StaticAsset> {
    let assets = assets();
    assets
        .binary_search_by(|asset| asset.path.cmp(path))
        .ok()
        .map(|index| &assets[index])
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(asset(path).map(|asset| Cow::Borrowed(asset.bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path.is_empty() || path == "lucide" || path == "lucide/" {
            return Ok(assets()
                .iter()
                .map(|asset| SharedString::from(asset.path))
                .collect());
        }

        if asset(path).is_some() {
            return Ok(vec![SharedString::from(path.to_owned())]);
        }

        if path.starts_with("lucide/") || path == "lucide" {
            return Err(anyhow!("could not find asset at path \"{path}\""));
        }

        Ok(Vec::new())
    }
}

#[macro_export]
macro_rules! icon {
    ($name:ident) => {
        concat!("lucide/", stringify!($name), ".svg")
    };
}
