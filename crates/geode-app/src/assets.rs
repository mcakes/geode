//! Application asset source combining selected catalogue icons with the
//! component library's default bundle.
//!
//! Catalogue names alone do not embed icon bytes. [`ExtraIcons`] selects the
//! additional icons used by Geode; add entries here when a surface uses an icon
//! outside the default bundle.
//!
//! [`AppAssets`] loads selected extras first and falls back to the default
//! bundle when an extra is absent. Listings combine both sources, sorted and
//! deduplicated.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [
        // The scope bar's save chip and the frame readout's workspace pin
        // glyph (`shell::toolbar`).
        Save, Pin,
        // The diagnostics page's sidebar button (`Activity`) and its log
        // detail copy button (`Copy`, `geode_diagnostics::page_chrome`).
        Activity, Copy,
    ]
);

/// [`ExtraIcons`] over [`gpui_kit_assets::Assets`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit_assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every icon selected outside the default bundle must resolve to
    /// nonempty bytes: a missing one paints nothing and reports nothing.
    #[test]
    fn the_extra_icons_the_shell_paints_are_served() {
        use gpui_kit_assets::IconName;
        for icon in [
            IconName::Save,
            IconName::Pin,
            IconName::Activity,
            IconName::Copy,
        ] {
            let path = icon.path();
            let bytes = AppAssets.load(&path).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "{path} is not served");
        }
    }

    /// Default component icons remain loadable, and listings include extras.
    #[test]
    fn the_default_component_icons_are_still_served() {
        let path = gpui_kit_assets::IconName::Close.path();
        assert!(AppAssets.load(&path).unwrap().is_some());
        let listed = AppAssets.list("icons").unwrap();
        assert!(listed.iter().any(|p| p.ends_with("save.svg")), "{listed:?}");
    }
}
