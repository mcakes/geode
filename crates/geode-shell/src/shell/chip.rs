//! The one door a status chip's colours come through — `pinned`,
//! `filtered`, `unscoped`, `AS OF`, the scope bar's contradiction chip,
//! the diagnostics tile's warning rows: any short state painted on a
//! translucent semantic tint, or in a semantic colour on the surface.
//!
//! It exists because the obvious token pairing is wrong and was wrong at
//! seven sites at once. `warning_foreground`/`danger_foreground` are the
//! tokens for text on a SOLID `warning`/`danger` fill, and at the pinned
//! gpui-component release both fall back to `primary_foreground` — the
//! background family — so over a 25% tint of their own colour they read
//! background-on-nearly-background: 1.13:1 on Default Light, 1.14:1 on
//! Everforest Light, under the 3:1 floor on 30 of 44 bundled themes
//! (measured 2026-09-19). The market-data panel found the same pairing on
//! its edited cells and fixed it there (`geode_marketdata::delegate::
//! cell_paint`); this module is that fix made the rule rather than the
//! instance. `foreground` is the one colour every theme author made
//! readable on their own background, and a translucent tint barely moves
//! that background, so it is the text on every tinted chip.
//!
//! The same token confusion reaches text with no fill: a warning ROW
//! painted in `warning_foreground` on the tile surface is the background
//! family on the background. A semantic colour on the surface is the
//! semantic colour itself (`theme.warning`), which
//! [`Tone::WarningText`] answers — kept in this door so the sweep test
//! below covers it on every theme rather than trusting each theme's
//! `warning` to clear its `background`, which ten bundled themes do not
//! (nine light themes at 1.77:1 to 2.63:1, plus Solarized Dark at 2.67:1
//! — measured 2026-09-19); those are floored the way Part
//! 2c floors a generated hue, lightness moved toward `foreground` with
//! hue and chroma kept.
//!
//! [`every_chip_tone_is_readable_on_every_bundled_theme`] is the test to
//! keep: a new tone added here without clearing the sweep cannot ship.

use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
use gpui::Hsla;
use gpui_component::Theme;

use super::colours::{over, to_hsla, to_rgb};

/// The tint a chip fill takes of its semantic colour: light enough that
/// `foreground` stays readable over it on every bundled theme, strong
/// enough to read as that colour. One value for every tone, so two chips
/// in one header are the same object at two colours rather than two
/// designs.
pub const FILL_ALPHA: f32 = 0.25;

/// What a chip means — its fill's semantic colour, or, for a run of text
/// carrying a semantic meaning with no fill of its own, the text colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// A state the trader should notice: a tile ignoring the shared
    /// scope, the frame held at a historical instant.
    Warning,
    /// A state that is wrong: a scope that can match no row.
    Danger,
    /// Warning-toned TEXT on the surface, no fill — a diagnostics row
    /// reporting a degraded source, a stale dataset time in a blotter
    /// header. `theme.warning` floored to the readable ratio against
    /// `background` where a theme ships it faint.
    WarningText,
    /// Danger-toned TEXT on the surface, no fill — a diagnostics row
    /// reporting a failed source, a blotter's query error. `theme.danger`
    /// floored the same way.
    DangerText,
    /// A state the trader chose and may want reminding of, carrying no
    /// hazard: a pinned grouping, a tile's own filter. The theme's
    /// `secondary` surface under `secondary_foreground` — a neutral pill,
    /// so the warning tone stays scarce enough to mean something (design
    /// guide: keep most badges neutral; a row of coloured badges is a
    /// missing hierarchy decision). `secondary`, not the dialogs' own
    /// `muted` chip pair, by measurement (2026-09-19): the text clears
    /// 3:1 on all 44 bundled themes over `secondary` and fails on 15 over
    /// `muted`, and the fill itself is visible (>1.15:1 against the
    /// background) on 34 themes for `secondary` against 11 for `muted`.
    /// The ten where `secondary` is faint (Asciinema 1.06:1, Tokyo Storm
    /// 1.08:1) read the pill as bare text — a theme-authoring matter, not
    /// worth a border every chip in the strip would have to reserve.
    Neutral,
}

/// A chip's colours, resolved: `fill` is `None` for a text-only tone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChipPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
}

