//! Shared colours for status chips and semantic text.
//!
//! Warning and danger chips keep their translucent semantic fills; neutral
//! chips keep `secondary`. Text starts with the paired theme token, then
//! clears 4.5:1 against the composited fill. Active chips also keep a 3:1
//! fill/title-bar floor so their on state remains visible.
//!
//! Use `chip_paint_on` for chips on chrome or a popover; `chip_paint` resolves
//! against the main background. Pointer states use the same text floor in
//! `control`, measured against their own hover and pressed fills.

use geode_core::colour::{Rgb, TEXT_READABLE_RATIO, contrast_ratio, readable_on, readable_text_on};
use gpui::Hsla;
use gpui_component::Theme;

use super::colours::{over, to_hsla, to_rgb};

/// A modest semantic tint shared by warning and danger chips. Text is
/// corrected independently against the tint composited on its actual surface.
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
    /// Info-toned TEXT or glyph on the surface, no fill — a tile header's
    /// visual-selection mode icon. `theme.info` floored the same way, so it
    /// reads apart from the warning-toned edit icon (pinned by the header's
    /// bundled-theme sweep).
    InfoText,
    /// A user-selected state with no hazard, such as a pinned grouping or a
    /// tile filter. Uses `secondary_foreground` on `secondary`, reserving
    /// warning colour for states that need attention. Text is adjusted only
    /// when the pair falls below 4.5:1 on its actual surface.
    Neutral,
    /// An on state the trader switched on that changes what the screen
    /// shows, such as a workspace's pinned frame: a solid `primary` whose
    /// lightness is moved toward `foreground` until it clears the readability
    /// floor against the title bar, carrying `primary_foreground` floored
    /// against that fill. Solid rather than tinted because it must read as on
    /// at a glance; `Neutral` is too faint for that on many themes, and several
    /// themes ship a `primary` too close to their title bar to use unadjusted.
    Active,
}

/// A chip's colours, resolved: `fill` is `None` for a text-only tone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChipPaint {
    pub fill: Option<Hsla>,
    pub text: Hsla,
}

/// Resolve a chip on the main background. Chrome and popover callers use
/// [`chip_paint_on`] so translucent fills are measured on their actual surface.
pub fn chip_paint(theme: &Theme, tone: Tone) -> ChipPaint {
    chip_paint_on(theme, tone, theme.background)
}

/// Resolve a chip's text and fill on `surface`, composited over the theme
/// background if the surface is translucent. Read per render so theme changes
/// are reflected immediately.
pub fn chip_paint_on(theme: &Theme, tone: Tone, surface: Hsla) -> ChipPaint {
    let surface = to_hsla(over(surface, to_rgb(theme.background)));
    let mut paint = match tone {
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
            text: theme.warning,
        },
        Tone::DangerText => ChipPaint {
            fill: None,
            text: theme.danger,
        },
        Tone::InfoText => ChipPaint {
            fill: None,
            text: theme.info,
        },
        Tone::Neutral => ChipPaint {
            fill: Some(theme.secondary),
            text: theme.secondary_foreground,
        },
        Tone::Active => {
            let background = to_rgb(theme.background);
            let bar = over(theme.title_bar, background);
            let fill = readable_on(
                over(theme.primary, background),
                bar,
                to_rgb(theme.foreground),
            );
            ChipPaint {
                fill: Some(to_hsla(fill)),
                text: theme.primary_foreground,
            }
        }
    };
    paint.text = text_on(paint.text, paint.fill, surface);
    paint
}

/// A neutral chip whose text carries `color`: an identity the trader
/// assigned (a link group), not a state. The fill is the neutral chip's;
/// the text is `color` floored to the small-text ratio against that fill on
/// `surface`. A generated hue arrives floored only for a graphic against
/// the background, which is short of what chip text on a fill needs.
pub fn colored(theme: &Theme, color: Hsla, surface: Hsla) -> ChipPaint {
    let neutral = chip_paint_on(theme, Tone::Neutral, surface);
    let surface = to_hsla(over(surface, to_rgb(theme.background)));
    ChipPaint {
        fill: neutral.fill,
        text: text_on(color, neutral.fill, surface),
    }
}

