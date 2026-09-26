//! The tile's chrome (spec §9.3): the header strip — stack marker, kind
//! badge, the range and frequency triggers, one chip per slot — the
//! notice line and the footer hint row.
//!
//! [`HeaderModel::prepare`] is the "prepare, never format per frame"
//! rule this crate shares with the market-data panel: every string the
//! header paints is built once, in the tile's `rebuild_chrome`, and
//! `render_header` only clones `SharedString`s and `Hsla`s.
//!
//! A slot's swatch COLOUR is prepared here too, which is why `prepare`
//! takes a resolver: deriving one costs a `Palette::from_theme` (five
//! readability floors, each a possible OKLab bisection) plus twenty-eight
//! `Hsla -> Rgb` conversions for the named-colour wheel, and doing that
//! in `render` would pay it every frame for a value that only moves when
//! the model or the theme does. The tile rebuilds the header on both, the
//! theme behind `theme_signature`.

use geode_core::series::SlotKind;
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::module::StackHandle;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates};
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips::{self, Chords, chord_for};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Hsla, MouseButton, MouseDownEvent, SharedString,
    Stateful, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::{Sizable as _, Theme, h_flex, v_flex};

use crate::core::Range;
use crate::core::model::{Colour, Model, SlotState};
use crate::popup::ExprField;
use crate::tile::TimeseriesTile;

/// The header strip's height at the design rem, and the footer's — both
/// on [`scale`], so `Small`/`Large` scale the frame with the text in it.
pub(crate) const HEADER_HEIGHT: f32 = 26.0;
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;

/// What a tile with no slots paints in place of the chart. Names the two
/// keys that end the state, per the design guide's empty-state rule.
pub(crate) const EMPTY_HINT: &str = "no series — a adds one, x composes";

/// The swatch beside a chip's label, in pixels at the design rem.
const SWATCH: f32 = 8.0;
/// The swatch's click target (mouse pass, 2026-09-24): the dot sits
/// centred in a square this wide, which is what takes the hover fill —
/// a hover painted on the dot itself would replace the one colour the
/// dot exists to show.
const SWATCH_TARGET: f32 = 16.0;

/// The empty state's two doors, as the button labels and the actions
/// they dispatch.
pub(crate) const EMPTY_ACTIONS: [(&str, &str); 2] = [
    ("Add series…", "timeseries::add"),
    ("Compose expression…", "timeseries::expr"),
];

/// One slot's chip, prepared. `tone` is what the chip MEANS; `filled`
/// is whether it paints that tone's tint — an idle slot away from the
/// cursor is bare text, so the strip stays quiet and the cursor, a
/// fetch and a failure are the only things with a fill (the design
/// guide's emphasis budget).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Chip {
    pub label: SharedString,
    pub axis: &'static str,
    pub tone: Tone,
    pub filled: bool,
    pub hidden: bool,
    /// The failure text, shown as a tooltip on a `Danger` chip.
    pub tooltip: Option<SharedString>,
    /// Prepared here rather than `format!`ed in the render closure.
    pub tip_selector: SharedString,
    /// The swatch's own tooltip selector, likewise prepared.
    pub swatch_tip_selector: SharedString,
    /// The slot's colour, already resolved against the theme — see the
    /// module doc for why this is not a `Colour` the painter resolves.
    pub swatch: Hsla,
    pub number: u8,
}

/// Everything the header paints, resolved once per change.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderModel {
    /// The range trigger's text: the preset (`1y`), or the two dates
    /// (`2025-09-26 – 2026-09-26`) while the range is absolute.
    pub range_label: SharedString,
    /// The frequency trigger's text (`1d`).
    pub freq_label: SharedString,
    pub chips: Vec<Chip>,
    pub cursor: Option<usize>,
    pub empty: bool,
}

