//! State colours shared by the shell's selectable list rows.
//!
//! Highlighted rows use `list_active` with `foreground` text. Other rows
//! use `list_hover` under the pointer and inherit their text colour.
//! [`RowPaint::accent`] adjusts `primary` to the readability floor against
//! the active fill over `popover`; fuzzy-match glyphs also use bold text.
//! The theme sweeps below check these text and accent pairings.
//!
//! Secondary labels keep `muted_foreground`, which can fall below that
//! floor on some themes. Active and hover fills have distinct token values,
//! but can appear nearly identical after compositing. GPUI suppresses hover
//! after keyboard input until the pointer moves.
//!
//! Use [`paint_row`] on an element with an ID so hover transitions trigger
//! a repaint. Menu items and component buttons have their own state colours.

use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
use gpui::prelude::*;
use gpui::{Div, Hsla, Stateful};
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

/// The fuzzy-match colour inside a `DataTable`: `primary` floored to the
/// readable ratio on each ground a table row paints on: the cursor row's
/// `table_active`, the pointer's `table_hover`, both over the `table`
/// surface, and that surface at rest.
///
/// [`RowPaint::accent`] floors against `list_active` over `popover`, the
/// grounds a dialog list paints on. A table paints on neither, and a theme
/// may set its table fills apart from the list ones. Each floor moves
/// toward whichever of black or white contrasts more with its ground; on
/// grounds of one polarity the moves only add contrast, so a later floor
/// keeps the earlier ones. Grounds of mixed polarity can leave one state
/// short, which the theme sweep reports.
pub fn table_accent(theme: &Theme) -> Hsla {
    let table = to_rgb(theme.table);
    let grounds = [
        over(theme.table_active, table),
        over(theme.table_hover, table),
        table,
    ];
    let accent = grounds
        .into_iter()
        .fold(to_rgb(theme.primary), |accent, ground| {
            readable_on(accent, ground, pole(ground))
        });
    to_hsla(accent)
}

/// Black or white, whichever contrasts more with `ground`: an endpoint
/// that can always reach the readable ratio, which a theme's own
/// foreground cannot promise.
fn pole(ground: Rgb) -> Rgb {
    let black = Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };
    let white = Rgb {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
    if contrast_ratio(black, ground) >= contrast_ratio(white, ground) {
        black
    } else {
        white
    }
}

/// Paints a list row's state: the active fill and text when
/// `highlighted`, else the hover fill under the pointer.
///
/// The row must carry an id. gpui notifies the rendering view on a hover
/// transition only for an element with one (the `MouseMoveEvent`
/// listener in `gpui-pre-0.3.5/src/elements/div.rs` flips `hover_state`,
/// which only an identified element has); an id-less row still paints
/// the fill, but only on some unrelated repaint, so the hover lags the
/// pointer. The `Stateful` bound makes that a compile error.
pub fn paint_row(row: Stateful<Div>, paint: RowPaint, highlighted: bool) -> Stateful<Div> {
    if highlighted {
        row.bg(paint.active).text_color(paint.text)
    } else {
        row.hover(move |s| s.bg(paint.hover))
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

    /// Text and accent meet the readability floor in each tested state on
    /// every bundled theme.
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

    /// A table's match accent stays readable on the cursor row, under the
    /// pointer, and at rest, on every bundled theme.
    #[gpui::test]
    fn the_table_accent_is_readable_on_every_table_ground(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        let mut list_accent_short = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let accent = to_rgb(table_accent(theme));
                let list_accent = to_rgb(row_paint(theme).accent);
                let table = to_rgb(theme.table);
                let mut list_short = false;
                for (state, ground) in [
                    ("on the cursor row", over(theme.table_active, table)),
                    ("under the pointer", over(theme.table_hover, table)),
                    ("at rest", table),
                ] {
                    checked += 1;
                    if contrast_ratio(accent, ground) < READABLE_RATIO {
                        failures.push(format!("{name}: {state}"));
                    }
                    list_short |= contrast_ratio(list_accent, ground) < READABLE_RATIO;
                }
                list_accent_short += usize::from(list_short);
            });
        }
        assert!(checked >= 3 * 40, "the sweep saw {checked} checks");
        // The negative control: the list accent, floored on list grounds,
        // falls short on some table ground, which is why tables floor
        // their own.
        assert!(
            list_accent_short > 0,
            "RowPaint::accent reads on every table ground — the sweep has lost its teeth"
        );
        assert!(
            failures.is_empty(),
            "unreadable table accents:\n{}",
            failures.join("\n")
        );
    }

    /// Use `primary` on text-selection fill as a negative control: the
    /// readability check must reject it on multiple bundled themes.
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

    /// Every list row paints its state through [`paint_row`], whose
    /// `Stateful` bound is what keeps the hover prompt. A call site
    /// writing its own `.hover(.. paint.hover)` compiles on an id-less
    /// row and lags the pointer again, which only a source scan sees.
    #[test]
    fn every_row_hover_goes_through_paint_row() {
        let sources: [(&str, &str); 8] = [
            ("palette.rs", include_str!("../palette.rs")),
            ("asof_view.rs", include_str!("asof_view.rs")),
            ("dialog.rs", include_str!("dialog.rs")),
            ("keybindings_view.rs", include_str!("keybindings_view.rs")),
            (
                "objectdialog/render.rs",
                include_str!("objectdialog/render.rs"),
            ),
            ("picker.rs", include_str!("picker.rs")),
            ("settings_view.rs", include_str!("settings_view.rs")),
            ("stacklist.rs", include_str!("stacklist.rs")),
        ];
        let mut offenders = Vec::new();
        let mut calls = 0;
        for (name, text) in sources {
            calls += text.matches("paint_row(").count();
            for (at, _) in text.match_indices("paint.hover") {
                let line = text[..at].lines().count();
                offenders.push(format!("{name}:{line}"));
            }
        }
        assert!(
            calls >= 9,
            "the scan saw only {calls} paint_row calls — a file moved?"
        );
        assert!(
            offenders.is_empty(),
            "a list row paints its hover by hand instead of through paint_row:\n{}",
            offenders.join("\n")
        );
    }

    /// Check that fuzzy-match runs use [`RowPaint::accent`]. The theme
    /// sweep validates the shared painter; this source scan catches call
    /// sites that bypass it by passing a raw `primary` token.
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
            for needle in [
                "highlighted_text(",
                "highlighted_title(",
                "highlighted_runs(",
                "lead_label(",
            ] {
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
            calls >= 13,
            "the scan saw only {calls} highlight calls — a file moved?"
        );
        assert!(
            offenders.is_empty(),
            "highlight runs handed a raw primary instead of RowPaint::accent:\n{}",
            offenders.join("\n")
        );
    }

    /// Active and hover use distinct list-row token values on every bundled
    /// theme. This checks token identity, not perceptual separation.
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
