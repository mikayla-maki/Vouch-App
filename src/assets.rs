use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;

/// Embeds the entire `assets/` directory so any icon path requested by
/// gpui-component widgets (e.g. `icons/close.svg` for the dialog close
/// button) resolves without per-icon match arms.
#[derive(RustEmbed)]
#[folder = "assets"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let asset = Self::get(path).map(|file| file.data);
        if asset.is_none() {
            eprintln!("warning: missing embedded asset: {}", path);
        }
        Ok(asset)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Self::iter()
            .filter(|asset_path| asset_path.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}
