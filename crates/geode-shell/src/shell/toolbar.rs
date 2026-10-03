//! The window title bar and scope toolbar. [`TitleBar`] supplies native
//! window controls and window dragging; the shell supplies the app title,
//! frame readout, and scope text [`Input`]. `ShellView` owns the input and
//! its editing session; this module renders the cached [`ScopeBarModel`].
//!
//! The readout's first control is the pin glyph, which toggles whether the
//! active workspace holds its own frame lane; it paints as a selected
//! solid primary chip while pinned and as a bare verb otherwise. The readout
//! then separates pin, as-of, grouping, and scope with inset hairlines.
//! The as-of chip alone uses warning colors. Grouping opens the Grouping
//! dialog and stays visibly pressed while it is open. Filled selection chips contain
//! a separate, occluding close target; add/load/save actions are bare
//! glyphs. The load glyph opens the scope picker and, like the grouping
//! readout, stays pressed while it is open.
//!
//! Scope chips show dimensions, named expressions, top-level expression
//! terms, and any
//! contradiction. The add action opens the Scope dialog. The text input
//! shows the frame's text and supplies its own clear glyph.
//!
//! Pressable controls and the scope input wrapper use `occlude()` so their
//! hitboxes block the title bar's window-drag handling, including the Windows
//! caption hit test. Text selection and other control gestures stay local;
//! dragging bare title-bar space moves the window. Event propagation alone
//! does not block the platform caption hit test.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Div, ElementId, Entity, Hsla, IntoElement, MouseButton, Pixels, SharedString, Stateful,
    Window, div, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::separator::Separator;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar, h_flex};
use gpui_kit_assets::IconName as CatalogIcon;

use super::chip;
use super::control::{self, ControlPaint, PointerStates as _};
use super::scale;
use crate::fonts;
use crate::scopebar::ScopeBarModel;
use crate::tiling::WorkspaceIx;
use crate::tips;

/// Compact width of the filter field in design pixels.
const FILTER_WIDTH: f32 = 200.0;

/// The square a chip's `×` and each bare verb glyph occupy, in design
/// pixels — a hit target inside a 20 px chip, and the same box for the
/// `+`/save glyphs so the verbs sit on the chips' centre line.
pub(super) const GLYPH_BOX: f32 = 14.0;

/// The pin glyph's square in both states, in design pixels: a chip's
/// height, so the filled pinned state reads as a chip and toggling it
/// does not shift the readout.
const PIN_BOX: f32 = 20.0;

/// The inset hairline between two segments, in design pixels: shorter
/// than the row so it reads as a segment boundary inside the bar, not a
/// pane divider through it.
const DIVIDER_HEIGHT: f32 = 14.0;

/// Whether the active workspace holds its own frame lane.
#[derive(Clone, Copy)]
pub struct PinState {
    pub ws: WorkspaceIx,
    pub pinned: bool,
}

/// Tooltip titles by workspace, static so hovering allocates nothing.
const PIN_TITLES: [&str; 9] = [
    "Pin the frame to workspace 1",
    "Pin the frame to workspace 2",
    "Pin the frame to workspace 3",
    "Pin the frame to workspace 4",
    "Pin the frame to workspace 5",
    "Pin the frame to workspace 6",
    "Pin the frame to workspace 7",
    "Pin the frame to workspace 8",
    "Pin the frame to workspace 9",
];
const PINNED_TITLES: [&str; 9] = [
    "Frame pinned to workspace 1",
    "Frame pinned to workspace 2",
    "Frame pinned to workspace 3",
    "Frame pinned to workspace 4",
    "Frame pinned to workspace 5",
    "Frame pinned to workspace 6",
    "Frame pinned to workspace 7",
    "Frame pinned to workspace 8",
    "Frame pinned to workspace 9",
];
const PIN_HINT: &str = "pinning keeps scope, grouping, and as-of changes in this workspace";
const PINNED_HINT: &str =
    "scope, grouping, and as-of changes stay here · click to rejoin the shared frame";

