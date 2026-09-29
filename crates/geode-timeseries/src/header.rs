//! Header chips, range and frequency triggers, notices, footer hints, and the
//! inline expression editor for the timeseries tile.
//!
//! `HeaderModel::prepare` retains labels, tooltip selectors, and resolved
//! swatches. The tile refreshes this model through `rebuild_chrome` when its
//! inputs change; ordinary header painting reuses those values.
//!
//! The tile supplies one color resolver for header swatches, series-list rows,
//! and chart lines. This shares the palette's readability adjustment and the
//! named-color wheel instead of deriving them for each slot or each paint.
//! Theme and named-color changes invalidate the prepared colors.

use geode_core::series::SlotKind;
use geode_shell::actions::ActionId;
use geode_shell::fonts;
use geode_shell::keymap::{Keystroke, Modifiers, parse_binding};
use geode_shell::module::StackHandle;
use geode_shell::shell::chip::{Tone, chip_paint};
use geode_shell::shell::control::{self, PointerStates};
use geode_shell::shell::kbd;
use geode_shell::shell::scale;
use geode_shell::tiling::TileId;
use geode_shell::tips::{self, Chords, chord_for};
use geode_tile::header::{Cluster, HealthChip, MenuTrigger};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Hsla, MouseButton, MouseDownEvent, SharedString,
    Stateful, Window, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::Input;
use gpui_component::{ActiveTheme as _, Sizable as _, Theme, h_flex, v_flex};

use std::rc::Rc;

use crate::core::Range;
use crate::core::model::{Color, Model, SlotState};
use crate::popup::ExprField;
use crate::tile::TimeseriesTile;

/// The footer's height at the design rem, on [`scale`], so
/// `Small`/`Large` scale it with the text in it. The header's is the
/// shared `geode_tile::header::HEADER_HEIGHT`.
pub(crate) const FOOTER_HEIGHT: f32 = 20.0;

/// The sources the tile's series read, once each, sorted: the tile's
/// health question. Expressions read no source of their own.
pub(crate) fn slot_sources(model: &Model) -> Vec<&str> {
    let mut sources: Vec<&str> = model
        .slots()
        .iter()
        .filter_map(|s| match &s.kind {
            SlotKind::Source { source, .. } => Some(source.as_str()),
            SlotKind::Expr(_) => None,
        })
        .collect();
    sources.sort_unstable();
    sources.dedup();
    sources
}

/// Guidance shown instead of the chart when no slots exist. Backtick-quoted
/// keys name the add and compose actions and paint as chips via `kbd::marked`.
pub(crate) const EMPTY_HINT: &str = "no series — `a` adds one, `x` composes";

/// The swatch beside a chip's label, in pixels at the design rem.
const SWATCH: f32 = 8.0;
/// Square pointer target around the swatch. Hover fills the surrounding
/// control so the dot continues to show the series color.
const SWATCH_TARGET: f32 = 16.0;

/// Empty-state button labels and their registered actions.
pub(crate) const EMPTY_ACTIONS: [(&str, &str); 2] = [
    ("Add series…", "timeseries::add"),
    ("Compose expression…", "timeseries::expr"),
];

