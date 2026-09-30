//! Prepared grid-row text colours, recomputed on theme changes.
//! CellState selects own, muted stale/inherited/mixed, or failure text. Package rows
//! share the line palette: the tree column carries their structure. Two rows have a
//! ground of their own and so a [`RowPalette`] floored on it: a grouping row
//! (`Paints::group`) and a package leg (`Paints::leg`, a faint tint marking the row as
//! inside its package, [`leg_ground`]). A package's template chip takes the neutral
//! chip pair (`chip_fill`, `chip_text`). GridModel remains independent of the theme.
//!
//! Row text is adjusted against its base background and the table's hover and selection
//! backgrounds, which replace that base. The bundled-theme test checks these prepared
//! colours on each background. Menu colors are `geode_tile::menu::MenuPaint`'s.
//!
//! Adjust toward whichever of black or white has greater contrast with the background.
//! Using the original text colour as the adjustment endpoint would fail when that
//! colour already matches its background. The multi-background helper is bounded; it
//! does not guarantee success for arbitrary combinations of backgrounds that require
//! opposite endpoints.

use crate::core::columns::CellState;
use geode_core::colour::oklab::relative_luminance;
use geode_core::colour::{READABLE_RATIO, Rgb, Sign, contrast_ratio, readable_on};
use geode_core::view::Colour;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::colours::{over, to_hsla, to_rgb};
use gpui::Hsla;
use gpui_component::Theme;

const BLACK: Rgb = Rgb {
    r: 0.0,
    g: 0.0,
    b: 0.0,
};
const WHITE: Rgb = Rgb {
    r: 1.0,
    g: 1.0,
    b: 1.0,
};

/// Whichever of black or white contrasts more against `bg`.
fn pole(bg: Rgb) -> Rgb {
    if contrast_ratio(BLACK, bg) >= contrast_ratio(WHITE, bg) {
        BLACK
    } else {
        WHITE
    }
}

/// Floor `c` to `READABLE_RATIO` against `bg`, moving toward `bg`'s own
/// pole rather than toward `c` (or any other fixed anchor) — see the
/// module doc for why a fixed anchor can leave the bisection nothing to
/// move toward.
fn floor_toward_pole(c: Hsla, bg: Rgb) -> Hsla {
    to_hsla(readable_on(to_rgb(c), bg, pole(bg)))
}

/// Adjust text against the lowest-contrast failing background on each pass, for at most
/// grounds.len() passes. Bundled-theme tests check all resulting background pairs;
/// incompatible backgrounds need not converge within this bound.
fn floor_on_all(c: Hsla, grounds: &[Rgb]) -> Hsla {
    let mut c = c;
    for _ in 0..grounds.len() {
        let Some(worst) = grounds
            .iter()
            .copied()
            .filter(|g| contrast_ratio(to_rgb(c), *g) < READABLE_RATIO)
            .min_by(|a, b| contrast_ratio(to_rgb(c), *a).total_cmp(&contrast_ratio(to_rgb(c), *b)))
        else {
            break;
        };
        c = floor_toward_pole(c, worst);
    }
    c
}

// `floor_on_all` floors to `READABLE_RATIO`; the connector needs at least
// the non-text floor from it.
const _: () = assert!(geode_core::colour::NON_TEXT_RATIO <= READABLE_RATIO);

/// The least luminance contrast between a package leg's ground and each
/// ground it must read apart from: the line ground beside it, the hover
/// ground that replaces it, and a grouping row's ground. The
/// measure is the one `geode_shell::shell::control`'s distinctness sweep
/// uses (the WCAG ratio); its floor there, `DISTINCT_RATIO` (1.10), is
/// for an interaction state that must read at a glance, which a rest
/// ground marking membership must not compete with. 1.04 is the faintest
/// step that still reads across a full-width row: the bundled themes' own
/// stripe tokens (`table_even`) run from about 1.02 to 1.22 against their
/// table ground, and the ones under 1.04 are the stripes that vanish.
pub const LEG_TINT_RATIO: f32 = 1.04;

