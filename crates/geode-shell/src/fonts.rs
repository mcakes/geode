//! Bundled Inter UI fonts and JetBrains Mono data fonts, embedded with
//! `include_bytes!` so registration needs no runtime file I/O.
//!
//! [`register`] loads the weights into GPUI's text system and sets the
//! `Theme` global's default and monospace families. Call it after
//! `gpui_component::init` and before opening a window. A registration failure
//! logs a warning and leaves the theme's existing font families untouched.
//!
//! Bundled theme configurations omit font-family overrides, so applying
//! one preserves the families installed at startup. `Root` inherits the
//! UI family; data and keybinding displays use [`MONO`].

use std::borrow::Cow;

use gpui::App;

/// The data face for tables, keybinding hints, and other monospace displays.
/// Use this constant at call sites to keep the family name consistent.
pub const MONO: &str = "JetBrains Mono";

/// The default UI face (chrome, palette titles, dialogs, hints) — set as
/// the theme's `font_family` by [`register`]; call sites don't normally
/// need to reference this directly since it's the app-wide default.
pub const UI: &str = "Inter";

const INTER_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf");
const JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf");
const JETBRAINS_MONO_BOLD: &[u8] =
    include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf");

/// Register the embedded font weights, then set the theme's [`UI`] and
/// [`MONO`] families. Call after `gpui_component::init` and before opening
/// windows. Registration failures emit a `geode::theme` warning and return
/// without changing the theme's font families.
pub fn register(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(INTER_REGULAR),
        Cow::Borrowed(INTER_MEDIUM),
        Cow::Borrowed(INTER_SEMIBOLD),
        Cow::Borrowed(JETBRAINS_MONO_REGULAR),
        Cow::Borrowed(JETBRAINS_MONO_BOLD),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        tracing::warn!(
            target: "geode::theme",
            "failed to register bundled fonts ({err}); using the platform default font instead"
        );
        return;
    }

    let theme = gpui_component::Theme::global_mut(cx);
    theme.font_family = UI.into();
    theme.mono_font_family = MONO.into();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Check the four-byte TrueType `sfnt` version tag of each embedded file.
    /// This detects a wrong header, not corruption elsewhere or a font-loading
    /// failure. GPUI tests use a no-op text system, so the tests below exercise
    /// registration's theme updates without proving the fonts can be loaded or
    /// matched by the platform text system.
    #[test]
    fn vendored_fonts_start_with_the_sfnt_version_tag() {
        const SFNT_VERSION_1: [u8; 4] = [0x00, 0x01, 0x00, 0x00];
        for (name, bytes) in [
            ("Inter-Regular.ttf", INTER_REGULAR),
            ("Inter-Medium.ttf", INTER_MEDIUM),
            ("Inter-SemiBold.ttf", INTER_SEMIBOLD),
            ("JetBrainsMono-Regular.ttf", JETBRAINS_MONO_REGULAR),
            ("JetBrainsMono-Bold.ttf", JETBRAINS_MONO_BOLD),
        ] {
            assert_eq!(
                bytes.get(..4),
                Some(SFNT_VERSION_1.as_slice()),
                "{name} should start with the TrueType sfnt version tag"
            );
        }
    }

    /// `register` sets the theme's default/mono families to [`UI`]/
    /// [`MONO`] — the seam `gpui_component::Root::render` reads
    /// (`.font_family(cx.theme().font_family.clone())`) to cascade Inter
    /// app-wide.
    #[gpui::test]
    fn register_sets_the_theme_font_families(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            register(cx);

            let theme = gpui_component::Theme::global(cx);
            assert_eq!(theme.font_family.as_ref(), UI);
            assert_eq!(theme.mono_font_family.as_ref(), MONO);
        });
    }

    /// Applying a bundled theme after registration preserves the installed
    /// families, so registration need only run once at startup.
    #[gpui::test]
    fn mono_family_survives_a_theme_switch(cx: &mut gpui::TestAppContext) {
        use crate::theme::load_bundled;

        cx.update(|cx| {
            gpui_component::init(cx);
            register(cx);

            let (mut theme_service, _warnings) = load_bundled();
            assert!(theme_service.apply("Default Dark", cx));

            let theme = gpui_component::Theme::global(cx);
            assert_eq!(theme.font_family.as_ref(), UI);
            assert_eq!(theme.mono_font_family.as_ref(), MONO);
        });
    }
}
