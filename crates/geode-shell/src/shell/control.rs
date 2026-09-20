//! The one door a clickable control's pointer states come through — the
//! scope bar's chips and their `×`, the sidebar's discs and gear, the
//! status bar's diagnostics segment, a dialog's steppable value chips
//! and ticks, the blotter's chevron, the market-data `⋯` button: any
//! element with a mouse listener that is not a list row (those take
//! [`super::listrow::row_paint`]) and not a gpui-component `Button`
//! (which paints its own).
//!
//! It exists because the design guide asks two things of every control
//! that the 2026-09-19 audit delivered for only one kind of element. The
//! guide's pointer convention keeps the ARROW cursor over buttons, tabs
//! and chips — the pointing hand is for links — so the audit removed the
//! six `cursor_pointer()` sites; but its interaction-state table also
//! owes every control a hover state ("subtle pointer feedback, never the
//! only cue") and a pressed state, and the audit added a hover fill to
//! list rows alone. Every other clickable element was left with no
//! pointer feedback at all, which read as "nothing here is clickable"
//! (user finding, 2026-09-19). This module is the second half: one
//! [`ControlPaint`] per control, applied through
//! [`PointerStates::pointer_states`] as gpui's `hover` and `active`
//! refinements — `active` is the pressed state.
//!
//! **The fills.** The first cut borrowed gpui-component's own `Button`
//! tokens outright — `secondary_hover`/`secondary_active` for a chip
//! with a fill of its own, the ghost variant's `accent` for a bare glyph
//! — and the review measured them: on 6 of 44 bundled themes
//! `secondary_hover` IS the rest fill (the token falls back to
//! `secondary` when a theme leaves it unset, and Fahrenheit's equals
//! `muted`), and on 28 the two are within 1.10:1 of each other — a hover
//! the eye cannot find, the very defect this module answers. So a state's
//! fill is the component's token where that token is visibly distinct
//! from what the control looked like at rest ([`DISTINCT_RATIO`]), and
//! otherwise the rest fill stepped toward `foreground` until it is; the
//! pressed fill must be distinct from rest the same way and a smaller
//! step ([`PRESSED_STEP`]) from hover, so the press reads as a press.
//! `accent` over the window background clears 1.09:1 on every bundled
//! theme, so a bare control's hover is almost always the component's own.
//!
//! **The ground.** Every comparison and every readability check is made
//! over the SURFACE the control sits on — the scope chips on
//! `title_bar`, the rail on `sidebar`, the status bar on `status_bar`,
//! a dialog's chips and ticks on `popover`, a blotter cell on `table` —
//! never the window background: 16 bundled `secondary_hover` values are
//! translucent, and a text floored to exactly 3:1 over `background` lands
//! under it on the title bar of six themes (review finding). The same
//! reason `listrow::row_paint` grounds on `popover`.
//!
//! **The text.** A control keeps its OWN text on every state — a ticked
//! tick stays `success`, the diagnostics segment stays `warning` — floored
//! to 3:1 over that state's fill (`geode_core::colour::readable_on`, Part
//! 2c §2.2's rule); the floor is not decoration: `muted_foreground` over
//! `secondary_hover` is under 3:1 on 16 bundled themes unfloored
//! (Catppuccin Latte 1.93:1), over `secondary_active` on 19, `warning`
//! over the pressed fill on 15 (measured 2026-09-19).
//!
//! **The key.** [`ControlInputs`] is every colour the derivation reads,
//! `Copy` and `PartialEq`, so a per-row painter (the blotter's chevron)
//! memoises one [`ControlPaint`] behind it and pays a few `Hsla` compares
//! per row rather than the OKLab bisection `readable_on` runs when the
//! floor bites — the delegate's documented steady path ("no `Hsla ->
//! Rgb` conversion at all") holds.
//!
//! Two gpui facts the trait depends on, verified against the pinned
//! `gpui-pre-0.3.5/src/elements/div.rs`: a STATELESS element's `.hover()`
//! never `notify`s on a hover transition (only an element with
//! `hover_state` in its element state does), so the `Stateful` bound is
//! what makes the hover repaint at all, not just what `active` needs; and
//! `hover` carries a debug assertion that no hover style was set before,
//! so `pointer_states` composes with no other `.hover()` on the same
//! element. The two sweeps below have no exception list: a new rest kind
//! or a new (surface, text) pairing that fails either cannot ship.

