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
//! `ControlPaint` per control, applied as
//! `.hover(|s| s.bg(p.hover).text_color(p.hover_text))
//!  .active(|s| s.bg(p.pressed).text_color(p.pressed_text))`
//! — gpui's `active` refinement is the pressed state.
//!
//! The colours mirror gpui-component's own `Button` variants at the
//! pinned release so a Geode chip lights the way a component button does
//! (the guide's "preserve the component family"): a [`Rest::Filled`]
//! control — a chip with a fill of its own — hovers to `secondary_hover`
//! and presses to `secondary_active`, the secondary variant's pair, and
//! keeps its own text; a [`Rest::Bare`] control — a glyph on the surface
//! — hovers to `accent` under `accent_foreground` and presses to
//! `secondary_active`, the ghost variant's pair. Every text is floored to
//! 3:1 over the ground it lands on (`geode_core::colour::readable_on`,
//! Part 2c §2.2's rule), because `muted_foreground` over `secondary_hover`
//! is a pairing no theme author tuned: unfloored it is under 3:1 on 16 of
//! 44 bundled themes (Catppuccin Latte 1.93:1), over `secondary_active`
//! on 19, and `warning` over the pressed fill on 15 (measured
//! 2026-09-19); `accent_foreground` over `accent` clears everywhere, as
//! a component token pair should.
//!
//! [`every_control_state_is_readable_on_every_bundled_theme`] is the test
//! to keep: a new rest kind added here without clearing the sweep cannot
//! ship.

use geode_core::colour::{READABLE_RATIO, Rgb, contrast_ratio, readable_on};
use gpui::prelude::*;
use gpui::{Hsla, StatefulInteractiveElement};
use gpui_component::Theme;

use super::colours::{over, to_hsla, to_rgb};

/// The one way a [`ControlPaint`] reaches an element: hover and pressed
/// as gpui style refinements. On a `Stateful` element only — `active`
/// (the pressed state) needs the element identity gpui keeps pressed
/// state under, which is also what a tooltip needs, so every clickable
/// control in this crate already carries an `.id(..)`. A child that pins
/// its own `text_color` (an `Icon::new(..).text_color(fg)`) will not
/// recolour on hover: let it inherit the control's text instead.
pub trait PointerStates: StatefulInteractiveElement + Styled + Sized {
    fn pointer_states(self, paint: ControlPaint) -> Self {
        self.hover(move |s| s.bg(paint.hover).text_color(paint.hover_text))
            .active(move |s| s.bg(paint.pressed).text_color(paint.pressed_text))
    }
}

impl<E: StatefulInteractiveElement + Styled> PointerStates for E {}

/// What the control looks like at rest — which decides the pair of
/// component-button tokens its pointer states borrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rest {
    /// A chip with a fill of its own (`muted`, `secondary`): hover and
    /// press step the fill through the secondary button's tokens.
    Filled,
    /// A glyph or label painted straight on the surface: hover lights an
    /// `accent` box the way a ghost button does.
    Bare,
}

/// A control's two pointer states, each with the text that is readable
/// over it. `Copy` so builders called from render closures take it by
/// value the way they take a `RowPaint`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlPaint {
    pub hover: Hsla,
    pub hover_text: Hsla,
    pub pressed: Hsla,
    pub pressed_text: Hsla,
}

/// The pointer states for a control at `rest` whose text at rest is
/// `text`, on `theme` — the one place they are decided. Two contrast
/// checks per call; read per render like [`super::chip::chip_paint`].
pub fn control_paint(theme: &Theme, rest: Rest, text: Hsla) -> ControlPaint {
    let pressed = theme.secondary_active;
    match rest {
        Rest::Filled => ControlPaint {
            hover: theme.secondary_hover,
            hover_text: floored(theme, text, theme.secondary_hover),
            pressed,
            pressed_text: floored(theme, text, pressed),
        },
        Rest::Bare => ControlPaint {
            hover: theme.accent,
            hover_text: floored(theme, theme.accent_foreground, theme.accent),
            pressed,
            pressed_text: floored(theme, text, pressed),
        },
    }
}

/// `text` over `fill` composited on the window background: itself where
/// it clears the floor, else moved in lightness toward `foreground`
/// until it does.
fn floored(theme: &Theme, text: Hsla, fill: Hsla) -> Hsla {
    to_hsla(readable_on(
        to_rgb(text),
        ground(theme, fill),
        to_rgb(theme.foreground),
    ))
}

/// The ground a state's text lands on: the state's fill composited over
/// the window background.
pub fn ground(theme: &Theme, fill: Hsla) -> Rgb {
    over(fill, to_rgb(theme.background))
}