/// A labeled chip with stable identity for tooltips and pointer state.
/// The lazy debug selector allocates only when test instrumentation reads
/// it. Horizontal layout keeps a selection's close target inside its frame.
fn chip(
    id: ElementId,
    label: impl IntoElement,
    fg: Hsla,
    bg: Hsla,
    radius: Pixels,
    selector: impl Fn() -> String + 'static,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        // A title-bar control: `occlude()` keeps a press here from
        // reaching `TitleBar`'s drag surface (module doc: title-bar controls).
        .occlude()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded(radius)
        .bg(bg)
        .text_color(fg)
        .child(label)
        .debug_selector(selector)
}

/// A bare verb glyph on the title bar — the `+` add-a-filter door, the
/// save door — the toolbar's answer to the guide's ghost button: no fill
/// at rest, the control door's hover and pressed fills, the icon
/// inheriting the box's text so it recolours with it. Data is a filled
/// chip; a verb is not, which is what tells the two apart at a glance.
/// `open` holds the pressed fill instead of answering the pointer, for a
/// verb whose popup is up (the grouping readout's rule).
fn verb(
    id: impl Into<ElementId>,
    icon: Icon,
    fg: Hsla,
    radius: Pixels,
    states: ControlPaint,
    open: Option<&'static str>,
    selector: impl Fn() -> String + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        // A title-bar control (module doc: title-bar controls).
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .size(scale::design(GLYPH_BOX))
        .rounded(radius)
        .text_color(fg)
        .debug_selector(selector)
        .map(|el| match open {
            // `open` is the pressed state's own debug selector, on the
            // glyph painted inside the pressed fill (the grouping
            // readout's `scope-grouping-open` chevron is the same guard).
            Some(open_selector) => el.bg(states.pressed).text_color(states.pressed_text).child(
                div()
                    .flex()
                    .debug_selector(move || open_selector.to_string())
                    .child(icon.small()),
            ),
            None => el.pointer_states(states).child(icon.small()),
        })
}

/// The inset hairline between two segments. gpui-component's
/// [`Separator`] inside a sized box: the component owns the line, the
/// box owns the height and the debug selector a window test measures
/// the segmentation by. `colour` is `theme.title_bar_border` — the token
/// the bar's own bottom rule is drawn in (pinned `TitleBar`), so the
/// hairline matches the surface it sits on rather than the panel
/// `border` the component would default to.
fn divider(selector: &'static str, colour: Hsla) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(1.))
        .h(scale::design(DIVIDER_HEIGHT))
        .debug_selector(move || selector.to_string())
        .child(Separator::vertical().color(colour))
}