use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
use gpui::prelude::*;
use gpui::{Hsla, StatefulInteractiveElement};
use gpui_component::Theme;

use super::colours::{over, to_hsla, to_rgb};

/// The least luminance ratio between a state's fill and the control's
/// rest fill for the state to count as visible. Calibrated against the
/// bundled themes' own `secondary → secondary_hover` deltas: the
/// deliberate ones sit at 1.12–1.78 (Nord 1.17, TradingView Dark 1.17,
/// Catppuccin Mocha 1.78); the accidental ones, where the token fell
/// back to the rest fill, at 1.00–1.08.
pub const DISTINCT_RATIO: f32 = 1.10;

/// The least ratio between the pressed and the hover fill: a press
/// darkens or lightens the hovered control a visible notch further.
pub const PRESSED_STEP: f32 = 1.05;

/// The one way a [`ControlPaint`] reaches an element: hover and pressed
/// as gpui style refinements. On a `Stateful` element only (see the
/// module doc): `active` needs the element identity gpui keeps pressed
/// state under, which is also what a tooltip needs, so every clickable
/// control in this crate already carries an `.id(..)`. A child that pins
/// its own `text_color` (an `Icon::new(..).text_color(fg)`) will not
/// recolour on hover: let it inherit the control's text instead. Do not
/// chain another `.hover()` on the same element — gpui asserts against
/// a second hover style in debug builds.
pub trait PointerStates: StatefulInteractiveElement + Styled + Sized {
    fn pointer_states(self, paint: ControlPaint) -> Self {
        self.hover(move |s| s.bg(paint.hover).text_color(paint.hover_text))
            .active(move |s| s.bg(paint.pressed).text_color(paint.pressed_text))
    }
}

impl<E: StatefulInteractiveElement + Styled> PointerStates for E {}

/// What the control looks like at rest — which decides the component
/// token its hover borrows and what "distinct from rest" is measured
/// against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Rest {
    /// A chip with a fill of its own (`muted`, `secondary`): hover
    /// borrows `secondary_hover`, the secondary button's.
    Filled(Hsla),
    /// A glyph or label painted straight on the surface: hover borrows
    /// `accent`, the ghost button's.
    Bare,
}

/// Every colour [`control_paint`] reads, and nothing else — the memo key
/// for a painter that cannot afford the derivation per row. Built by
/// [`ControlInputs::new`] from the theme so a call site cannot leave one
/// out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlInputs {
    pub rest: Rest,
    /// The surface the control sits on (`title_bar`, `sidebar`,
    /// `popover`, `table`, …): every fill is composited over it and every
    /// text is floored against the result.
    pub surface: Hsla,
    /// The control's text at rest; kept on every state, floored.
    pub text: Hsla,
    secondary_hover: Hsla,
    secondary_active: Hsla,
    accent: Hsla,
    foreground: Hsla,
}

impl ControlInputs {
    pub fn new(theme: &Theme, rest: Rest, surface: Hsla, text: Hsla) -> Self {
        Self {
            rest,
            surface,
            text,
            secondary_hover: theme.secondary_hover,
            secondary_active: theme.secondary_active,
            accent: theme.accent,
            foreground: theme.foreground,
        }
    }
}

/// A control's two pointer states, each with the text that is readable
/// over it. Opaque colours (already composited over the surface), so
/// what gpui paints is exactly what the sweeps measured. `Copy` so
/// builders called from render closures take it by value the way they
/// take a `RowPaint`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlPaint {
    pub hover: Hsla,
    pub hover_text: Hsla,
    pub pressed: Hsla,
    pub pressed_text: Hsla,
}

/// The pointer states for a control at `rest` with text `text`, sitting
/// on `surface`, on `theme` — the shorthand every once-per-render site
/// uses. A per-row painter builds the [`ControlInputs`] itself and
/// memoises [`control_paint`]'s answer behind them.
pub fn paint(theme: &Theme, rest: Rest, surface: Hsla, text: Hsla) -> ControlPaint {
    control_paint(&ControlInputs::new(theme, rest, surface, text))
}

