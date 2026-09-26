//! The top toolbar row (Task 4). User direction: the toolbar IS the native
//! title bar — no separate strip underneath eating extra vertical real
//! estate. Built on gpui-component's [`TitleBar`] (pinned release:
//! `gpui-component-0.6.2/src/title_bar.rs`), which already draws the
//! platform window controls (macOS traffic lights overlay it via
//! `traffic_light_position`; Windows/Linux get real caption buttons) and
//! owns window-drag/double-click — `TitleBar::title_bar_options()` feeds
//! `main.rs`'s `WindowOptions`, and the `window_title` example (upstream
//! repo's `examples/window_title/src/main.rs`) is the reference for
//! putting content inside it via `TitleBar::new().child(...)`.
//!
//! Content: the app title left, the frame readout centered in the
//! previously-reserved middle region (Task 6, spec §4.4), and a
//! right-aligned scope text [`Input`] (Task 4, spec §3.1/§3.11) — every
//! keystroke while it's focused feeds the frame's scope through
//! `ShellView`'s own `InputEvent` subscription; this function only
//! renders it and the chips from the cached [`ScopeBarModel`]. Its focus
//! interplay (click-to-focus, Esc-restores-pre-focus-text, shell chords
//! suppressed while it has focus) lives in `ShellView::handle_key_down` —
//! see that method's filter-focused branch.
//!
//! **The readout is three segments (toolbar restyle, 2026-09-19, user
//! ruling on the mockups' option A):** the AS OF chip when the frame is
//! held at an instant, the grouping readout, and the scope — chips, then
//! verbs — each pair of neighbours parted by an inset hairline
//! ([`Separator`]) at the group gap. What the design guide's audit of
//! the previous bar found, and this layout answers: a selection chip's
//! `×` used to be a sibling at the same gap on both sides, so it belonged
//! to neither chip (it is now INSIDE the chip's frame, a second hit zone
//! that occludes the body's); the `+` and save verbs wore the same filled
//! chip as the data selections (they are bare glyphs now — data is
//! filled, verbs are not); the grouping readout was bare text with no
//! sign it opened a picker (it carries a chevron and a persistent pressed
//! fill while its picker is up); and the warning tint spread across the
//! whole readout when scoped to an instant (it is on the AS OF chip
//! alone — the window stripe and status segment keep the state
//! unmissable, per the same ruling). The `text "…"` chip is gone: the
//! field shows the frame's text while unfocused and clears it with the
//! component's own clear glyph (`Input::cleanable`).
//!
//! **Title-bar controls occlude.** Every pressable element here (the
//! `chip` and `verb` builders, the grouping readout, the field's wrapper)
//! calls `occlude()`. `TitleBar` turns any left press that reaches its own
//! hitbox into a window move on the next mouse move, without asking
//! whether a child handled the press, and on Windows its `Drag` control
//! area answers the platform's caption hit test the same way. An
//! occluding hitbox ends gpui's hit test, so neither sees a press on a
//! control: a drag there belongs to the control (text selection in the
//! field) and only bare title-bar space moves the window. `occlude()`
//! rather than `stop_propagation`, which would leave the caption hit test
//! untouched.
//!
//! The scope's chips are the dimension chips, then one chip per
//! top-level `and` term of the expression (mono text, the same `×`
//! inside), then the contradiction chip. The `+` opens the add-a-filter
//! menu ([`super::addfilter`]) rather than a picker directly.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Div, ElementId, Entity, Hsla, IntoElement, MouseButton, Pixels, SharedString,
    Stateful, Window, div, px,
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
use crate::tips;

/// Compact width of the filter field (brief: "~200px").
const FILTER_WIDTH: f32 = 200.0;

/// The square a chip's `×` and each bare verb glyph occupy, in design
/// pixels — a hit target inside a 20 px chip, and the same box for the
/// `+`/save glyphs so the verbs sit on the chips' centre line.
const GLYPH_BOX: f32 = 14.0;