impl HeaderModel {
    pub(crate) fn prepare(
        model: &Model,
        default_source: Option<&str>,
        colour_of: &dyn Fn(&Colour) -> Hsla,
    ) -> HeaderModel {
        let cursor = model.cursor();
        let chips = model
            .slots()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                // Failed outranks fetching outranks the cursor: a slot
                // that cannot load is the one thing worth the strip's
                // strongest colour, whatever else is true of it.
                let (tone, filled, tooltip) = match &s.state {
                    SlotState::Failed(why) => {
                        (Tone::Danger, true, Some(SharedString::from(why.clone())))
                    }
                    SlotState::Fetching => (Tone::Warning, true, None),
                    SlotState::Idle => (Tone::Neutral, cursor == Some(i), None),
                };
                Chip {
                    label: model.label(i, default_source).into(),
                    axis: s.axis.letter(),
                    tone,
                    filled,
                    hidden: !s.visible,
                    tooltip,
                    tip_selector: format!("tip-timeseries-chip-{}", s.number).into(),
                    swatch_tip_selector: format!("tip-timeseries-swatch-{}", s.number).into(),
                    swatch: colour_of(&s.colour),
                    number: s.number,
                }
            })
            .collect();
        let range_label = match model.range() {
            Range::Relative(p) => SharedString::new_static(p.as_str()),
            Range::Absolute { from, to } => format!("{from} – {to}").into(),
        };
        HeaderModel {
            range_label,
            freq_label: SharedString::new_static(model.frequency().as_str()),
            chips,
            cursor,
            empty: model.slots().is_empty(),
        }
    }
}

/// The tile's title for the stack list and the window (ruling 4):
/// `timeseries · 1y · 1d`, plus ` · n series` once it holds any.
pub(crate) fn title_text(model: &Model) -> SharedString {
    let n = model.slots().len();
    if n == 0 {
        format!("timeseries · {}", model.header_text()).into()
    } else {
        format!("timeseries · {} · {n} series", model.header_text()).into()
    }
}

/// Whether the cursor's slot is a SOURCE — `e` (edit the expression)
/// has nothing to open on one, and says so.
pub(crate) fn cursor_is_source(model: &Model) -> Option<u8> {
    model.cursor_slot().and_then(|s| match s.kind {
        SlotKind::Source { .. } => Some(s.number),
        SlotKind::Expr(_) => None,
    })
}

/// The footer's hint row: each verb's live chord where the keymap has
/// one, the shipped key otherwise. Resolved on a `Chords` change, never
/// per frame.
const FOOTER_HINTS: &[(&str, &str, &str)] = &[
    ("timeseries::add", "a", "add"),
    ("timeseries::expr", "x", "expr"),
    ("timeseries::list", "L", "series"),
    ("timeseries::range", "r", "range"),
    ("timeseries::freq", "f", "freq"),
    ("timeseries::density", "D", "density"),
    ("timeseries::percentiles", "p", "percentiles"),
];

pub(crate) fn footer_text(cx: &App) -> SharedString {
    let empty = Vec::new();
    let bindings = cx
        .try_global::<Chords>()
        .map(|c| c.0.as_slice())
        .unwrap_or(&empty);
    FOOTER_HINTS
        .iter()
        .map(|(action, shipped, word)| {
            let key = chord_for(bindings, action)
                .map(|ks| geode_shell::palette::render_binding(&ks))
                .unwrap_or_else(|| (*shipped).to_string());
            format!("{key} {word}")
        })
        .collect::<Vec<_>>()
        .join(" · ")
        .into()
}

/// What the header draws beside its prepared text: which of its
/// triggers owns the popup that is up (each paints its open state while
/// it does) and the popup elements that hang off them.
pub(crate) struct HeaderPopups {
    /// The action list is up, under `⋯`.
    pub menu_open: bool,
    /// The range menu or the custom dates editor is up.
    pub range_open: bool,
    /// The frequency menu is up.
    pub freq_open: bool,
    /// The open colour picker: the slot number it targets and the
    /// component element, which takes that chip's swatch position.
    pub colour_picker: Option<(u8, AnyElement)>,
    /// The range menu or the dates editor, hung under the range trigger.
    pub under_range: Option<AnyElement>,
    /// The frequency menu, hung under the frequency trigger.
    pub under_freq: Option<AnyElement>,
}