/// The pointer states for a clickable [`super::chip`] chip — the stack
/// marker every occupant's header paints (`Tone::Neutral`) — sitting on
/// `surface`: a tinted chip is a [`Rest::Filled`] control with its own
/// text, a text-only one is bare.
pub fn for_chip(theme: &Theme, chip: &super::chip::ChipPaint, surface: Hsla) -> ControlPaint {
    let rest = match chip.fill {
        Some(fill) => Rest::Filled(fill),
        None => Rest::Bare,
    };
    paint(theme, rest, surface, chip.text)
}

/// The one place a control's states are decided. Cost when every token
/// is already distinct and every text already readable: three
/// `Hsla -> Rgb` conversions and five contrast checks; when one is not,
/// up to twenty blends or a sixteen-step OKLab bisection for that state.
pub fn control_paint(inputs: &ControlInputs) -> ControlPaint {
    let surface = to_rgb(inputs.surface);
    let rest = match inputs.rest {
        Rest::Filled(fill) => over(fill, surface),
        Rest::Bare => surface,
    };
    let hover_token = match inputs.rest {
        Rest::Filled(_) => inputs.secondary_hover,
        Rest::Bare => inputs.accent,
    };
    let hover = distinct_fill(
        over(hover_token, surface),
        rest,
        inputs.foreground,
        &[(rest, DISTINCT_RATIO)],
    );
    let pressed = distinct_fill(
        over(inputs.secondary_active, surface),
        hover,
        inputs.foreground,
        &[(rest, DISTINCT_RATIO), (hover, PRESSED_STEP)],
    );
    let text = to_rgb(inputs.text);
    let toward = to_rgb(inputs.foreground);
    ControlPaint {
        hover: to_hsla(hover),
        hover_text: to_hsla(readable_on(text, hover, toward)),
        pressed: to_hsla(pressed),
        pressed_text: to_hsla(readable_on(text, pressed, toward)),
    }
}

/// `candidate` where it clears every `(other, ratio)` in `apart_from`;
/// otherwise `base` blended toward `toward` in twenty steps, the first
/// blend that clears them all — `toward` itself if none does (it is the
/// foreground, readable on the surface by the theme author's own hand,
/// so it always clears).
fn distinct_fill(candidate: Rgb, base: Rgb, toward: Hsla, apart_from: &[(Rgb, f32)]) -> Rgb {
    let clears = |c: Rgb| {
        apart_from
            .iter()
            .all(|(other, ratio)| contrast_ratio(c, *other) >= *ratio)
    };
    if clears(candidate) {
        return candidate;
    }
    (1..=20)
        .map(|i| over(toward.opacity(i as f32 * 0.05), base))
        .find(|c| clears(*c))
        .unwrap_or_else(|| to_rgb(toward))
}