/// The colours for a chip of `tone` on `theme` — the one place a chip's
/// text and fill are decided. Read per render (it is a few field reads
/// and, for `WarningText`, one contrast check); a caller painting many
/// per frame may hold the answer across a theme signature as the blotter
/// does for named colours.
pub fn chip_paint(theme: &Theme, tone: Tone) -> ChipPaint {
    match tone {
        Tone::Warning => ChipPaint {
            fill: Some(theme.warning.opacity(FILL_ALPHA)),
            text: theme.foreground,
        },
        Tone::Danger => ChipPaint {
            fill: Some(theme.danger.opacity(FILL_ALPHA)),
            text: theme.foreground,
        },
        Tone::WarningText => ChipPaint {
            fill: None,
            text: floored_text(theme, theme.warning),
        },
        Tone::DangerText => ChipPaint {
            fill: None,
            text: floored_text(theme, theme.danger),
        },
        Tone::Neutral => ChipPaint {
            fill: Some(theme.secondary),
            text: theme.secondary_foreground,
        },
    }
}

/// `colour` as text on the theme's own background: itself where it
/// already clears the floor, else moved in lightness toward `foreground`
/// until it does (`geode_core::colour::readable_on`, Part 2c §2.2's rule).
fn floored_text(theme: &Theme, colour: Hsla) -> Hsla {
    to_hsla(readable_on(
        to_rgb(colour),
        to_rgb(theme.background),
        to_rgb(theme.foreground),
    ))
}

/// The ground a chip's text lands on: its fill composited over the
/// window background, or the background itself for a text-only tone.
/// Public so a surface with its own fill beneath the chip (a table body
/// at its own alpha) can compose the same way in its own test.
pub fn ground(theme: &Theme, paint: &ChipPaint) -> Rgb {
    let background = to_rgb(theme.background);
    match paint.fill {
        Some(fill) => over(fill, background),
        None => background,
    }
}

/// Whether `paint` clears the readability floor on `theme` — what the
/// sweep test asserts and what a caller can ask before inventing a tone.
pub fn is_readable(theme: &Theme, paint: &ChipPaint) -> bool {
    contrast_ratio(to_rgb(paint.text), ground(theme, paint)) >= READABLE_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ActiveTheme as _;

    const TONES: [Tone; 5] = [
        Tone::Warning,
        Tone::Danger,
        Tone::WarningText,
        Tone::DangerText,
        Tone::Neutral,
    ];

    /// Every tone's text must clear the 3:1 floor over its own ground on
    /// EVERY bundled theme, with no exception list. The pairing this
    /// module replaced failed on 30 of 44.
    #[gpui::test]
    fn every_chip_tone_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                for tone in TONES {
                    checked += 1;
                    let paint = chip_paint(theme, tone);
                    let ratio = contrast_ratio(to_rgb(paint.text), ground(theme, &paint));
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {tone:?} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(
            checked >= 5 * 40,
            "the sweep saw {checked} checks — bundled themes missing?"
        );
        assert!(
            failures.is_empty(),
            "unreadable chips:\n{}",
            failures.join("\n")
        );
    }

    /// The pairing this module exists to retire must still FAIL the same
    /// sweep — otherwise the floor is not measuring what the module doc
    /// claims and a regression back to `warning_foreground` would pass.
    #[gpui::test]
    fn the_retired_pairing_still_fails_the_sweep(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failing_themes = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let retired = ChipPaint {
                    fill: Some(theme.warning.opacity(FILL_ALPHA)),
                    text: theme.warning_foreground,
                };
                if !is_readable(theme, &retired) {
                    failing_themes += 1;
                }
            });
        }
        assert!(
            failing_themes >= 20,
            "warning_foreground over the tint failed on only {failing_themes} themes — \
             the sweep has lost its teeth or the pinned tokens changed"
        );
    }

    /// A tinted tone's fill is its semantic colour at [`FILL_ALPHA`] and
    /// its text is the plain foreground; the text-only tone has no fill.
    #[gpui::test]
    fn tones_resolve_to_their_documented_tokens(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let warning = chip_paint(theme, Tone::Warning);
            assert_eq!(warning.fill, Some(theme.warning.opacity(FILL_ALPHA)));
            assert_eq!(warning.text, theme.foreground);
            let danger = chip_paint(theme, Tone::Danger);
            assert_eq!(danger.fill, Some(theme.danger.opacity(FILL_ALPHA)));
            assert_eq!(danger.text, theme.foreground);
            for tone in [Tone::WarningText, Tone::DangerText] {
                let text = chip_paint(theme, tone);
                assert_eq!(text.fill, None);
                assert!(is_readable(theme, &text));
            }
            let neutral = chip_paint(theme, Tone::Neutral);
            assert_eq!(neutral.fill, Some(theme.secondary));
            assert_eq!(neutral.text, theme.secondary_foreground);
        });
    }
}