/// Prepared slot chip. `tone` carries fetch state; `filled` highlights fetching,
/// failure, or the selected idle slot. Other idle slots use bare text.
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
    /// The slot's color, already resolved against the theme — see the
    /// module doc for why this is not a `Color` the painter resolves.
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
        color_of: &dyn Fn(&Color) -> Hsla,
    ) -> HeaderModel {
        let cursor = model.cursor();
        let chips = model
            .slots()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                // Failed outranks fetching outranks the cursor: a slot
                // that cannot load is the one thing worth the strip's
                // strongest color, whatever else is true of it.
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
                    swatch: color_of(&s.color),
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

/// The tile's title for the stack list and window:
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
/// one, the shipped binding otherwise. Resolved on a `Chords` change,
/// never per frame.
const FOOTER_HINTS: &[(&str, &str, &str)] = &[
    ("timeseries::add", "a", "add"),
    ("timeseries::expr", "x", "expr"),
    ("timeseries::list", "shift+l", "series"),
    ("timeseries::range", "r", "range"),
    ("timeseries::freq", "f", "freq"),
    ("timeseries::density", "shift+d", "density"),
    ("timeseries::percentiles", "p", "percentiles"),
];

/// One footer hint: the verb's keys, then its word — every word but the
/// last already carries its ` ·` separator, so paint formats nothing.
pub(crate) type FooterHint = (Vec<Keystroke>, SharedString);

pub(crate) fn footer_hints(cx: &App) -> Vec<FooterHint> {
    let empty = Vec::new();
    let bindings = cx
        .try_global::<Chords>()
        .map(|c| c.0.as_slice())
        .unwrap_or(&empty);
    let last = FOOTER_HINTS.len() - 1;
    FOOTER_HINTS
        .iter()
        .enumerate()
        .map(|(i, (action, shipped, word))| {
            let keys = chord_for(bindings, action).unwrap_or_else(|| {
                parse_binding(shipped, Modifiers::NONE).expect("shipped footer keys are valid")
            });
            let word = if i == last {
                SharedString::new_static(word)
            } else {
                format!("{word} ·").into()
            };
            (keys, word)
        })
        .collect()
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
    /// The open color picker: the slot number it targets and the
    /// component element, which takes that chip's swatch position.
    pub color_picker: Option<(u8, AnyElement)>,
    /// The range menu or the dates editor, hung under the range trigger.
    pub under_range: Option<AnyElement>,
    /// The frequency menu, hung under the frequency trigger.
    pub under_freq: Option<AnyElement>,
}

/// Range or frequency trigger with a popup anchored at its bottom-left.
/// Closed triggers use bare-control hover and press states. An open trigger
/// keeps a neutral fill without additional pointer feedback.
///
/// Toggle during capture, before the popup's outside-press listener can close
/// it; toggling during bubble would reopen the popup on the same click.
/// Propagation continues so the shell can focus the clicked tile.
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
        })
        .when_some(popup, |d, popup| {
            d.child(div().absolute().left_0().top_full().child(popup))
        })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_header(
    h: &HeaderModel,
    theme: &Theme,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    stack: Option<&StackHandle>,
    popups: HeaderPopups,
    menu_selector: SharedString,
    health: Option<&HealthChip>,
) -> impl IntoElement {
    let HeaderPopups {
        menu_open,
        range_open,
        freq_open,
        mut color_picker,
        under_range,
        under_freq,
    } = popups;
    // The module's own left side; the shared frame adds the stack marker
    // before it and the cluster after it.
    let mut left = h_flex().items_center().gap_2();

    // The module badge uses the secondary surface and its paired text color.
    left = left.child(
        div()
            .px_1p5()
            .rounded(theme.radius_tokens().sm)
            .bg(theme.secondary)
            .text_color(theme.secondary_foreground)
            .text_xs()
            .child("Timeseries"),
    );

    // Derive shared bare-control pointer states once for the triggers and the
    // swatch targets, avoiding repeated contrast calculations.
    let bare_states = control::paint(
        theme,
        control::Rest::Bare,
        theme.background,
        theme.muted_foreground,
    );
    left = left
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

    // One chip per slot, including hidden series.
    for (index, chip) in h.chips.iter().enumerate() {
        // Clear both fill and text styling for an unfilled chip. `for_chip`
        // then chooses bare-control hover colors, and muted text pairs with the
        // tile background rather than relying on contrast over a missing fill.
        let mut paint = chip_paint(theme, chip.tone);
        if !chip.filled {
            paint.fill = None;
            paint.text = theme.muted_foreground;
        }
        let states = control::for_chip(theme, &paint, theme.background);
        let number = chip.number;
        // Replace only the target slot's swatch with an equally sized component
        // trigger. It consumes its press so visibility and chip selection do not also run.
        let picker = color_picker
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
            // Use the visibility target normally, or the component's trigger while a
            // color picker is open for this slot. The component owns its trigger press.
            .child(swatch)
            .child(chip.label.clone())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(chip.axis),
            )
            // Select this slot and let the press reach the shell's tile handler.
            // That handler dismisses the command line, focuses the clicked tile,
            // and arms focus restoration so subsequent keys reach this selection.
            .on_mouse_down(MouseButton::Left, {
                let tile = tile.clone();
                move |_: &MouseDownEvent, _window, cx| {
                    tile.update(cx, |t, cx| t.chip_clicked(index, cx));
                }
            })
            // Right-click selects this slot and opens its action menu.
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
        left = left.child(el);
    }

    let mut cluster = Cluster::new(TileId(tile_id));
    cluster.health = health;
    cluster.menu = Some(MenuTrigger {
        id: ElementId::Name(SharedString::new_static("ts-menu-button")),
        selector: menu_selector,
        tip_selector: SharedString::new_static("tip-timeseries-menu"),
        action: "timeseries::menu",
        open: menu_open,
        on_press: Rc::new({
            let tile = tile.clone();
            move |window, cx| tile.update(cx, |t, cx| t.toggle_menu(window, cx))
        }),
    });
    geode_tile::header::frame(
        stack.and_then(|s| s.marker(theme, TileId(tile_id))),
        left,
        cluster,
        theme,
    )
    .debug_selector(move || format!("timeseries-header-{tile_id}"))
}