/// Text for an application-owned chip or bare label on an opaque surface.
/// Keep the original token (including its alpha) when it already clears.
pub(crate) fn text_on(text: Hsla, fill: Option<Hsla>, surface: Hsla) -> Hsla {
    let ground = fill.map_or_else(|| to_rgb(surface), |fill| over(fill, to_rgb(surface)));
    let ink = over(text, ground);
    if contrast_ratio(ink, ground) >= TEXT_READABLE_RATIO {
        text
    } else {
        to_hsla(readable_text_on(ink, ground))
    }
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

/// Whether `paint` clears the text floor on the main theme background.
/// For another surface, measure against the fill composited on that surface.
pub fn is_readable(theme: &Theme, paint: &ChipPaint) -> bool {
    contrast_ratio(over(paint.text, ground(theme, paint)), ground(theme, paint))
        >= TEXT_READABLE_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::READABLE_RATIO;
    use gpui_component::ActiveTheme as _;

    const TONES: [Tone; 7] = [
        Tone::Warning,
        Tone::Danger,
        Tone::WarningText,
        Tone::DangerText,
        Tone::InfoText,
        Tone::Neutral,
        Tone::Active,
    ];

    /// Every tone's text must meet the readability floor against its
    /// composited ground on every bundled theme.
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
                for (surface_name, surface) in [
                    ("background", theme.background),
                    ("title bar", theme.title_bar),
                    ("status bar", theme.status_bar),
                    ("popover", theme.popover),
                    ("table", theme.table),
                ] {
                    let surface_rgb = over(surface, to_rgb(theme.background));
                    for tone in TONES {
                        checked += 1;
                        let paint = chip_paint_on(theme, tone, surface);
                        let ground = paint
                            .fill
                            .map_or(surface_rgb, |fill| over(fill, surface_rgb));
                        let ratio = contrast_ratio(over(paint.text, ground), ground);
                        if ratio < TEXT_READABLE_RATIO {
                            failures.push(format!(
                                "{name}: {tone:?} on {surface_name} at {ratio:.2}:1"
                            ));
                        }
                        let states =
                            crate::shell::control::for_chip(theme, &paint, to_hsla(surface_rgb));
                        assert!(
                            crate::shell::control::is_readable(&states),
                            "{name}: {tone:?} on {surface_name}"
                        );
                    }
                    // Scope and expression chips use muted tokens rather than
                    // a semantic tone, but have the same small-text requirement.
                    let text = text_on(
                        theme.muted_foreground,
                        Some(theme.muted),
                        to_hsla(surface_rgb),
                    );
                    let ground = over(theme.muted, surface_rgb);
                    assert!(
                        contrast_ratio(over(text, ground), ground) >= TEXT_READABLE_RATIO,
                        "{name}: muted chip on {surface_name}"
                    );
                }
            });
        }
        assert!(
            checked >= TONES.len() * 5 * 40,
            "the sweep saw {checked} checks — bundled themes missing?"
        );
        assert!(
            failures.is_empty(),
            "unreadable chips:\n{}",
            failures.join("\n")
        );
    }

    /// An `Active` chip marks an on state that must read at a glance, so its
    /// solid fill must stand apart from the title bar it sits on by the
    /// non-text contrast floor (3:1) on every bundled theme.
    #[gpui::test]
    fn an_active_chip_stands_out_from_the_title_bar_on_every_bundled_theme(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let paint = chip_paint(theme, Tone::Active);
                let fill = over(paint.fill.unwrap(), to_rgb(theme.background));
                let ratio = contrast_ratio(fill, over(theme.title_bar, to_rgb(theme.background)));
                if ratio < READABLE_RATIO {
                    failures.push(format!("{name}: {ratio:.2}:1"));
                }
            });
        }
        assert!(
            failures.is_empty(),
            "faint active chips:\n{}",
            failures.join("\n")
        );
    }

    /// Use `warning_foreground` over a translucent warning fill as a negative
    /// control: the sweep must reject this unreadable pairing.
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

    /// A group's color is the text of its header chip. The generated hue
    /// is floored for a graphic (3:1) against the background, not for small
    /// text on the chip's fill, so `colored` must bring it to the text
    /// floor on every bundled theme.
    #[gpui::test]
    fn a_colored_chip_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        use geode_core::link::Group;
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                for group in Group::ALL {
                    checked += 1;
                    let color = crate::link::group_color(theme, group);
                    let paint = colored(theme, color, theme.background);
                    assert_eq!(
                        paint.fill,
                        chip_paint(theme, Tone::Neutral).fill,
                        "{name}: the neutral chip's fill"
                    );
                    if !is_readable(theme, &paint) {
                        failures.push(format!("{name}: group {}", group.letter()));
                    }
                }
            });
        }
        assert!(
            checked >= 4 * 40,
            "the sweep saw {checked} checks: bundled themes missing?"
        );
        assert!(
            failures.is_empty(),
            "unreadable group chips:\n{}",
            failures.join("\n")
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
            for tone in [Tone::WarningText, Tone::DangerText, Tone::InfoText] {
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
