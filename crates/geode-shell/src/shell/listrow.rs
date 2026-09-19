//! The one door a list row's state colours come through — the command
//! palette, the settings, keybindings and object dialogs, the dimension
//! picker, the as-of presets, the command line's completions: every
//! surface that paints a highlighted row and, since the design-guide
//! audit (2026-09-19), a hovered one.
//!
//! Two things it fixes at once. The rows used to paint the highlighted
//! row `theme.selection` under `theme.primary` text: `selection` is
//! gpui-component's TEXT-selection colour, not its list-row token, and
//! `primary` over it is under the 3:1 floor on 15 of 44 bundled themes
//! (Fahrenheit 1.53:1) — the highlighted row, the one a trader is about
//! to `enter`, was the least readable row in the list. The list-row
//! tokens are `list_active` (the highlighted row) and `list_hover` (the
//! pointer's row), and `foreground` clears the floor over both on every
//! bundled theme (worst 3.85:1 and 4.53:1, Solarized Light), so those
//! are the pair. And there was no hover feedback at all: the design
//! guide asks for subtle pointer feedback on every clickable row (never
//! the only cue — the highlight is the state, the hover is the pointer).
//!
//! [`RowPaint::accent`] is the fuzzy-match colour on a highlighted row —
//! `primary`, floored the way [`super::chip`] floors a text tone, because
//! `primary` alone is under the floor on 11 themes over the active fill.
//! The match glyphs are bold as well, so the colour is never the only
//! cue.
//!
//! Two things this door does NOT claim. A row's SECONDARY line (the
//! muted category, summary or role) keeps `muted_foreground`, which is
//! under the floor over the active fill on 16 bundled themes (Solarized
//! Light 1.91:1) — down from 25 over the old `selection`, and the same
//! theme-authoring matter as `muted_foreground` on the bare background
//! (nine themes ship it under 3:1; the market-data header's `Plain` tone
//! made the same call). And `list_active` and `list_hover` are distinct
//! `Hsla`s on every theme but composited within 1.01:1 of each other on
//! seven (Harper, Solarized Dark, Adventure Time among them) — the
//! library's own `ListItem` wears the same pair, so a keyboard highlight
//! and a resting pointer can merge there; gpui suppresses hover after a
//! keystroke until the mouse moves, which narrows it. Both are recorded
//! rather than papered over with a border every row would have to
//! reserve.
//!
//! Why `list_active` and not `accent`: gpui-component's `ListItem` paints
//! its rows `list_active`/`list_hover`, while its `MenuItem` (and so the
//! market-data `⋯` popup, which copies the menu family) paints `accent`.
//! The shell's nine lists are lists — selectable rows a trader moves
//! through and commits — not menus, so they wear the list pair.

use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
use gpui::Hsla;
use gpui_component::Theme;

use super::colours::{over, to_hsla, to_rgb};

/// A list row's state colours on `theme`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RowPaint {
    /// The highlighted row's fill (`list_active`).
    pub active: Hsla,
    /// The pointer's row's fill (`list_hover`); painted only while the
    /// row is not the highlighted one, so the two states stay distinct.
    pub hover: Hsla,
    /// Text on either fill and at rest: the plain `foreground`.
    pub text: Hsla,
    /// The fuzzy-match colour on a highlighted row: `primary` floored to
    /// the readable ratio against the active fill over the popover.
    pub accent: Hsla,
}

/// The colours for a list row on `theme`, decided once per render.
pub fn row_paint(theme: &Theme) -> RowPaint {
    let ground = over(theme.list_active, to_rgb(theme.popover));
    RowPaint {
        active: theme.list_active,
        hover: theme.list_hover,
        text: theme.foreground,
        accent: to_hsla(readable_on(
            to_rgb(theme.primary),
            ground,
            to_rgb(theme.foreground),
        )),
    }
}

