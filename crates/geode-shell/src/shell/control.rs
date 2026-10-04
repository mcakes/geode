//! Hover and pressed colours for clickable controls: chips, icons, ticks,
//! and other elements with mouse listeners. List rows use
//! [`super::listrow::row_paint`]; component buttons paint their own states.
//!
//! [`ControlPaint`] supplies the two states through
//! [`PointerStates::pointer_states`]. Each fill uses the theme's component
//! token when it is visibly distinct from rest ([`DISTINCT_RATIO`]);
//! otherwise it steps toward `foreground`. Pressed fills must also differ
//! from hover by [`PRESSED_STEP`]. Theme tokens can equal the rest fill,
//! so using them directly would sometimes hide the interaction state.
//!
//! Comparisons composite fills over the control's actual surface, such as
//! `title_bar`, `sidebar`, `popover`, or `table`. Each state's text keeps
//! the control's semantic colour, adjusted to the readability floor against
//! that fill. Measuring against the window background would give incorrect
//! results for translucent fills on other surfaces.
//!
//! [`ControlInputs`] includes every colour read by the derivation. A caller
//! painting many rows can cache a [`ControlPaint`] by these inputs and avoid
//! repeating colour conversions and readability adjustments.
//!
//! Controls must be stateful for GPUI to repaint on hover transitions and
//! track presses. Apply `pointer_states` once: GPUI rejects a second hover
//! style on the same element in debug builds. Theme sweeps below check fill
//! distinction and text readability for each supported control pairing.

use geode_core::colour::{Rgb, TEXT_READABLE_RATIO, contrast_ratio, readable_text_on};
use gpui::prelude::*;
use gpui::{Hsla, StatefulInteractiveElement};
use gpui_component::Theme;

use super::colours::{over, to_hsla, to_rgb};

/// Minimum luminance ratio between a state's fill and the control at
/// rest. Theme tokens below this ratio receive a lightness adjustment so
/// hover and pressed states remain visible.
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
    ControlPaint {
        hover: to_hsla(hover),
        hover_text: to_hsla(readable_text_on(text, hover)),
        pressed: to_hsla(pressed),
        pressed_text: to_hsla(readable_text_on(text, pressed)),
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
    contrast_ratio(to_rgb(paint.hover_text), to_rgb(paint.hover)) >= TEXT_READABLE_RATIO
        && contrast_ratio(to_rgb(paint.pressed_text), to_rgb(paint.pressed)) >= TEXT_READABLE_RATIO
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
        let active = crate::shell::chip::chip_paint(theme, crate::shell::chip::Tone::Active);
        vec![
            (
                ControlInputs::new(theme, muted, theme.title_bar, theme.muted_foreground),
                "scope chip",
            ),
            (
                ControlInputs::new(theme, Rest::Bare, theme.title_bar, theme.muted_foreground),
                // The grouping readout and the `+`/save verbs;
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
                ControlInputs::new(theme, Rest::Bare, theme.status_bar, theme.danger),
                "stopped segment",
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
                "market-data ⋯ and the tile ×",
            ),
            (
                ControlInputs::new(
                    theme,
                    Rest::Bare,
                    theme.background,
                    crate::shell::chip::chip_paint(theme, crate::shell::chip::Tone::WarningText)
                        .text,
                ),
                // `geode_tile::notice::dismissable`, a warning notice.
                "dismissable warning notice",
            ),
            (
                ControlInputs::new(
                    theme,
                    Rest::Bare,
                    theme.background,
                    crate::shell::chip::chip_paint(theme, crate::shell::chip::Tone::DangerText)
                        .text,
                ),
                "dismissable danger notice",
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
                // Also the timeseries range and frequency triggers'
                // open state and the cursor's slot chip: the same fill,
                // text and ground.
                "stack marker (Tone::Neutral on a tile header)",
            ),
            (
                // The clickable AS OF chip uses `Tone::Warning` on the title bar.
                ControlInputs::new(
                    theme,
                    Rest::Filled(theme.warning.opacity(crate::shell::chip::FILL_ALPHA)),
                    theme.title_bar,
                    theme.foreground,
                ),
                "as-of chip (Tone::Warning on the title bar)",
            ),
            (
                // The pinned workspace glyph uses `Tone::Active` on the title bar.
                ControlInputs::new(
                    theme,
                    Rest::Filled(active.fill.unwrap_or(theme.primary)),
                    theme.title_bar,
                    active.text,
                ),
                "pinned glyph (Tone::Active on the title bar)",
            ),
            (
                // Timeseries slot chips sit on the tile background and move the
                // cursor when clicked. Fetching slots use `Tone::Warning`.
                ControlInputs::new(
                    theme,
                    Rest::Filled(theme.warning.opacity(crate::shell::chip::FILL_ALPHA)),
                    theme.background,
                    theme.foreground,
                ),
                "timeseries slot chip (Tone::Warning on a tile background)",
            ),
            (
                // A timeseries slot with a failed fetch uses `Tone::Danger`.
                ControlInputs::new(
                    theme,
                    Rest::Filled(theme.danger.opacity(crate::shell::chip::FILL_ALPHA)),
                    theme.background,
                    theme.foreground,
                ),
                "timeseries slot chip (Tone::Danger on a tile background)",
            ),
            // An UNFILLED chip — the timeseries tile's idle slot away
            // from the cursor — is a bare `muted_foreground` glyph on
            // the tile background, which is the "market-data ⋯" pairing
            // above; it needs no entry of its own. That it is muted and
            // not the neutral chip's `secondary_foreground` is the whole
            // point: `Tone::Neutral`'s readability is measured over its
            // OWN fill, so a chip that drops the fill must drop the
            // paired text with it.
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

    /// Every state's text clears the 4.5:1 floor over that state's fill on
    /// EVERY bundled theme, at every shipped site, with no exception list.
    #[gpui::test]
    fn every_control_state_is_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        let mut failures = Vec::new();
        sweep(cx, |name, _theme, inputs, label| {
            let paint = control_paint(inputs);
            let hover = contrast_ratio(to_rgb(paint.hover_text), to_rgb(paint.hover));
            let pressed = contrast_ratio(to_rgb(paint.pressed_text), to_rgb(paint.pressed));
            if hover < TEXT_READABLE_RATIO {
                failures.push(format!("{name}: {label} hover at {hover:.2}:1"));
            }
            if pressed < TEXT_READABLE_RATIO {
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
                ratio >= TEXT_READABLE_RATIO,
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