/// How finely [`leg_ground`]'s fallback walks from the table ground: the
/// opacity step of each blend. Fine enough that the first blend to clear
/// [`LEG_TINT_RATIO`] overshoots it by well under a hundredth.
const LEG_TINT_STEP: f32 = 0.002;

/// The opaque grounds a leg's ground is measured against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TableGrounds {
    /// The table's own ground: a bare line's and a package row's.
    pub line: Rgb,
    /// Hover, which replaces a row's own ground under the pointer.
    pub hover: Rgb,
    /// A grouping row's.
    pub group: Rgb,
}

/// Whether `leg` is a usable leg ground: at least [`LEG_TINT_RATIO`]
/// from the line ground (else the leg is unmarked), from hover (which
/// replaces it: equal, and hovering a leg would show nothing) and from
/// the group ground (equal, and a leg reads as a group row); and fainter
/// than hover from the line ground (hover is the louder signal; a tint
/// louder than it reads as a band).
///
/// The selected ground is not held apart: on several bundled light
/// themes it sits so near the line ground, and hover so near both, that
/// no tint fainter than hover can clear it. The selected row is the
/// cursor row, which also carries the cursor cell's border and the
/// gutter's own paint; hover has no other cue.
pub(crate) fn leg_ground_clears(leg: Rgb, g: &TableGrounds) -> bool {
    let from_line = contrast_ratio(leg, g.line);
    from_line >= LEG_TINT_RATIO
        && from_line < contrast_ratio(g.hover, g.line)
        && [g.hover, g.group]
            .iter()
            .all(|other| contrast_ratio(leg, *other) >= LEG_TINT_RATIO)
}

/// A package leg's ground: the theme's stripe token (`stripe`, already
/// composited over the table ground) where it clears
/// [`leg_ground_clears`]; otherwise the faintest blend of the line ground
/// toward the foreground, or away from it toward the opposite pole, that
/// does. Some themes ship a stripe token equal to or barely off the table
/// ground, one as loud as hover, or one equal to their group ground; each
/// would leave the leg unmarked, hover invisible on it, or the leg
/// reading as a group row.
/// Where no blend clears (no bundled theme), the faintest blend toward
/// the foreground distinct from the line ground.
pub(crate) fn leg_ground(stripe: Rgb, g: &TableGrounds, foreground: Rgb) -> Rgb {
    if leg_ground_clears(stripe, g) {
        return stripe;
    }
    let away = if relative_luminance(foreground) > relative_luminance(g.line) {
        BLACK
    } else {
        WHITE
    };
    let blends = |toward: Rgb| {
        (1..=(1.0 / LEG_TINT_STEP) as usize)
            .map(move |i| over(to_hsla(toward).opacity(i as f32 * LEG_TINT_STEP), g.line))
    };
    let faintest = |toward: Rgb| blends(toward).find(|c| leg_ground_clears(*c, g));
    match (faintest(foreground), faintest(away)) {
        (Some(a), Some(b)) => {
            if contrast_ratio(a, g.line) <= contrast_ratio(b, g.line) {
                a
            } else {
                b
            }
        }
        (Some(c), None) | (None, Some(c)) => c,
        (None, None) => blends(foreground)
            .find(|c| contrast_ratio(*c, g.line) >= LEG_TINT_RATIO)
            .unwrap_or(foreground),
    }
}

/// The text paints of a row with a ground of its own (a grouping row, a
/// package leg), each floored on that ground and on the hover and
/// selected grounds that replace it: the state paints (own, muted
/// `mixed` / stale / inherited, danger), and a `sign` column's bearish
/// and bullish, which on a line are the theme's chart tokens as they are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowPalette {
    /// Opaque: the row's own ground.
    pub ground: Hsla,
    pub own: Hsla,
    pub muted: Hsla,
    pub danger: Hsla,
    pub bearish: Hsla,
    pub bullish: Hsla,
    /// `ground`, hover and selected: what the row's text is floored on.
    grounds: [Rgb; 3],
}