/// Whether `text` clears the readability floor over `fill` composited on
/// the popover surface every list here sits on.
pub fn is_readable(theme: &Theme, text: Hsla, fill: Option<Hsla>) -> bool {
    let popover: Rgb = to_rgb(theme.popover);
    let ground = match fill {
        Some(fill) => over(fill, popover),
        None => popover,
    };
    contrast_ratio(to_rgb(text), ground) >= READABLE_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ActiveTheme as _;

    /// Text and accent clear the floor on the active row, the hovered
    /// row and at rest, on EVERY bundled theme — no exception list. The
    /// pairing this replaced failed on 15.
    #[gpui::test]
    fn every_row_state_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = row_paint(theme);
                // At rest a row paints no text colour of its own and
                // inherits the panel's `popover_foreground`, so that is
                // what "at rest" measures — not `p.text`, which only
                // ever lands on the active row.
                for (state, text, fill) in [
                    ("text on active", p.text, Some(p.active)),
                    ("text on hover", p.text, Some(p.hover)),
                    ("popover_foreground at rest", theme.popover_foreground, None),
                    (
                        "popover_foreground on hover",
                        theme.popover_foreground,
                        Some(p.hover),
                    ),
                    ("accent on active", p.accent, Some(p.active)),
                    ("accent at rest", p.accent, None),
                ] {
                    checked += 1;
                    if !is_readable(theme, text, fill) {
                        failures.push(format!("{name}: {state}"));
                    }
                }
            });
        }
        assert!(checked >= 6 * 40, "the sweep saw {checked} checks");
        assert!(
            failures.is_empty(),
            "unreadable rows:\n{}",
            failures.join("\n")
        );
    }

    /// The retired pairing must still fail the same sweep, or the floor
    /// is not measuring what the module doc claims.
    #[gpui::test]
    fn the_retired_pairing_still_fails_the_sweep(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failing = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                if !is_readable(theme, theme.primary, Some(theme.selection)) {
                    failing += 1;
                }
            });
        }
        assert!(
            failing >= 10,
            "primary on selection failed on only {failing} themes — the sweep has lost its teeth"
        );
    }

    /// Every fuzzy-match run in the crate takes [`RowPaint::accent`], not
    /// a raw `primary`: the sweep above measures the door, and the one
    /// way past it is a call site handing `highlighted_text`/`_title` the
    /// token by hand — which the branch's review found once (the Sources
    /// row's `<dataset> · ` prefix run). A source scan is the only test
    /// that can see a call site's colour argument.
    #[test]
    fn every_highlight_run_takes_the_doors_accent() {
        let sources: [(&str, &str); 6] = [
            ("palette.rs", include_str!("../palette.rs")),
            ("commandline_view.rs", include_str!("commandline_view.rs")),
            ("keybindings_view.rs", include_str!("keybindings_view.rs")),
            (
                "objectdialog/render.rs",
                include_str!("objectdialog/render.rs"),
            ),
            ("picker.rs", include_str!("picker.rs")),
            ("settings_view.rs", include_str!("settings_view.rs")),
        ];
        let mut offenders = Vec::new();
        let mut calls = 0;
        for (name, text) in sources {
            for needle in ["highlighted_text(", "highlighted_title("] {
                for (at, _) in text.match_indices(needle) {
                    // Skip the definitions themselves.
                    if text[..at].ends_with("fn ") || text[..at].ends_with("pub(crate) fn ") {
                        continue;
                    }
                    calls += 1;
                    let window = &text[at..(at + 240).min(text.len())];
                    if window.contains("theme.primary") || window.contains(", primary)") {
                        let line = text[..at].lines().count();
                        offenders.push(format!("{name}:{line}"));
                    }
                }
            }
        }
        assert!(
            calls >= 15,
            "the scan saw only {calls} highlight calls — a file moved?"
        );
        assert!(
            offenders.is_empty(),
            "highlight runs handed a raw primary instead of RowPaint::accent:\n{}",
            offenders.join("\n")
        );
    }

    /// Active and hover are the list-row tokens and are distinct from
    /// each other on every bundled theme, so the highlighted row and the
    /// pointer's row never merge.
    #[gpui::test]
    fn active_and_hover_are_the_list_tokens_and_differ(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = row_paint(theme);
                assert_eq!(p.active, theme.list_active, "{name}");
                assert_eq!(p.hover, theme.list_hover, "{name}");
                assert_eq!(p.text, theme.foreground, "{name}");
                assert_ne!(p.active, p.hover, "{name}: active and hover must differ");
            });
        }
    }
}
