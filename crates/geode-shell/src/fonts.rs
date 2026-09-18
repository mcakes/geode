//! Bundled fonts (Task 10, phase 1c, user direction mid-phase): **Inter** is
//! the default UI face (chrome, palette titles, dialogs, hints); **JetBrains
//! Mono** is the data face — status pending keys, the which-key key column,
//! palette binding hints, tile placeholder labels today, and documented as
//! the face the phase-3 blotter will use for cells.
//!
//! # Inventory (recorded before wiring, per plan constraint)
//!
//! Checked against the pinned releases named in the root `Cargo.toml` —
//! gpui-pre 0.3.5 and gpui-component 0.6.2:
//!
//! - **Embedded-font API.** `App::text_system(&self) -> &Arc<TextSystem>`
//!   (`gpui-pre-0.3.5/src/app.rs`); `TextSystem::add_fonts(&self, fonts:
//!   Vec<Cow<'static, [u8]>>) -> Result<()>` (`gpui-pre-0.3.5/src/
//!   text_system.rs`, delegating to `PlatformTextSystem::add_fonts`,
//!   `gpui-pre-0.3.5/src/platform.rs`) is the exact registration call.
//!   Zed's own bootstrap (zed's own repo, `crates/zed/src/main.rs`,
//!   `load_embedded_fonts`) lists an asset dir for `.ttf` paths, loads each
//!   file's bytes, and makes one `add_fonts(Vec<Cow<..>>)` call — the same
//!   shape [`register`] follows below, except bytes come from
//!   `include_bytes!` (no runtime file I/O, no extra `AssetSource`) rather
//!   than listing a `rust_embed` folder, since the brief calls embedded bytes
//!   fine and this repo's `AssetSource` is `gpui_kit_assets::Assets`
//!   (vendored upstream, not ours to extend with an app-specific `fonts/`
//!   folder).
//! - **Theme font-family seam.** `gpui_component::Theme`
//!   (`gpui-component-0.6.2/src/theme/mod.rs`) carries `font_family:
//!   SharedString` (default `.SystemUIFont`, the macOS system UI font —
//!   `mod.rs` `impl From<&ThemeColor> for Theme`) and `mono_font_family:
//!   SharedString` (default `Menlo`/`Consolas`/`DejaVu Sans Mono` by
//!   platform). `Root::render` (`gpui-component-0.6.2/src/root.rs`, the
//!   root `div`'s `.font_family(cx.theme().font_family.clone())`) applies
//!   to the app-wide root `div` that wraps `ShellView`, so it cascades to
//!   every element that doesn't set its own `font_family` — exactly the
//!   seam [`register`] uses to make Inter the default face app-wide.
//! - **Theme JSON does not fight this.** `ThemeConfig`
//!   (`gpui-component-0.6.2/src/theme/schema.rs`) carries the *optional*
//!   mirror fields `font_family: Option<SharedString>` /
//!   `mono_font_family: Option<SharedString>` — `ThemeConfig`'s own
//!   `font_family`/`mono_font_family` fields; `Theme::apply_config` only
//!   overwrites `Theme::font_family` / `mono_font_family` when the
//!   config's field is `Some`, in its font-family assignment block. None
//!   of this repo's 44 bundled `assets/themes/*.json` themes set either
//!   key (`grep -l font_family
//!   assets/themes/*.json` → zero matches, checked at implementation
//!   time), so switching theme family/mode via `theme::Registry::
//!   apply_theme` (which calls `Theme::global_mut(cx).apply_config`) never
//!   clobbers the families [`register`] sets here — confirmed by
//!   `mono_family_survives_a_theme_switch` below.
//!
//! # Fallback honesty
//!
//! A failure from `add_fonts` only warns to stderr and leaves the
//! platform-default families in place (the `Theme` global's `font_family`/
//! `mono_font_family` are left untouched, so `.SystemUIFont`/`Menlo`-style
//! defaults keep working) — this must never panic (spec §10.1: bad input
//! never stops the app from starting).

use std::borrow::Cow;

use gpui::App;

/// The data face — reference this constant at mono call sites (status
/// pending keys, which-key key column, palette binding hints, tile
/// placeholder labels; future blotter cells), never a scattered string
/// literal.
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

/// Register the bundled Inter/JetBrains Mono weights with gpui's text
/// system, then make [`UI`] the theme's default face and [`MONO`] its mono
/// face. Call once at startup — after `gpui_component::init(cx)` (which
/// installs the `Theme` global this then edits) and before the window
/// opens, so the very first frame already carries the bundled faces.
///
/// On failure to register (see module docs, "Fallback honesty"), warns to
/// stderr and returns without touching the theme's font families, so the
/// app keeps running on the gpui-component/platform default fonts.
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

    /// **Honest limitation** (brief: "asserting the families resolve in the
    /// text system if the API allows; else document"): it doesn't, under
    /// `#[gpui::test]`. `TestAppContext::build` (`gpui-pre-0.3.5/src/app/
    /// test_context.rs`) builds its platform via `TestPlatform::new`,
    /// which wires up `Arc::new(NoopTextSystem)` (`gpui-pre-0.3.5/src/
    /// platform/test/platform.rs`) rather than a real `cosmic_text`-backed
    /// system — `NoopTextSystem::add_fonts` is a literal no-op returning
    /// `Ok(())` regardless of the bytes given it, and its
    /// `all_font_names()` returns an empty `Vec` (`gpui-pre-0.3.5/src/
    /// platform.rs`, `impl PlatformTextSystem for NoopTextSystem`).
    /// `TextSystem::all_font_names()` then only ever reports its own
    /// hardcoded fallback stack (`.ZedMono`, `.ZedSans`, `Helvetica`, …)
    /// plus `.SystemUIFont` on top of that empty platform list — confirmed
    /// by running the family-membership assertion below against a real
    /// `register()` call: it fails, listing exactly that fallback set, no
    /// matter what `add_fonts` was given. So a `#[gpui::test]` can exercise
    /// [`register`]'s *control flow* (below) but never a real "did the
    /// bytes parse as a loadable font" check — that's covered instead by
    /// [`vendored_fonts_start_with_the_sfnt_version_tag`], a plain `#[test]`
    /// asserting each embedded byte slice starts with TrueType's `sfnt`
    /// version tag (`0x00010000`, confirmed via `xxd -l4` against every
    /// vendored file before writing this test) — genuine confirmation the
    /// vendored files are real, uncorrupted TrueType data, just not routed
    /// through gpui's font-matching machinery.
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

    /// Inventory finding above ("Theme JSON does not fight this"): applying
    /// a bundled theme config (none of which set `font_family`/
    /// `mono_font_family`) after `register` must not revert the families
    /// it set — this is the load-bearing guarantee that lets `register`
    /// run once at startup rather than after every theme switch.
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