impl RowPalette {
    fn on(theme: &Theme, danger: Hsla, ground: Rgb, [hover, selected]: [Rgb; 2]) -> RowPalette {
        let grounds = [ground, hover, selected];
        RowPalette {
            ground: to_hsla(ground),
            own: floor_on_all(theme.foreground, &grounds),
            muted: floor_on_all(theme.muted_foreground, &grounds),
            danger: floor_on_all(danger, &grounds),
            bearish: floor_on_all(theme.chart_bearish, &grounds),
            bullish: floor_on_all(theme.chart_bullish, &grounds),
            grounds,
        }
    }

    /// `c` floored on the row's three grounds, as the palette is: for a
    /// colour the palette cannot prepare (a view's named column colour).
    pub fn floor(&self, c: Hsla) -> Hsla {
        floor_on_all(c, &self.grounds)
    }

    /// [`Paints::text`] on this row's ground.
    pub fn text(&self, state: CellState) -> Hsla {
        match state {
            CellState::Stale | CellState::Inherited | CellState::Mixed => self.muted,
            CellState::Failed => self.danger,
            CellState::Own | CellState::Blank => self.own,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paints {
    pub own: Hsla,
    pub muted: Hsla,
    pub danger: Hsla,
    /// Opaque: `table_hover` over the table ground — what the table
    /// paints on a row under the pointer, replacing the row's own ground.
    /// The chevron's only interactive ground.
    pub row_hover: Hsla,
    /// A package's template chip: the neutral chip's fill (`secondary`,
    /// possibly translucent) and its text, floored on that fill composited
    /// over each of the row's three grounds (own, hover, selected).
    pub chip_fill: Hsla,
    pub chip_text: Hsla,
    /// A leg's drawn connector lines: the theme's structural `border`
    /// token, floored to the non-text contrast floor (`NON_TEXT_RATIO`)
    /// on the leg ground and on the hover and selected grounds. Legs
    /// never sit on the group ground (a group row is its own row).
    pub connector: Hsla,
    /// The expiry date field's active segment text on its `primary` fill,
    /// and a mid-typing segment's on its `accent` fill. The field paints
    /// on the cursor row, a bare line or a leg, whose ground may be the
    /// line's, the leg's, hover or selected: each fill is composited over
    /// all four (a translucent fill reads differently on each) and the
    /// text floored on every one.
    pub date_active_text: Hsla,
    pub date_typing_text: Hsla,
    /// A grouping row's palette. Its ground is opaque `secondary` over
    /// the table ground (the blotter's group rows have none; spec §9
    /// assumed one, so the pricer takes this one). Hover and selection
    /// replace it as they replace a line's.
    pub group: RowPalette,
    /// A package leg's palette, on its faint tint ([`leg_ground`]): the
    /// ground that marks a row as inside its package. Bare lines and
    /// package rows keep the table's ground.
    pub leg: RowPalette,
}

impl Paints {
    pub fn derive(theme: &Theme) -> Paints {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let danger = chip_paint(theme, Tone::DangerText).text;
        // A row's text reads on its own ground and on the two the table
        // paints over it (hover, selected), which replace it.
        let [hover, selected] = Self::row_grounds(theme);
        let line = [ground, hover, selected];
        let chip = chip_paint(theme, Tone::Neutral);
        // `Tone::Neutral` fills with `secondary`; fall back to it rather
        // than panic on a theme change.
        let chip_fill = chip.fill.unwrap_or(theme.secondary);
        let chip_grounds = line.map(|g| over(chip_fill, g));
        let group_ground = over(theme.secondary, ground);
        let leg_ground = leg_ground(
            over(*theme.tokens.table_even, ground),
            &TableGrounds {
                line: ground,
                hover,
                group: group_ground,
            },
            to_rgb(theme.foreground),
        );
        let leg = [leg_ground, hover, selected];
        // The date field sits on a bare line or a leg.
        let editable = [ground, leg_ground, hover, selected];
        Paints {
            group: RowPalette::on(theme, danger, group_ground, [hover, selected]),
            leg: RowPalette::on(theme, danger, leg_ground, [hover, selected]),
            own: floor_on_all(theme.foreground, &line),
            muted: floor_on_all(theme.muted_foreground, &line),
            danger: floor_on_all(danger, &line),
            row_hover: to_hsla(hover),
            chip_fill,
            chip_text: floor_on_all(chip.text, &chip_grounds),
            // Opaque first: `border` may be translucent, and the floor
            // measures an opaque colour.
            connector: floor_on_all(to_hsla(over(theme.border, leg_ground)), &leg),
            date_active_text: floor_on_all(
                theme.primary_foreground,
                &editable.map(|g| over(theme.primary, g)),
            ),
            date_typing_text: floor_on_all(
                theme.accent_foreground,
                &editable.map(|g| over(theme.accent, g)),
            ),
        }
    }

    /// Table hover and selection backgrounds composited over the table base. Selection
    /// uses table_active when list.active_highlight is enabled, otherwise accent.
    pub(crate) fn row_grounds(theme: &Theme) -> [Rgb; 2] {
        let ground: Rgb = over(theme.table, to_rgb(theme.background));
        let selected = if theme.list.active_highlight {
            *theme.tokens.table_active
        } else {
            *theme.tokens.accent
        };
        [
            over(*theme.tokens.table_hover, ground),
            over(selected, ground),
        ]
    }

    pub fn text(&self, state: CellState) -> Hsla {
        match state {
            CellState::Stale | CellState::Inherited | CellState::Mixed => self.muted,
            CellState::Failed => self.danger,
            CellState::Own | CellState::Blank => self.own,
        }
    }
}

/// Which colour a painted cell takes, decided from the column's declared
/// colour, the cell's state and its value's sign. State colours (muted
/// stale, danger failed) win over any column colour: a wrong-looking
/// number must never read as a healthy one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellColour {
    /// The state paint (`Paints::text`).
    State,
    Bearish,
    Bullish,
    /// The column's named colour, in the variant for this sign (a text
    /// cell's `Zero` is the base).
    Named(Sign),
}

pub(crate) fn cell_colour(colour: &Colour, state: CellState, sign: Option<Sign>) -> CellColour {
    if !matches!(state, CellState::Own) {
        return CellColour::State;
    }
    match (colour, sign) {
        (Colour::Sign, Some(Sign::Negative)) => CellColour::Bearish,
        (Colour::Sign, Some(Sign::Positive)) => CellColour::Bullish,
        (Colour::Named(_), sign) => CellColour::Named(sign.unwrap_or(Sign::Zero)),
        _ => CellColour::State,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::colour::{NON_TEXT_RATIO, READABLE_RATIO, contrast_ratio};
    use gpui_component::{ActiveTheme as _, Theme};

    /// Check each prepared text colour against its painted backgrounds for every
    /// bundled theme: each row palette on its own ground and on the row hover and
    /// selection grounds that replace it, the chip and the date segment fills
    /// composited over every ground they can land on, and a leg's connector.
    #[gpui::test]
    fn every_pricer_paint_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = Paints::derive(theme);
                let ground = over(theme.table, to_rgb(theme.background));
                let [hover, selected] = Paints::row_grounds(theme);
                let replaced = [("hover", hover), ("selected", selected)];
                let line = [("ground", ground), replaced[0], replaced[1]];
                let (leg, group) = (to_rgb(p.leg.ground), to_rgb(p.group.ground));
                let mut pairs: Vec<(String, Hsla, Rgb)> = Vec::new();
                for (which, bg) in line {
                    for (label, text) in [("own", p.own), ("muted", p.muted), ("danger", p.danger)]
                    {
                        pairs.push((format!("{label} on {which}"), text, bg));
                    }
                    // A package's template chip: its text on its fill.
                    pairs.push((
                        format!("chip on {which}"),
                        p.chip_text,
                        over(p.chip_fill, bg),
                    ));
                }
                // The date field sits on a bare line or a leg.
                for (which, bg) in [("ground", ground), ("leg", leg), replaced[0], replaced[1]] {
                    for (label, text, fill) in [
                        ("date active", p.date_active_text, theme.primary),
                        ("date typing", p.date_typing_text, theme.accent),
                    ] {
                        pairs.push((format!("{label} on {which}"), text, over(fill, bg)));
                    }
                }
                // A grouping row's and a leg's palette on its own ground
                // and on hover and selected, which replace it.
                for (row, palette, own_ground) in [("group", p.group, group), ("leg", p.leg, leg)] {
                    for (which, bg) in [("own ground", own_ground), replaced[0], replaced[1]] {
                        for (label, text) in [
                            ("own", palette.own),
                            ("muted", palette.muted),
                            ("danger", palette.danger),
                            ("bearish", palette.bearish),
                            ("bullish", palette.bullish),
                        ] {
                            pairs.push((format!("{row} {label} on {which}"), text, bg));
                        }
                    }
                }
                for (label, text, bg) in pairs {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(text), bg);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {label} at {ratio:.2}:1"));
                    }
                }
                // A leg's connector lines, a non-text graphic, on the
                // leg's own ground, hover and selected. It must be
                // opaque: the ratio is measured as painted.
                for (which, bg) in [("leg ground", leg), replaced[0], replaced[1]] {
                    checked += 1;
                    let ratio = contrast_ratio(to_rgb(p.connector), bg);
                    if p.connector.a < 1.0 || ratio < NON_TEXT_RATIO {
                        failures.push(format!(
                            "{name}: connector on {which} at {ratio:.2}:1 (alpha {})",
                            p.connector.a
                        ));
                    }
                }
            });
        }
        // Fifty-three pairs a theme (line paints and chip on three
        // grounds, the date field's two on four, each row palette's five
        // on three, the connector on three) over at least forty themes.
        assert!(
            checked >= 53 * 40,
            "every bundled theme was swept ({checked})"
        );
        assert!(
            failures.is_empty(),
            "unreadable pricer paints:\n{}",
            failures.join("\n")
        );
    }

    /// A leg's ground marks it as inside its package on every bundled
    /// theme: at least [`LEG_TINT_RATIO`] from the line ground, fainter
    /// than hover, and at least [`LEG_TINT_RATIO`] from hover and the
    /// group ground, so hovering a leg still shows and a leg never reads
    /// as a group row. Some themes' stripe token
    /// fails this (it equals the table ground, is as loud as hover, or
    /// equals another ground): the fallback must supply one, and the count
    /// of such themes pins that it is needed.
    #[gpui::test]
    fn every_leg_ground_is_a_faint_distinct_tint_on_every_bundled_theme(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let (service, _) = geode_shell::theme::load_bundled();
        let (mut failures, mut fallbacks, mut themes) = (Vec::new(), 0, 0);
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = Paints::derive(theme);
                let line = over(theme.table, to_rgb(theme.background));
                let [hover, _] = Paints::row_grounds(theme);
                let group = to_rgb(p.group.ground);
                let leg = to_rgb(p.leg.ground);
                themes += 1;
                let stripe = over(*theme.tokens.table_even, line);
                let apart = [("hover", hover), ("group", group)];
                if contrast_ratio(stripe, line) < LEG_TINT_RATIO
                    || contrast_ratio(stripe, line) >= contrast_ratio(hover, line)
                    || apart
                        .iter()
                        .any(|(_, g)| contrast_ratio(stripe, *g) < LEG_TINT_RATIO)
                {
                    fallbacks += 1;
                }
                let from_line = contrast_ratio(leg, line);
                let hover_ratio = contrast_ratio(hover, line);
                if from_line < LEG_TINT_RATIO || from_line >= hover_ratio {
                    failures.push(format!(
                        "{name}: leg {from_line:.3}:1 from the line ground \
                         (hover {hover_ratio:.3}:1)"
                    ));
                }
                for (which, g) in apart {
                    let r = contrast_ratio(leg, g);
                    if r < LEG_TINT_RATIO {
                        failures.push(format!("{name}: leg {r:.3}:1 from {which}"));
                    }
                }
            });
        }
        assert!(themes >= 40, "every bundled theme was swept ({themes})");
        assert!(
            failures.is_empty(),
            "indistinct leg grounds:\n{}",
            failures.join("\n")
        );
        assert!(
            fallbacks > 0,
            "no bundled theme needs the fallback — the pinned tokens changed"
        );
    }

    /// The fallback's own contract, on grounds a theme could ship: a
    /// stripe equal to the line ground, to hover or to the group ground
    /// gives way to the faintest blend that clears, on light
    /// and dark, and in the direction away from the foreground when hover
    /// sits too near the line ground for a tint between them.
    #[test]
    fn an_indistinct_or_loud_stripe_falls_back_to_the_faintest_clearing_tint() {
        let grey = |v: f32| Rgb { r: v, g: v, b: v };
        let grounds = |line: f32, hover: f32, group: f32| TableGrounds {
            line: grey(line),
            hover: grey(hover),
            group: grey(group),
        };
        for (g, fg) in [
            (grounds(1.0, 0.9, 0.93), grey(0.1)),
            (grounds(0.12, 0.2, 0.18), grey(0.9)),
            (grounds(0.12, 0.16, 0.3), grey(0.9)),
        ] {
            for stripe in [g.line, g.hover, g.group] {
                let leg = leg_ground(stripe, &g, fg);
                assert!(leg_ground_clears(leg, &g), "{leg:?} clears on {g:?}");
                let r = contrast_ratio(leg, g.line);
                assert!(r < LEG_TINT_RATIO + 0.02, "faint: {r:.3}:1 on {g:?}");
            }
        }
        let g = grounds(1.0, 0.8, 0.75);
        let stripe = over(to_hsla(grey(0.0)).opacity(0.03), g.line);
        assert!(leg_ground_clears(stripe, &g), "fixture clears");
        assert_eq!(
            leg_ground(stripe, &g, grey(0.1)),
            stripe,
            "a clearing stripe is used as it is"
        );
    }

    /// A color equal to its background must still reach READABLE_RATIO.
    /// Moving toward the higher-contrast black or white pole supplies a
    /// distinct endpoint for bisection on both light and dark backgrounds.
    #[test]
    fn the_floor_moves_a_colour_equal_to_its_ground_to_the_readable_ratio() {
        for level in [0.2, 0.5, 0.8] {
            let ground = Rgb {
                r: level,
                g: level,
                b: level,
            };
            let floored = floor_toward_pole(to_hsla(ground), ground);
            assert!(
                contrast_ratio(to_rgb(floored), ground) >= READABLE_RATIO,
                "a colour equal to a {level} grey ground must still clear \
                 {READABLE_RATIO}:1 once floored"
            );
        }
    }

    #[test]
    fn colour_applies_only_to_an_own_numeric_cell() {
        let (neg, pos) = (Some(Sign::Negative), Some(Sign::Positive));
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Own, neg),
            CellColour::Bearish
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Own, pos),
            CellColour::Bullish
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Own, Some(Sign::Zero)),
            CellColour::State,
            "zero has no sign"
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Stale, neg),
            CellColour::State,
            "stale stays muted"
        );
        assert_eq!(
            cell_colour(&Colour::Sign, CellState::Failed, neg),
            CellColour::State,
            "failed stays danger"
        );
        assert_eq!(
            cell_colour(&Colour::None, CellState::Own, neg),
            CellColour::State
        );
        assert_eq!(
            cell_colour(&Colour::Named("rose".into()), CellState::Own, neg),
            CellColour::Named(Sign::Negative)
        );
        assert_eq!(
            cell_colour(&Colour::Named("rose".into()), CellState::Own, None),
            CellColour::Named(Sign::Zero),
            "text in a named column takes the base"
        );
    }

    #[gpui::test]
    fn a_state_picks_its_colour(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let p = Paints::derive(cx.theme());
            assert_eq!(p.text(CellState::Own), p.own);
            assert_eq!(p.text(CellState::Blank), p.own);
            assert_eq!(p.text(CellState::Stale), p.muted);
            assert_eq!(p.text(CellState::Inherited), p.muted);
            assert_eq!(p.text(CellState::Failed), p.danger);
            assert_eq!(p.text(CellState::Mixed), p.muted, "`mixed` is muted");
            for palette in [p.group, p.leg] {
                assert_eq!(palette.text(CellState::Own), palette.own);
                assert_eq!(palette.text(CellState::Blank), palette.own);
                assert_eq!(palette.text(CellState::Mixed), palette.muted);
                assert_eq!(palette.text(CellState::Stale), palette.muted);
                assert_eq!(palette.text(CellState::Inherited), palette.muted);
                assert_eq!(palette.text(CellState::Failed), palette.danger);
            }
        });
    }
}