/// Whether both states of `paint` clear the readability floor — what the
/// readability sweep asserts. The fills are opaque, so the ground is the
/// fill itself.
pub fn is_readable(paint: &ControlPaint) -> bool {
    contrast_ratio(to_rgb(paint.hover_text), to_rgb(paint.hover)) >= READABLE_RATIO
        && contrast_ratio(to_rgb(paint.pressed_text), to_rgb(paint.pressed)) >= READABLE_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ActiveTheme as _;

    /// Every (rest, surface, text) pairing a shipped call site hands in,
    /// plus `secondary_foreground` on `popover` as the text a new dialog
    /// site is most likely to bring. A site handing in a new pairing adds
    /// it here.
    fn shipped(theme: &Theme) -> Vec<(ControlInputs, &'static str)> {
        let muted = Rest::Filled(theme.muted);
        vec![
            (
                ControlInputs::new(theme, muted, theme.title_bar, theme.muted_foreground),
                "scope chip",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.title_bar, theme.muted_foreground),
                // The grouping readout and the `+`/save verbs (2026-09-19);
                // the chip's `×` takes the chip pairing above.
                "bare title-bar glyph",
            ),
            (
                ControlInputs::new(
                    theme,
                    Rest::Filled(theme.secondary),
                    theme.sidebar,
                    theme.sidebar_foreground,
                ),
                "workspace disc",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.sidebar, theme.sidebar_foreground),
                "settings gear",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.status_bar, theme.warning),
                "diagnostics segment",
            ),
            (
                ControlInputs::new(theme, muted, theme.popover, theme.muted_foreground),
                "value chip",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.popover, theme.muted_foreground),
                "tick (unticked)",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.popover, theme.success),
                "tick (ticked)",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.table, theme.muted_foreground),
                "blotter chevron",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.background, theme.muted_foreground),
                "market-data ⋯",
            ),
            (
                ControlInputs::new(theme, muted, theme.popover, theme.secondary_foreground),
                "secondary_foreground on popover",
            ),
            (
                ControlInputs::new(
                    theme,
                    Rest::Filled(theme.secondary),
                    theme.background,
                    theme.secondary_foreground,
                ),
                "stack marker (Tone::Neutral on a tile header)",
            ),
            (
                // The toolbar's AS OF chip (2026-09-19): clickable since
                // the restyle, a `Tone::Warning` chip on the title bar.
                ControlInputs::new(
                    theme,
                    Rest::Filled(theme.warning.opacity(crate::shell::chip::FILL_ALPHA)),
                    theme.title_bar,
                    theme.foreground,
                ),
                "as-of chip (Tone::Warning on the title bar)",
            ),
        ]
    }

    /// `Rgb → Hsla → Rgb` moves the seventh decimal; two colours within
    /// 1/512 per channel are the same colour on screen.
    fn same_colour(a: Rgb, b: Rgb) -> bool {
        (a.r - b.r).abs() < 1.0 / 512.0
            && (a.g - b.g).abs() < 1.0 / 512.0
            && (a.b - b.b).abs() < 1.0 / 512.0
    }

    fn sweep(
        cx: &mut gpui::TestAppContext,
        mut check: impl FnMut(&str, &Theme, &ControlInputs, &str),
    ) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut themes = 0;
        for name in service.names() {
            themes += 1;
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                for (inputs, label) in shipped(theme) {
                    check(&name, theme, &inputs, label);
                }
            });
        }
        assert!(
            themes >= 40,
            "the sweep saw {themes} themes — bundled themes missing?"
        );
    }

    /// Every state's text clears the 3:1 floor over that state's fill on
    /// EVERY bundled theme, at every shipped site, with no exception list.
    #[gpui::test]
    fn every_control_state_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        let mut failures = Vec::new();
        sweep(cx, |name, _theme, inputs, label| {
            let paint = control_paint(inputs);
            let hover = contrast_ratio(to_rgb(paint.hover_text), to_rgb(paint.hover));
            let pressed = contrast_ratio(to_rgb(paint.pressed_text), to_rgb(paint.pressed));
            if hover < READABLE_RATIO {
                failures.push(format!("{name}: {label} hover at {hover:.2}:1"));
            }
            if pressed < READABLE_RATIO {
                failures.push(format!("{name}: {label} pressed at {pressed:.2}:1"));
            }
        });
        assert!(
            failures.is_empty(),
            "unreadable control states:\n{}",
            failures.join("\n")
        );
    }

    /// Every state is VISIBLE: hover and pressed each at least
    /// [`DISTINCT_RATIO`] from the rest fill, and pressed at least
    /// [`PRESSED_STEP`] from hover, on every theme at every site.
    #[gpui::test]
    fn every_control_state_is_distinct_from_rest_on_every_bundled_theme(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut failures = Vec::new();
        sweep(cx, |name, _theme, inputs, label| {
            let paint = control_paint(inputs);
            let surface = to_rgb(inputs.surface);
            let rest = match inputs.rest {
                Rest::Filled(fill) => over(fill, surface),
                Rest::Bare => surface,
            };
            let hover = contrast_ratio(to_rgb(paint.hover), rest);
            let pressed = contrast_ratio(to_rgb(paint.pressed), rest);
            let step = contrast_ratio(to_rgb(paint.pressed), to_rgb(paint.hover));
            if hover < DISTINCT_RATIO {
                failures.push(format!("{name}: {label} hover vs rest {hover:.3}"));
            }
            if pressed < DISTINCT_RATIO {
                failures.push(format!("{name}: {label} pressed vs rest {pressed:.3}"));
            }
            if step < PRESSED_STEP {
                failures.push(format!("{name}: {label} pressed vs hover {step:.3}"));
            }
        });
        assert!(
            failures.is_empty(),
            "invisible control states:\n{}",
            failures.join("\n")
        );
    }

    /// The floor must bite: the same texts UNFLOORED fail the readability
    /// sweep somewhere, or the door is not doing the work its doc claims.
    #[gpui::test]
    fn the_unfloored_texts_fail_the_sweep(cx: &mut gpui::TestAppContext) {
        let mut failing = 0;
        sweep(cx, |_name, _theme, inputs, _label| {
            let paint = control_paint(inputs);
            let unfloored = ControlPaint {
                hover_text: inputs.text,
                pressed_text: inputs.text,
                ..paint
            };
            if !is_readable(&unfloored) {
                failing += 1;
            }
        });
        assert!(
            failing >= 20,
            "the unfloored texts failed at only {failing} sites — the floor is not measuring anything"
        );
    }

    /// The distinctness rule must bite too: the component's raw
    /// `secondary_hover` over the chips' `muted` rest is under
    /// [`DISTINCT_RATIO`] on a majority of bundled themes (28 of 44 when
    /// measured), which is why the door cannot simply borrow it.
    #[gpui::test]
    fn the_raw_hover_token_fails_the_distinctness_sweep(cx: &mut gpui::TestAppContext) {
        let mut failing = 0;
        sweep(cx, |_name, theme, inputs, label| {
            if label != "scope chip" {
                return;
            }
            let surface = to_rgb(inputs.surface);
            let rest = over(theme.muted, surface);
            let raw = over(theme.secondary_hover, surface);
            if contrast_ratio(raw, rest) < DISTINCT_RATIO {
                failing += 1;
            }
        });
        assert!(
            failing >= 15,
            "the raw token was indistinct on only {failing} themes — the pinned tokens changed \
             or the ratio has lost its teeth"
        );
    }

    /// Where the component's own token IS distinct the door hands it
    /// back untouched (composited over the surface): Nord's
    /// `secondary_hover` sits 1.17:1 from `secondary`. The synthesis is a
    /// fallback, not a house style.
    #[gpui::test]
    fn a_distinct_component_token_is_used_as_is(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let nord = service.resolve("Nord").expect("Nord is bundled").clone();
        cx.update(|cx| {
            Theme::global_mut(cx).apply_config(&nord);
            let theme = cx.theme();
            let surface = to_rgb(theme.sidebar);
            let raw = over(theme.secondary_hover, surface);
            assert!(
                contrast_ratio(raw, over(theme.secondary, surface)) >= DISTINCT_RATIO,
                "the fixture assumes Nord's hover token is distinct"
            );
            let painted = paint(
                theme,
                Rest::Filled(theme.secondary),
                theme.sidebar,
                theme.sidebar_foreground,
            );
            assert!(
                same_colour(to_rgb(painted.hover), raw),
                "filled hover {:?} is not the token {raw:?}",
                to_rgb(painted.hover)
            );
            // And the bare rest borrows `accent`, not the chip's token.
            let accent = over(theme.accent, surface);
            assert!(
                contrast_ratio(accent, surface) >= DISTINCT_RATIO,
                "the fixture assumes Nord's accent is distinct from its sidebar"
            );
            let bare = paint(theme, Rest::Bare, theme.sidebar, theme.sidebar_foreground);
            assert!(
                same_colour(to_rgb(bare.hover), accent),
                "bare hover {:?} is not accent {accent:?}",
                to_rgb(bare.hover)
            );
        });
    }

    /// A text that already clears the floor over its state's fill comes
    /// back unchanged — the floor moves colours only when it has to.
    /// `foreground` over any control fill clears it on the default theme,
    /// asserted rather than assumed so the test cannot pass vacuously.
    #[gpui::test]
    fn a_readable_text_is_left_alone(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let painted = paint(theme, Rest::Bare, theme.background, theme.foreground);
            let ratio = contrast_ratio(to_rgb(theme.foreground), to_rgb(painted.pressed));
            assert!(
                ratio >= READABLE_RATIO,
                "foreground over the pressed fill is {ratio:.2}:1 on the default theme"
            );
            assert_eq!(painted.pressed_text, theme.foreground);
            assert_eq!(painted.hover_text, theme.foreground);
        });
    }

    /// The memo key is the whole derivation: two inputs that compare
    /// equal paint identically, and moving any one field changes the key.
    #[gpui::test]
    fn the_inputs_are_a_complete_memo_key(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let a = ControlInputs::new(theme, Rest::Bare, theme.table, theme.muted_foreground);
            let b = ControlInputs::new(theme, Rest::Bare, theme.table, theme.muted_foreground);
            assert_eq!(a, b);
            assert_eq!(control_paint(&a), control_paint(&b));
            let moved = ControlInputs {
                foreground: theme.background,
                ..a
            };
            assert_ne!(a, moved);
        });
    }
}