/// The notice line under the header: the last refusal or advisory, in
/// the notice door's danger tone as TEXT (no fill — it is a sentence, not
/// a state).
pub(crate) fn render_notice(notice: &SharedString, theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .px_2()
        .text_xs()
        .child(geode_tile::notice::paint(
            notice,
            geode_tile::notice::Tone::Danger,
            theme,
        ))
}

/// Inline expression editor between the header and chart. Parse and reference
/// errors appear beneath the one-line Input and leave the draft open. Model
/// refusals after resolution close the editor and use the tile's notice.
///
/// The loaded-name completion list hangs from the strip's bottom-left
/// corner over the chart, so the chart does not reflow as it grows and
/// shrinks. The strip carries [`crate::popup::EXPR_CONTEXT`] and the
/// listener that takes `tab`/`shift-tab` for completion before the shell
/// sees them.
pub(crate) fn render_expr_field(
    f: &ExprField,
    tile: &Entity<TimeseriesTile>,
    tile_id: u64,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let paint = chip_paint(theme, Tone::DangerText);
    let list = crate::popup::render_expr_list(f, tile, tile_id, cx);
    v_flex()
        .relative()
        .w_full()
        .px_2()
        .py_1()
        .gap_0p5()
        .border_b_1()
        .border_color(theme.border)
        .key_context(crate::popup::EXPR_CONTEXT)
        .on_key_down({
            let tile = tile.clone();
            move |event: &gpui::KeyDownEvent, window, cx| {
                if tile.update(cx, |t, cx| t.expr_key(event, window, cx)) {
                    cx.stop_propagation();
                }
            }
        })
        .child(Input::new(&f.input).appearance(false).w_full())
        .when_some(f.error.clone(), |el, e| {
            el.child(div().text_xs().text_color(paint.text).child(e))
        })
        .when_some(list, |el, list| {
            el.child(div().absolute().left_0().bottom_0().child(list))
        })
}

pub(crate) fn render_footer(hints: &[FooterHint], theme: &Theme) -> impl IntoElement {
    h_flex()
        .w_full()
        .h(scale::design(FOOTER_HEIGHT))
        .items_center()
        .gap_1()
        .px_2()
        .text_xs()
        .text_color(theme.muted_foreground)
        .border_t_1()
        .border_color(theme.border)
        .overflow_hidden()
        .children(
            hints
                .iter()
                .map(|(keys, word)| kbd::hint(keys, word.clone())),
        )
}

/// Empty-chart guidance with Add and Compose buttons. Each button dispatches
/// the same action as its keyboard equivalent.
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
        .child(kbd::marked(EMPTY_HINT))
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
    fn stub(color: &Color) -> gpui::Hsla {
        match color {
            Color::Palette(i) => gpui::hsla(*i as f32 / 10.0, 1.0, 0.5, 1.0),
            Color::Named(_) => gpui::black(),
            Color::Custom(c) => c.to_hsla(),
        }
    }

    fn two() -> Model {
        let mut m = Model::new();
        m.add_source("SPX.close", "demo_kdb", "series").unwrap();
        m.add_source("VIX", "demo_rest", "series").unwrap();
        m
    }

    #[test]
    fn slot_sources_are_the_source_slots_sources_once_each() {
        let mut m = two();
        m.add_source("SPX.open", "demo_kdb", "series").unwrap();
        m.add_expr("SPX.close / VIX", Expr::Ref(1)).unwrap();
        assert_eq!(slot_sources(&m), vec!["demo_kdb", "demo_rest"]);
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
        assert_eq!(h.chips[0].swatch, stub(&Color::Palette(0)));
        assert_eq!(h.chips[1].swatch, stub(&Color::Palette(1)));
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
        m.add_expr("SPX.close / VIX", Expr::Ref(1)).unwrap();
        assert_eq!(cursor_is_source(&m), None);
    }
}