/// One bare trigger that owns a popup (the range and the frequency
/// triggers): its prepared text and a `▾`, the bare-control hover and
/// press at rest, and while its popup is up the neutral chip's fill as a
/// persistent open state that answers the pointer with nothing (the
/// design guide's "Open / pressed" row, and the `⋯` button's own rule).
///
/// It toggles in the CAPTURE phase: the open popup's own
/// `on_mouse_down_out` is a capture listener that would close it before
/// a bubble handler here could see it open, so the click meant to close
/// would reopen it instead. And it calls `prevent_default`: the shell
/// root is `track_focus`ed and gpui focuses it in the press's bubble
/// phase unless default is prevented, so a press that closes or opens a
/// focus-owning popup must not hand the keyboard to the root under it.
/// Propagation still runs, so the tile press in the shell still focuses
/// the tile.
///
/// `popup` hangs off a zero-size point at the trigger's bottom-left, so
/// the menu opens under the control that owns it.
#[allow(clippy::too_many_arguments)]
fn trigger(
    theme: &Theme,
    rest: control::ControlPaint,
    id: &'static str,
    selector: impl Fn() -> String + 'static,
    label: SharedString,
    open: bool,
    tip: (&'static str, &'static str, &'static str),
    on_press: impl Fn(&mut Window, &mut App) + 'static,
    popup: Option<AnyElement>,
) -> Stateful<Div> {
    let open_paint = chip_paint(theme, Tone::Neutral);
    let (tip_id, tip_text, tip_action) = tip;
    div()
        .id(ElementId::Name(SharedString::new_static(id)))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_1()
        .rounded(theme.radius)
        .font_family(fonts::MONO)
        .when(open, |d| {
            d.text_color(open_paint.text)
                .when_some(open_paint.fill, |d, fill| d.bg(fill))
        })
        .when(!open, |d| d.pointer_states(rest))
        .child(label)
        .child(div().text_xs().child("▾"))
        .debug_selector(selector)
        .tooltip(tips::tip(tip_id, tip_text, Some(tip_action), None))
        .capture_any_mouse_down(move |event: &MouseDownEvent, window, cx| {
            if event.button != MouseButton::Left {
                return;
            }
            on_press(window, cx);
            window.prevent_default();
        })
        .when_some(popup, |d, popup| {
            d.child(div().absolute().left_0().top_full().child(popup))
        })
}

pub(crate) fn render_header(
    h: &HeaderModel,
    theme: &Theme,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    stack: Option<&StackHandle>,
    popups: HeaderPopups,
) -> impl IntoElement {
    let HeaderPopups {
        menu_open,
        range_open,
        freq_open,
        mut colour_picker,
        under_range,
        under_freq,
    } = popups;
    let mut row = h_flex()
        .w_full()
        .h(scale::design(HEADER_HEIGHT))
        .items_center()
        .gap_2()
        .px_2()
        .text_sm()
        .text_color(theme.muted_foreground)
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(move || format!("timeseries-header-{tile_id}"));

    // 0. The stack marker, first in the strip, through the one builder
    //    every module uses (tile-stacks spec §5.1).
    row = row.children(stack.and_then(|s| s.marker(theme, TileId(tile_id))));

    // 1. Kind badge — the market-data badge's `secondary` pill.
    row = row.child(
        div()
            .px_1p5()
            .rounded(theme.radius_tokens().sm)
            .bg(theme.secondary)
            .text_color(theme.secondary_foreground)
            .text_xs()
            .child("Timeseries"),
    );

    // 2. The range and frequency triggers, in the data face — bare
    //    controls, each the mouse door onto its own menu (`r` and `f`
    //    are the keys), through `dispatch` on the verb's own id.
    // One bare-control derivation for the triggers, every swatch target
    // and the `⋯` button: all are muted text (or no text) on the tile
    // surface, and `control::paint` can run an OKLab bisection — not a
    // per-chip-per-frame cost.
    let bare_states = control::paint(
        theme,
        control::Rest::Bare,
        theme.background,
        theme.muted_foreground,
    );
    row = row
        .child(trigger(
            theme,
            bare_states,
            "ts-range-trigger",
            move || format!("timeseries-range-{tile_id}"),
            h.range_label.clone(),
            range_open,
            ("tip-timeseries-range", "Range", "timeseries::range"),
            {
                let tile = tile.clone();
                move |window, cx| tile.update(cx, |t, cx| t.range_trigger_clicked(window, cx))
            },
            under_range,
        ))
        .child(trigger(
            theme,
            bare_states,
            "ts-freq-trigger",
            move || format!("timeseries-freq-{tile_id}"),
            h.freq_label.clone(),
            freq_open,
            ("tip-timeseries-freq", "Frequency", "timeseries::freq"),
            {
                let tile = tile.clone();
                move |window, cx| tile.update(cx, |t, cx| t.freq_trigger_clicked(window, cx))
            },
            under_freq,
        ));

    // 3. One chip per slot.
    for (index, chip) in h.chips.iter().enumerate() {
        // An unfilled chip is a BARE control, and it drops the TEXT
        // with the fill: `for_chip` reads the fill to pick the hover
        // token, so clearing it is what makes a quiet chip's hover
        // borrow `accent` rather than the secondary button's; and
        // `Tone::Neutral`'s own 3:1 guarantee is measured over its fill,
        // so `secondary_foreground` painted straight on the tile
        // background is covered by no sweep at all (review round 1,
        // I-1). `muted_foreground` on `background` is the bare pairing
        // `control::shipped()` already carries.
        let mut paint = chip_paint(theme, chip.tone);
        if !chip.filled {
            paint.fill = None;
            paint.text = theme.muted_foreground;
        }
        let states = control::for_chip(theme, &paint, theme.background);
        let number = chip.number;
        // The picker's trigger is `Size::XSmall`'s square — the swatch
        // target's own size — so the strip keeps its geometry while it
        // stands in for the swatch. The trigger stops its own press, so
        // neither the chip's click nor the swatch's toggle runs under it.
        let picker = colour_picker
            .take_if(|(target, _)| *target == number)
            .map(|(_, el)| el);
        let swatch = match picker {
            Some(picker) => picker,
            None => div()
                .id(ElementId::NamedInteger(
                    SharedString::new_static("ts-swatch"),
                    number as u64,
                ))
                .debug_selector(move || format!("timeseries-swatch-{tile_id}-{number}"))
                .size(scale::design(SWATCH_TARGET))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_tokens().sm)
                .pointer_states(bare_states)
                .tooltip(tips::tip_with(
                    chip.swatch_tip_selector.clone(),
                    SharedString::new_static(if chip.hidden { "Show" } else { "Hide" }),
                    Some("timeseries::toggle_visible"),
                    None,
                ))
                .on_mouse_down(MouseButton::Left, {
                    let tile = tile.clone();
                    move |_: &MouseDownEvent, _window, cx| {
                        tile.update(cx, |t, cx| t.swatch_clicked(index, cx));
                    }
                })
                .child(
                    div()
                        .size(scale::design(SWATCH))
                        .rounded_full()
                        .bg(chip.swatch),
                )
                .into_any_element(),
        };
        let mut el = div()
            .id(ElementId::NamedInteger(
                SharedString::new_static("ts-chip"),
                number as u64,
            ))
            .debug_selector(move || format!("timeseries-chip-{tile_id}-{number}"))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_1()
            .rounded(theme.radius)
            .text_color(paint.text)
            .when_some(paint.fill, |d, fill| d.bg(fill))
            .pointer_states(states)
            // A hidden series stays in the strip — `v` is a toggle, and a
            // chip that vanished would leave nothing to press again.
            .when(chip.hidden, |d| d.opacity(0.5).line_through())
            // The swatch is the show/hide toggle (mouse pass,
            // 2026-09-24): a square target round the dot with the bare
            // control's hover, and a click that takes `v`'s own path.
            // No propagation stop, for the chip's reason below — the
            // chip's own handler also runs and moves the cursor onto
            // the slot just toggled, which is the slot `v` would act on.
            .child(swatch)
            .child(chip.label.clone())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(chip.axis),
            )
            // The mouse's form of `tab`: a click moves the cursor.
            //
            // Deliberately no `cx.stop_propagation()` — the shell's own
            // tile-level mouse-down (`leave_command_line`,
            // `focus_main_tile`, the `pending_focus_restore` re-arm) must
            // still run. Stopping here suppressed the shell's whole
            // bubble phase for this click, so a chip click on an
            // UNFOCUSED tile moved that tile's cursor while shell focus
            // stayed elsewhere, and every bare key after it drove
            // whichever tile the shell still had focused. Same finding,
            // same fix as the market-data `⋯` button:
            // `crates/geode-marketdata/src/header.rs:528-537`. The
            // popup ROWS keep theirs — they sit on a `deferred`,
            // occluding surface of their own, the market-data popup's
            // shape.
            .on_mouse_down(MouseButton::Left, {
                let tile = tile.clone();
                move |_: &MouseDownEvent, _window, cx| {
                    tile.update(cx, |t, cx| t.chip_clicked(index, cx));
                }
            })
            // The context menu (mouse pass, 2026-09-24): a right-click
            // selects the slot and opens the action list on it.
            .on_mouse_down(MouseButton::Right, {
                let tile = tile.clone();
                move |_: &MouseDownEvent, window, cx| {
                    tile.update(cx, |t, cx| t.chip_context_menu(index, window, cx));
                }
            });
        if let Some(reason) = &chip.tooltip {
            el = el.tooltip(tips::tip_with(
                chip.tip_selector.clone(),
                reason.clone(),
                None,
                None,
            ));
        }
        row = row.child(el);
    }

    // 4. `⋯` — the mouse door onto the action list (mouse pass,
    //    2026-09-24), the click's own form of `.`, at the strip's right
    //    edge behind a spacer. The market-data `⋯` button's shape
    //    exactly, including the two things that are easy to get wrong:
    //    it toggles in the CAPTURE phase (ahead of an open menu's own
    //    `on_mouse_down_out`, which would otherwise close the menu one
    //    beat before this handler asked whether it was open, so a
    //    second click reopened it) and it does NOT stop propagation
    //    (the shell's click-to-focus must still run, or the menu's keys
    //    drive whichever tile the shell still had focused).
    let muted = theme.muted_foreground;
    row = row.child(div().flex_1()).child(
        div()
            .id(ElementId::Name(SharedString::new_static("ts-menu-button")))
            .debug_selector(move || format!("timeseries-menu-button-{tile_id}"))
            .px_1p5()
            .rounded(theme.radius_tokens().sm)
            .border_1()
            .border_color(theme.border)
            .when(menu_open, |d| d.bg(theme.secondary))
            .text_color(muted)
            // Open, the button keeps its persistent fill and answers the
            // pointer with nothing, as the guide asks of a button that
            // owns a popup.
            .when(!menu_open, |d| d.pointer_states(bare_states))
            .child("⋯")
            .tooltip(tips::tip(
                "tip-timeseries-menu",
                "Actions",
                Some("timeseries::menu"),
                None,
            ))
            .capture_any_mouse_down({
                let tile = tile.clone();
                move |event, window, cx| {
                    if event.button != MouseButton::Left {
                        return;
                    }
                    tile.update(cx, |t, cx| t.toggle_menu(window, cx))
                }
            }),
    );
    row
}

