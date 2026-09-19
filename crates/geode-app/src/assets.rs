//! The app's asset source: gpui-kit's default component icons plus the
//! handful of catalogue icons Geode's own surfaces name.
//!
//! `gpui_kit_assets::Assets` embeds only the 101 icons
//! `gpui_component::IconName` enumerates (its `default-icons.txt`); the
//! other 1,700-odd Lucide icons exist in the shared catalogue as *names*
//! but their bytes are embedded only where an application selects them
//! (`icon_assets!`). A shell surface may name any catalogue icon —
//! `IconName` is just a path — but the glyph paints only if a source here
//! can serve it, so **every catalogue icon a crate paints is listed in
//! [`ExtraIcons`]**, and this file is the one place to add the next one.
//! Under a source that lacks it the icon renders as an empty glyph, not a
//! panic, which is why a chip that carries one keeps a tooltip saying
//! what it does.
//!
//! The composition (extras first, default bundle second) is the crate's
//! own `extra_assets` example verbatim; `list` merges and dedupes so a
//! path listing sees each icon once whichever source holds it.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

gpui_kit_assets::icon_assets!(
    ExtraIcons,
    [
        // The scope bar's save chip (`shell::toolbar`).
        Save,
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

    /// The save chip's icon is served — the one property this file exists
    /// for, pinned so a future `IconName` the toolbar names without a
    /// matching entry here fails a test rather than painting nothing.
    #[test]
    fn the_extra_icons_the_shell_paints_are_served() {
        let path = gpui_kit_assets::IconName::Save.path();
        let bytes = AppAssets.load(&path).unwrap();
        assert!(bytes.is_some_and(|b| !b.is_empty()), "{path} is not served");
    }

    /// The default bundle is still reachable through the composed source —
    /// the extras are added, never substituted.
    #[test]
    fn the_default_component_icons_are_still_served() {
        let path = gpui_kit_assets::IconName::Close.path();
        assert!(AppAssets.load(&path).unwrap().is_some());
        let listed = AppAssets.list("icons").unwrap();
        assert!(listed.iter().any(|p| p.ends_with("save.svg")), "{listed:?}");
    }
}
