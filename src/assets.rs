use gpui::{AssetSource, SharedString};
use std::borrow::Cow;

#[derive(Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let contents = match path {
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg").as_slice()),
            "icons/circle-x.svg" => Some(include_bytes!("../assets/icons/circle-x.svg").as_slice()),
            _ => None,
        };

        Ok(contents.map(|bytes| Cow::Borrowed(bytes)))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