/// The notice line under the header: the last refusal or advisory, in
/// the danger tone as TEXT (no fill — it is a sentence, not a state).
pub(crate) fn render_notice(notice: &SharedString, theme: &Theme) -> impl IntoElement {
    let paint = chip_paint(theme, Tone::DangerText);
    h_flex()
        .w_full()
        .px_2()
        .text_xs()
        .text_color(paint.text)
        .child(notice.clone())
}

/// The expression field's strip (spec §9.7), between the header and the
/// chart: a one-line borderless `Input` with its parse error under it —
/// inline, in the notice line's own danger text, because a bad
/// expression keeps the field open and the reason belongs beside what
/// caused it rather than in the tile's standing notice.
pub(crate) fn render_expr_field(f: &ExprField, theme: &Theme) -> impl IntoElement {
    let paint = chip_paint(theme, Tone::DangerText);
    v_flex()
        .w_full()
        .px_2()
        .py_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .child(Input::new(&f.input).appearance(false).w_full())
        .when_some(f.error.clone(), |el, e| {
            el.child(div().text_xs().text_color(paint.text).child(e))
        })
}

pub(crate) fn render_footer(text: SharedString, theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .items_center()
        .px_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_t_1()
        .border_color(theme.border)
        .child(text)
}

/// The chart's place while the tile holds no slot: the hint naming
/// the two keys, and (mouse pass, 2026-09-24) the same two verbs as
/// ghost buttons under it — the design guide's "useful empty state
/// that explains the next action", reachable by either hand. Each
/// button dispatches its action id, the key's own path.
pub(crate) fn render_empty(
    theme: &Theme,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
) -> impl IntoElement {
    let mut buttons = h_flex().gap_2();
    for (i, (label, action)) in EMPTY_ACTIONS.into_iter().enumerate() {
        let tile = tile.clone();
        buttons = buttons.child(
            div()
                .debug_selector(move || format!("timeseries-empty-{tile_id}-{i}"))
                .child(
                    Button::new(ElementId::NamedInteger(
                        SharedString::new_static("ts-empty"),
                        i as u64,
                    ))
                    .small()
                    .ghost()
                    .label(label)
                    .on_click(move |_event, window, cx| {
                        tile.update(cx, |t, cx| {
                            t.dispatch(&ActionId(action.to_string()), None, window, cx);
                        });
                    }),
                ),
        );
    }
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(theme.muted_foreground)
        .child(EMPTY_HINT)
        .child(buttons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Model;
    use geode_core::series::expr::Expr;

    /// A resolver with no theme behind it: the palette index straight
    /// into the hue, so a test can tell one slot's swatch from another's
    /// without a window.
    fn stub(colour: &Colour) -> gpui::Hsla {
        match colour {
            Colour::Palette(i) => gpui::hsla(*i as f32 / 10.0, 1.0, 0.5, 1.0),
            Colour::Named(_) => gpui::black(),
            Colour::Custom(c) => c.to_hsla(),
        }
    }

    fn two() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m
    }

    #[test]
    fn a_chip_carries_its_label_axis_and_swatch() {
        let m = two();
        let h = HeaderModel::prepare(&m, Some("demo_kdb"), &stub);
        assert_eq!(h.range_label.as_ref(), "1y");
        assert_eq!(h.freq_label.as_ref(), "1d");
        assert!(!h.empty);
        assert_eq!(h.chips[0].label.as_ref(), "SPX.close");
        assert_eq!(
            h.chips[1].label.as_ref(),
            "VIX@demo_rest",
            "the source shows when it is not the default"
        );
        assert_eq!(h.chips[0].axis, "L");
        assert_eq!(h.chips[0].swatch, stub(&Colour::Palette(0)));
        assert_eq!(h.chips[1].swatch, stub(&Colour::Palette(1)));
        assert_eq!(h.chips[0].number, 1);
    }

    #[test]
    fn failed_outranks_fetching_outranks_the_cursor_and_only_those_three_fill() {
        let mut m = two();
        // s2 is the cursor and still fetching.
        assert_eq!(m.cursor(), Some(1));
        let h = HeaderModel::prepare(&m, Some("demo_kdb"), &stub);
        assert_eq!(h.chips[1].tone, Tone::Warning);
        assert!(h.chips[1].filled);
        m.set_state(2, SlotState::Failed("no route".into()));
        let h = HeaderModel::prepare(&m, Some("demo_kdb"), &stub);
        assert_eq!(h.chips[1].tone, Tone::Danger);
        assert_eq!(h.chips[1].tooltip.as_deref(), Some("no route"));
        m.set_state(1, SlotState::Idle);
        m.set_state(2, SlotState::Idle);
        let h = HeaderModel::prepare(&m, Some("demo_kdb"), &stub);
        assert!(!h.chips[0].filled, "idle, off the cursor: no fill");
        assert!(h.chips[1].filled, "idle, on the cursor: the neutral pill");
        assert_eq!(h.chips[1].tone, Tone::Neutral);
    }

    /// An absolute range's trigger reads its two dates; a preset's, its
    /// short label.
    #[test]
    fn the_triggers_read_the_range_and_the_frequency_in_force() {
        let mut m = two();
        let now = chrono::Utc::now();
        m.set_range(
            Range::parse(&["2025-09-26", "2026-09-26"]).unwrap(),
            now,
            &geode_core::query::AsOf::Live,
        )
        .unwrap();
        m.set_frequency(
            geode_core::series::Frequency::W1,
            now,
            &geode_core::query::AsOf::Live,
        )
        .unwrap();
        let h = HeaderModel::prepare(&m, Some("demo_kdb"), &stub);
        assert_eq!(h.range_label.as_ref(), "2025-09-26 – 2026-09-26");
        assert_eq!(h.freq_label.as_ref(), "1w");
    }

    #[test]
    fn a_hidden_slot_is_struck_through_not_dropped() {
        let mut m = two();
        m.set_visible(1, false).unwrap();
        let h = HeaderModel::prepare(&m, Some("demo_kdb"), &stub);
        assert_eq!(h.chips.len(), 2);
        assert!(h.chips[0].hidden);
        assert!(!h.chips[1].hidden);
    }

    #[test]
    fn an_empty_model_says_so_and_the_title_counts_series() {
        let empty = Model::new();
        let h = HeaderModel::prepare(&empty, None, &stub);
        assert!(h.empty);
        assert!(h.chips.is_empty());
        assert_eq!(title_text(&empty).as_ref(), "timeseries · 1y · 1d");
        assert_eq!(
            title_text(&two()).as_ref(),
            "timeseries · 1y · 1d · 2 series"
        );
    }

    #[test]
    fn cursor_is_source_names_a_source_slot_and_not_an_expression() {
        let mut m = two();
        assert_eq!(cursor_is_source(&m), Some(2));
        m.add_expr("s1 / s2", Expr::Ref(1)).unwrap();
        assert_eq!(cursor_is_source(&m), None);
    }
}