/// Whether both states of `paint` clear the readability floor on
/// `theme` — what the sweep test asserts.
pub fn is_readable(theme: &Theme, paint: &ControlPaint) -> bool {
    contrast_ratio(to_rgb(paint.hover_text), ground(theme, paint.hover)) >= READABLE_RATIO
        && contrast_ratio(to_rgb(paint.pressed_text), ground(theme, paint.pressed))
            >= READABLE_RATIO
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ActiveTheme as _;

    /// Every (rest, text) pairing a shipped call site hands in: the
    /// chips' `muted_foreground`, the sidebar's `sidebar_foreground` (a
    /// disc is filled, the gear bare), the status bar's `warning`, plus
    /// `secondary_foreground` and `foreground` as the two texts a new
    /// site is most likely to bring. A site handing in a new text adds
    /// it here.
    fn rest_texts(theme: &Theme) -> [(Rest, Hsla, &'static str); 7] {
        [
            (
                Rest::Filled,
                theme.muted_foreground,
                "filled/muted_foreground",
            ),
            (
                Rest::Filled,
                theme.secondary_foreground,
                "filled/secondary_foreground",
            ),
            (
                Rest::Filled,
                theme.sidebar_foreground,
                "filled/sidebar_foreground",
            ),
            (Rest::Bare, theme.muted_foreground, "bare/muted_foreground"),
            (
                Rest::Bare,
                theme.sidebar_foreground,
                "bare/sidebar_foreground",
            ),
            (Rest::Bare, theme.warning, "bare/warning"),
            (Rest::Bare, theme.foreground, "bare/foreground"),
        ]
    }

    /// Every state's text must clear the 3:1 floor over its own ground on
    /// EVERY bundled theme, for every rest kind and every text a shipped
    /// site hands in, with no exception list.
    #[gpui::test]
    fn every_control_state_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                for (rest, text, label) in rest_texts(theme) {
                    checked += 1;
                    let paint = control_paint(theme, rest, text);
                    let hover =
                        contrast_ratio(to_rgb(paint.hover_text), ground(theme, paint.hover));
                    let pressed =
                        contrast_ratio(to_rgb(paint.pressed_text), ground(theme, paint.pressed));
                    if hover < READABLE_RATIO {
                        failures.push(format!("{name}: {label} hover at {hover:.2}:1"));
                    }
                    if pressed < READABLE_RATIO {
                        failures.push(format!("{name}: {label} pressed at {pressed:.2}:1"));
                    }
                }
            });
        }
        assert!(
            checked >= 7 * 40,
            "the sweep saw {checked} checks — bundled themes missing?"
        );
        assert!(
            failures.is_empty(),
            "unreadable control states:\n{}",
            failures.join("\n")
        );
    }

    /// The floor must bite: the same texts handed in UNFLOORED fail the
    /// sweep somewhere, or the door is not doing the work its doc claims
    /// and a maintainer could drop the `readable_on` calls unnoticed.
    #[gpui::test]
    fn the_unfloored_texts_fail_the_sweep(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failing = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                for (rest, text, _) in rest_texts(theme) {
                    let hover = match rest {
                        Rest::Filled => theme.secondary_hover,
                        Rest::Bare => theme.accent,
                    };
                    let unfloored = ControlPaint {
                        hover,
                        hover_text: match rest {
                            Rest::Filled => text,
                            Rest::Bare => theme.accent_foreground,
                        },
                        pressed: theme.secondary_active,
                        pressed_text: text,
                    };
                    if !is_readable(theme, &unfloored) {
                        failing += 1;
                    }
                }
            });
        }
        assert!(
            failing >= 1,
            "every unfloored pairing cleared the floor on every theme — \
             the floor is not measuring anything"
        );
    }

    /// A filled control borrows the secondary button's hover/active pair
    /// and a bare one the ghost button's `accent` hover; both press to
    /// `secondary_active`.
    #[gpui::test]
    fn rests_resolve_to_their_documented_tokens(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let filled = control_paint(theme, Rest::Filled, theme.muted_foreground);
            assert_eq!(filled.hover, theme.secondary_hover);
            assert_eq!(filled.pressed, theme.secondary_active);
            let bare = control_paint(theme, Rest::Bare, theme.muted_foreground);
            assert_eq!(bare.hover, theme.accent);
            assert_eq!(bare.pressed, theme.secondary_active);
        });
    }

    /// A text that already clears the floor over its ground comes back
    /// unchanged — the floor moves colours only when it has to.
    #[gpui::test]
    fn a_readable_text_is_left_alone(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            let theme = cx.theme();
            let paint = control_paint(theme, Rest::Bare, theme.foreground);
            let ratio = contrast_ratio(to_rgb(theme.foreground), ground(theme, paint.pressed));
            if ratio >= READABLE_RATIO {
                assert_eq!(paint.pressed_text, theme.foreground);
            }
        });
    }
}