/// Separate mouse doors rather than a bundling struct (clippy's
/// `too_many_arguments`, `-D warnings`-enforced) — `status_bar`'s own
/// reasoning: `render.rs`'s one call site builds each as its own
/// `cx.entity()`-capturing closure, and this body hands each to exactly
/// one element, so a struct would only move the assembly for no reader
/// benefit. `grouping_open` is whether the Grouping dialog is up right
/// now — the readout paints its pressed fill for as long as it is (a
/// control that owns a popup stays visibly pressed until it closes);
/// `scope_open` is the same for the scope picker and the load glyph, which
/// `on_load` opens.
/// `scope_dialog_open` is whether the Scope dialog is up; the `+` that
/// opens it holds its pressed fill for as long as it is. `on_term_open` and
/// `on_term_close` take the expression term's index; `on_named_open` and
/// `on_named_close` take the named expression's name. `pin` is the active
/// workspace and whether it is pinned; `on_pin` toggles that pin.
#[allow(clippy::too_many_arguments)]
pub fn toolbar(
    filter_input: &Entity<InputState>,
    model: &ScopeBarModel,
    grouping_open: bool,
    scope_open: bool,
    scope_dialog_open: bool,
    on_chip_close: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_chip_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_add: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_save: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_load: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_grouping: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_as_of: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_term_open: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    on_term_close: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    on_named_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_named_close: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    pin: PinState,
    on_pin: impl Fn(&mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // Muted/foreground for the rest (no raw colours) — the same scheme
    // `Kbd` paints its own key chips with.
    let chip_fg = chip::text_on(theme.muted_foreground, Some(theme.muted), theme.title_bar);
    let chip_bg = theme.muted;
    let chip_radius = theme.radius;
    let glyph_radius = theme.radius_tokens().sm;
    // Pointer states for the clickable chips (the selection chips) and
    // for everything bare on the title bar — the `×` inside a chip, the
    // grouping readout, the `+` and save verbs. The `×` takes the CHIP's
    // states, not the bare ones: it occludes the body's hitbox, so while
    // the pointer is on it the body sits at its rest fill, and the `×`'s
    // hover has to be distinct from THAT — exactly what the chip pairing
    // measures. The contradiction chip has no listener and takes none —
    // a hover fill promises a click (design guide, interaction states).
    // The expression term chips and their `×`s take the same
    // `chip_states` pairing as the dimension chips.
    let chip_states = control::paint(
        theme,
        control::Rest::Filled(chip_bg),
        theme.title_bar,
        chip_fg,
    );
    let glyph_states = control::paint(theme, control::Rest::Bare, theme.title_bar, chip_fg);

    let mut chips_row = h_flex().gap_1().items_center();
    let mut has_chips = false;
    for (i, c) in model.chips.iter().enumerate() {
        has_chips = true;
        // Share one owned column name among the mouse handler and selectors.
        // Cloning these `Rc` handles increments a count without copying text.
        let column: Rc<str> = Rc::from(c.column.as_str());
        let on_close = on_chip_close.clone();
        let on_open = on_chip_open.clone();
        let body_column = column.clone();
        let open_column = column.clone();
        let close_column = column.clone();
        // The body opens the dimension picker; the close target occludes it
        // so closing cannot also open the picker or show its hover/tooltip.
        // Tooltip strings come from the cached model and clone shared handles.
        chips_row = chips_row.child(
            chip(
                ElementId::NamedInteger(SharedString::new_static("scope-chip"), i as u64),
                c.summary.clone(),
                chip_fg,
                chip_bg,
                chip_radius,
                move || format!("scope-chip-{body_column}"),
            )
            // The frame's right padding shrinks to the `×`'s own inset
            // so the glyph's box sits flush inside the chip's edge.
            .pr_0p5()
            .tooltip(tips::tip_with(
                c.tip_selector.clone(),
                c.full.clone(),
                Some("frame::pick"),
                Some(SharedString::new_static("click: pick values")),
            ))
            .pointer_states(chip_states)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_open(&open_column, window, cx)
            })
            .child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("scope-chip-close"),
                        i as u64,
                    ))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(scale::design(GLYPH_BOX))
                    .rounded(glyph_radius)
                    .text_color(chip_fg)
                    // The icon inherits the box's text rather than pinning
                    // its own, so a hover recolours glyph and box together
                    // (`control::PointerStates`).
                    .child(Icon::new(IconName::Close).small())
                    .debug_selector(move || format!("scope-chip-close-{close_column}"))
                    .occlude()
                    .pointer_states(chip_states)
                    .tooltip(tips::tip_with(
                        c.close_selector.clone(),
                        c.close_title.clone(),
                        None,
                        None,
                    ))
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        on_close(&column, window, cx)
                    }),
            ),
        );
    }
    // Broken names (missing or invalid) take the danger chip: a tile
    // scoped by one refuses to query, so it must read as an error. Its
    // `×` pairs with the danger fill as the muted chips' `×` pairs with
    // theirs.
    let broken = chip::chip_paint_on(theme, chip::Tone::Danger, theme.title_bar);
    let broken_fill = broken.fill.unwrap_or(theme.danger);
    let broken_states = control::for_chip(theme, &broken, theme.title_bar);
    for named in &model.named {
        has_chips = true;
        // One chip per named expression, after the dimension chips and
        // before the expression terms. The body opens the Expressions
        // dialog on the name; the `×` drops the name and occludes the
        // body's hitbox, as a dimension chip's does, so a click on it
        // cannot also open the dialog. Ids derive from the name, so a chip
        // keeps its pointer state when a neighbour is removed.
        //
        // A broken chip's label carries its own selector, chosen in the same
        // branch as the danger paint, so a window test that finds the
        // selector has found the danger tone.
        // The body and its `×` share one pairing measured against the
        // chip's own fill, the muted chips' rule.
        let (fg, bg, states, broken_marker) = if named.broken {
            let marker = named.name.clone();
            (broken.text, broken_fill, broken_states, Some(marker))
        } else {
            (chip_fg, chip_bg, chip_states, None)
        };
        let on_open = on_named_open.clone();
        let on_close = on_named_close.clone();
        let open_name = named.name.clone();
        let name = named.name.clone();
        let body_selector = named.selector.clone();
        let named_close_selector = named.close_selector.clone();
        let label = div()
            .child(named.label.clone())
            .when_some(broken_marker, |el, marker| {
                el.debug_selector(move || format!("scope-named-chip-broken-{marker}"))
            });
        chips_row = chips_row.child(
            chip(
                ElementId::Name(named.selector.clone()),
                label,
                fg,
                bg,
                chip_radius,
                move || body_selector.to_string(),
            )
            .pr_0p5()
            .tooltip(tips::tip_with(
                named.tip_selector.clone(),
                named.full.clone(),
                None,
                Some(SharedString::new_static("click: edit this expression")),
            ))
            .pointer_states(states)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_open(&open_name, window, cx)
            })
            .child(
                div()
                    .id(ElementId::Name(named.close_selector.clone()))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(scale::design(GLYPH_BOX))
                    .rounded(glyph_radius)
                    .text_color(fg)
                    .child(Icon::new(IconName::Close).small())
                    .debug_selector(move || named_close_selector.to_string())
                    .occlude()
                    .pointer_states(states)
                    .tooltip(tips::tip_with(
                        named.close_tip_selector.clone(),
                        named.close_title.clone(),
                        None,
                        None,
                    ))
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        on_close(&name, window, cx)
                    }),
            ),
        );
    }
    for (i, term) in model.terms.iter().enumerate() {
        has_chips = true;
        // One chip per top-level `and` term of the expression, after the
        // dimension chips and in the same mould: the body opens the
        // expression dialog on this term alone, the `×` inside it drops
        // this term alone. The `×` `occlude()`s the body's hitbox exactly
        // as a dimension chip's does (see the comment there), which is the
        // whole reason a click on it does not ALSO open the dialog. The
        // text is the mono face — expression source is code — named
        // through `fonts::MONO` here rather than inherited from the
        // readout, so the chip reads as code wherever it is placed.
        let on_open = on_term_open.clone();
        let on_close = on_term_close.clone();
        let body_selector = term.selector.clone();
        let close_selector = term.close_selector.clone();
        chips_row = chips_row.child(
            chip(
                ElementId::NamedInteger(SharedString::new_static("scope-expr-chip"), i as u64),
                term.label.clone(),
                chip_fg,
                chip_bg,
                chip_radius,
                move || body_selector.to_string(),
            )
            .font_family(fonts::MONO)
            .pr_0p5()
            .tooltip(tips::tip_with(
                term.tip_selector.clone(),
                term.full.clone(),
                None,
                Some(SharedString::new_static("click: edit this term")),
            ))
            .pointer_states(chip_states)
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_open(i, window, cx)
            })
            .child(
                div()
                    .id(ElementId::NamedInteger(
                        SharedString::new_static("scope-expr-chip-close"),
                        i as u64,
                    ))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(scale::design(GLYPH_BOX))
                    .rounded(glyph_radius)
                    .text_color(chip_fg)
                    .child(Icon::new(IconName::Close).small())
                    .debug_selector(move || close_selector.to_string())
                    .occlude()
                    .pointer_states(chip_states)
                    .tooltip(tips::tip_with(
                        term.close_tip_selector.clone(),
                        SharedString::new_static("Remove this term"),
                        None,
                        None,
                    ))
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        on_close(i, window, cx)
                    }),
            ),
        );
    }
    if let Some(named) = &model.impossible {
        has_chips = true;
        // The contradiction chip: `chip::Tone::Danger`, not the muted
        // scheme every other chip uses — a scope that can match nothing
        // must read as an error, not routine state. Through the chip door
        // rather than `danger_foreground` over the tint by hand: that
        // token is the background family at the pinned rev, under 3:1 on
        // 31 of 44 bundled themes over its own 25% tint.
        let impossible = chip::chip_paint_on(theme, chip::Tone::Danger, theme.title_bar);
        chips_row = chips_row.child(
            chip(
                "scope-impossible-chip".into(),
                named.clone(),
                impossible.text,
                impossible.fill.unwrap_or(theme.danger),
                chip_radius,
                || "scope-impossible-chip".to_string(),
            )
            .tooltip(tips::tip(
                "tip-scope-impossible-chip",
                "No row can match: two scope layers select disjoint values on this dimension",
                None,
                None,
            )),
        );
    }

    // The verbs, `gap_0p5` apart and one group gap after the chips. The `+`
    // opens the Scope dialog (`frame::scope`), where every ingredient is
    // added — always painted, empty scope or not, since adding a filter is
    // exactly how a scope starts. It holds its pressed fill while the
    // dialog is up, as the grouping readout does for its dialog.
    let mut verbs = h_flex().gap_0p5().items_center().child(
        verb(
            "scope-pick-chip",
            Icon::new(CatalogIcon::Plus),
            chip_fg,
            glyph_radius,
            glyph_states,
            scope_dialog_open.then_some("scope-pick-chip-open"),
            || "scope-pick-chip".to_string(),
        )
        .tooltip(tips::tip(
            "tip-scope-pick-chip",
            "Scope",
            Some("frame::scope"),
            None,
        ))
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            on_add(window, cx)
        }),
    );
    // The load glyph opens the saved-scope chooser. Always
    // painted: loading a saved scope is as useful on an empty scope as on
    // a full one, and the picker says how to save one when none exist. It
    // sits before the conditional save glyph so save appearing never moves
    // it, and it holds its pressed fill while the picker is up, as the
    // grouping readout does. `FolderOpen` is in the default icon bundle.
    verbs = verbs.child(
        verb(
            "scope-load-chip",
            Icon::new(CatalogIcon::FolderOpen),
            chip_fg,
            glyph_radius,
            glyph_states,
            scope_open.then_some("scope-load-chip-open"),
            || "scope-load-chip".to_string(),
        )
        .tooltip(tips::tip(
            "tip-scope-load-chip",
            "Load a named scope",
            // No chord: `frame::scope` opens the Scope dialog, not this
            // chooser, and the chooser has no action of its own yet.
            None,
            None,
        ))
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            on_load(window, cx)
        }),
    );
    if model.savable {
        // Show Save only when the scope is savable. Its icon comes from the
        // shared catalog and requires the app's `ExtraIcons` asset source; the
        // plain component assets do not include it.
        verbs = verbs.child(
            verb(
                "scope-save-chip",
                Icon::new(CatalogIcon::Save),
                chip_fg,
                glyph_radius,
                glyph_states,
                None,
                || "scope-save-chip".to_string(),
            )
            .tooltip(tips::tip(
                "tip-scope-save-chip",
                "Save as a named scope",
                Some("scope::save_current"),
                None,
            ))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_save(window, cx)
            }),
        );
    }
    let scope_segment = h_flex()
        .items_center()
        .gap_2()
        .when(has_chips, |el| el.child(chips_row))
        .child(verbs);

    // The Grouping dialog trigger retains pressed styling while open and
    // uses a chevron to expose that state. Closed, it shares the bare
    // control pointer colors used by the toolbar actions.
    let grouping = h_flex()
        .id("scope-grouping")
        // A title-bar control (module doc: title-bar controls).
        .occlude()
        .items_center()
        .gap_1()
        .pl_1p5()
        .pr_1()
        .py_0p5()
        .rounded(chip_radius)
        .text_color(chip_fg)
        .debug_selector(|| "scope-grouping".to_string())
        .tooltip(tips::tip(
            "tip-scope-grouping",
            "Grouping",
            Some("frame::grouping"),
            None,
        ))
        .map(|el| {
            if grouping_open {
                el.bg(glyph_states.pressed)
                    .text_color(glyph_states.pressed_text)
            } else {
                el.pointer_states(glyph_states)
            }
        })
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            on_grouping(window, cx)
        })
        .child(model.slot_label.clone())
        .child(if grouping_open {
            div()
                .flex()
                .child(Icon::new(CatalogIcon::ChevronUp).small())
                .debug_selector(|| "scope-grouping-open".to_string())
        } else {
            div()
                .flex()
                .child(Icon::new(CatalogIcon::ChevronDown).small())
                .debug_selector(|| "scope-grouping-chevron".to_string())
        });

    let i = usize::from(pin.ws.get() - 1);
    let (title, hint) = if pin.pinned {
        (PINNED_TITLES[i], PINNED_HINT)
    } else {
        (PIN_TITLES[i], PIN_HINT)
    };
    // Pinned paints solid in the theme's primary (`Tone::Active`) so the
    // on state reads at a glance; unpinned is a bare verb like `+` and save.
    let (pin_fg, pin_bg, pin_states) = if pin.pinned {
        let pinned_paint = chip::chip_paint_on(theme, chip::Tone::Active, theme.title_bar);
        (
            pinned_paint.text,
            pinned_paint.fill,
            control::for_chip(theme, &pinned_paint, theme.title_bar),
        )
    } else {
        (chip_fg, None, glyph_states)
    };
    let pin_glyph = div()
        .id("scope-pin")
        .flex()
        .items_center()
        .justify_center()
        .size(scale::design(PIN_BOX))
        .rounded(chip_radius)
        .text_color(pin_fg)
        .when_some(pin_bg, |el, bg| el.bg(bg))
        .child(Icon::new(CatalogIcon::Pin).small())
        .debug_selector(|| "scope-pin".to_string())
        // A title-bar control (module doc: title-bar controls).
        .occlude()
        .pointer_states(pin_states)
        .tooltip(tips::tip_with(
            SharedString::new_static("tip-scope-pin"),
            SharedString::new_static(title),
            Some("frame::pin_workspace"),
            Some(SharedString::new_static(hint)),
        ))
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            on_pin(window, cx)
        });

    // The frame readout: the pin glyph, then AS OF when scoped to a
    // snapshot rather than the live data, then the grouping, then the
    // scope, a hairline between neighbours. Mono face, matching every
    // other data-adjacent readout in the shell.
    let readout = h_flex()
        .flex_1()
        .justify_center()
        .items_center()
        .gap_3()
        .font_family(fonts::MONO)
        .text_sm()
        .debug_selector(|| "frame-readout".to_string())
        .child(pin_glyph)
        .child(divider("scope-divider-pin", theme.title_bar_border))
        .when_some(
            model.as_of_badge.as_ref().zip(model.as_of_full.as_ref()),
            |el, (badge, full)| {
                // The as-of chip opens its picker and carries the row's warning tone.
                // The tooltip title shows the full timestamp; the badge uses the
                // compact label. Both strings are shared values from the cached model.
                let as_of = chip::chip_paint_on(theme, chip::Tone::Warning, theme.title_bar);
                let as_of_states = control::for_chip(theme, &as_of, theme.title_bar);
                let on_as_of = on_as_of.clone();
                el.child(
                    chip(
                        "scope-asof".into(),
                        badge.clone(),
                        as_of.text,
                        as_of.fill.unwrap_or(theme.warning),
                        chip_radius,
                        || "scope-asof".to_string(),
                    )
                    .tooltip(tips::tip_with(
                        SharedString::new_static("tip-scope-asof"),
                        full.clone(),
                        Some("frame::as_of"),
                        Some(badge.clone()),
                    ))
                    .pointer_states(as_of_states)
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        on_as_of(window, cx)
                    }),
                )
                .child(divider("scope-divider-asof", theme.title_bar_border))
            },
        )
        .child(grouping)
        .child(divider("scope-divider-scope", theme.title_bar_border))
        .child(scope_segment);

    TitleBar::new().child(
        h_flex()
            .w_full()
            .items_center()
            .child(div().text_color(theme.foreground).child("geode"))
            .child(readout)
            // A muted search icon in the `prefix` slot rather than a
            // "filter" placeholder (user direction), matching the palette
            // and the dialogs' shared `dialog::filter_row` — every text
            // field in the shell names itself the same way. This one keeps
            // `Input`'s own chrome (no `appearance(false)`), since it sits
            // on the title bar rather than inside a panel that already
            // draws a border for it. `cleanable`: the component's own
            // clear glyph paints while the field has text and clears it
            // through `InputState::clean` → `InputEvent::Change`, the same
            // subscription typing feeds, so the text layer drops through
            // one door (it also focuses the field, which opens a session
            // over the now-empty text; `escape` restores that empty base,
            // never the cleared text). The wrapper carries the selector a
            // window test measures the field by — `Input` has none.
            .child(
                // A title-bar control (module doc: title-bar controls): a drag
                // in the field selects text.
                div()
                    .occlude()
                    .debug_selector(|| "scope-field".to_string())
                    .child(
                        Input::new(filter_input)
                            .prefix(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                            .cleanable(true)
                            .w(scale::design(FILTER_WIDTH)),
                    ),
            ),
    )
}