/// The inset hairline between two segments, in design pixels: shorter
/// than the row so it reads as a segment boundary inside the bar, not a
/// pane divider through it.
const DIVIDER_HEIGHT: f32 = 14.0;

/// One painted chip: a rounded, colored label with a debug selector so
/// e2e tests can find it (`debug_bounds`/`simulate_click`) without this
/// module knowing anything about test infrastructure. `selector` builds
/// its `String` lazily (fix round 1, Finding 2): `debug_selector` is a
/// release no-op that never calls its closure, so an already-`format!`ed
/// `String` handed in here would pay full allocation cost every render
/// for a value release builds never read — matching the `frame-readout`/
/// `scope-asof` selectors two calls away, which build their (static)
/// strings the same lazy way. Takes an `id` (Task 3) so the caller can
/// chain `.tooltip(..)` — a tooltip needs a stable `Stateful<Div>`
/// identity across renders, the same reason every hovered element in
/// this crate (`sidebar.rs`'s discs and profile icon) already carries
/// one. The chip is an `h_flex` so a selection chip can carry its `×` as
/// a second child inside the same frame.
fn chip(
    id: ElementId,
    label: impl Into<SharedString>,
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
        .child(label.into())
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
/// benefit. `grouping_open` is whether the grouping picker is up right
/// now — the readout paints its pressed fill for as long as it is (a
/// control that owns a popup stays visibly pressed until it closes).
/// `add_menu` is the add-a-filter menu's painted panel while it is open
/// ([`super::addfilter::render`]); the `+` hangs it under itself and holds
/// its pressed fill for as long as it is there. `on_term_open` and
/// `on_term_close` take the expression term's index.
#[allow(clippy::too_many_arguments)]
pub fn toolbar(
    filter_input: &Entity<InputState>,
    model: &ScopeBarModel,
    grouping_open: bool,
    add_menu: Option<AnyElement>,
    on_chip_close: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_chip_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_add: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_save: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_grouping: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_as_of: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_term_open: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    on_term_close: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // Muted/foreground for the rest (no raw colours) — the same scheme
    // `Kbd` paints its own key chips with.
    let chip_fg = theme.muted_foreground;
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
        // `Rc<str>`, not `String`: the mouse-down handler, the chip-body
        // selector and the close-glyph selector are three separate
        // `'static` closures, each needing its own owned handle to the
        // column name — an `Rc` clone is a refcount bump, so this is one
        // real allocation (the `Rc::from` below) per chip per render,
        // not three (fix round 1, Finding 2's "the one clone... is
        // unavoidable and fine" — this is that one clone, shared).
        let column: Rc<str> = Rc::from(c.column.as_str());
        let on_close = on_chip_close.clone();
        let on_open = on_chip_open.clone();
        let body_column = column.clone();
        let open_column = column.clone();
        let close_column = column.clone();
        // The chip body opens the dimension picker on this column (Phase
        // 4a §3.3); the `×` INSIDE it drops the dimension. One frame, two
        // hit zones: the `×` calls `occlude()`, which makes its hitbox
        // opaque to the hit test, so while the pointer is on it the
        // body's hitbox is not hovered at all — its mouse-down listener
        // does not fire (gpui gates every mouse listener on
        // `hitbox.is_hovered`), its tooltip does not show and its hover
        // fill does not paint. That one call is the whole reason a click
        // on the `×` drops the chip without ALSO opening the picker;
        // there is no `stop_propagation` beside it to mask its absence.
        // `c.summary`/`c.full`/`c.tip_selector` are all `build_model`'s
        // own fields (fix round 1): attaching the tooltip here costs a
        // `SharedString` clone (a refcount bump, or a stack copy for
        // anything under `SmolStr`'s inline cap) per render, never a
        // fresh `format!`/heap `String` the way the first cut of this
        // task did.
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
        let impossible = chip::chip_paint(theme, chip::Tone::Danger);
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
    // opens the add-a-filter menu (a dimension through the picker, or an
    // expression through the dialog's add mode) — always painted, empty
    // scope or not, since adding a filter is exactly how a scope starts.
    // The menu hangs from a box at the glyph's bottom-left, so it opens
    // directly under the `+`; the glyph holds its pressed fill while the
    // menu is up. No tooltip action: the menu names each row's own key.
    let add_open = add_menu.is_some();
    let mut verbs = h_flex().gap_0p5().items_center().child(
        div()
            .relative()
            .child(
                verb(
                    "scope-pick-chip",
                    Icon::new(CatalogIcon::Plus),
                    chip_fg,
                    glyph_radius,
                    glyph_states,
                    add_open.then_some("scope-pick-chip-open"),
                    || "scope-pick-chip".to_string(),
                )
                .tooltip(tips::tip("tip-scope-pick-chip", "Add a filter", None, None))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    on_add(window, cx)
                }),
            )
            .when_some(add_menu, |el, menu| {
                el.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(scale::design(GLYPH_BOX))
                        .child(menu),
                )
            }),
    );
    if model.savable {
        // The save door: the mouse form of `scope::save_current`,
        // withdrawn rather than merely disabled while the frame has
        // nothing to save (`ScopeBarModel::savable`'s own doc has the
        // reasoning — a verb that always does nothing is worse than no
        // verb). A save icon rather than the word (user ruling
        // 2026-09-19): `Save` is outside `gpui_component::IconName`'s 101,
        // so it is named through the shared catalog and its bytes come
        // from `geode-app`'s `ExtraIcons` source — a shell that paints it
        // under the plain `Assets` alone gets an empty glyph, not a
        // panic, which is why the tooltip still says what it does.
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

    // The grouping readout (2026-09-19) is a control: a click opens the
    // grouping picker, the mouse form of `frame::grouping`/`mod+g`. It
    // reads as the dropdown trigger it is — a trailing chevron, down at
    // rest and up while its picker is open — and while the picker is up
    // it holds the pressed fill instead of answering the pointer (the
    // guide's "a control that owns a popup stays visibly pressed until
    // the popup closes"; selected and open states take no hover, like
    // the active workspace disc). Same `(Bare, title_bar,
    // muted_foreground)` pairing as the verbs, and a tooltip naming the
    // chord.
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
            "Pick a grouping",
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

    // The frame readout (§4.4): AS OF first when scoped to a snapshot
    // rather than the live data, then the grouping, then the scope, a
    // hairline between neighbours. Mono face, matching every other
    // data-adjacent readout in the shell.
    let readout = h_flex()
        .flex_1()
        .justify_center()
        .items_center()
        .gap_3()
        .font_family(fonts::MONO)
        .text_sm()
        .debug_selector(|| "frame-readout".to_string())
        .when_some(
            model.as_of_badge.as_ref().zip(model.as_of_full.as_ref()),
            |el, (badge, full)| {
                // The one warning-toned element on this row, and its own
                // segment: a stray as-of scope must be unmissable, and
                // the window stripe and status segment beside this chip
                // keep it so (user ruling 2026-09-19, retiring the tint
                // that used to spread across the whole readout). Through
                // the chip door, and clickable — the mouse form of
                // `frame::as_of`, the chord its tooltip has always named.
                // `scope-asof` names the chip for tests. `badge`/`full`
                // are both `build_model`'s own finished strings
                // (`ScopeBarModel::as_of_badge`/`as_of_full`) — the
                // tooltip's TITLE is the full resolved timestamp (final
                // review, spec §5.1: a trader hovering to see exactly
                // when must not get the same elided text the badge
                // already shows), and the elided badge text moves to the
                // detail line. Both clones below are refcount bumps,
                // never a fresh `format!`.
                let as_of = chip::chip_paint(theme, chip::Tone::Warning);
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
