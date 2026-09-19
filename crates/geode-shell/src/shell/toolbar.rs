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
//! previously-reserved middle region (Task 6, spec §4.4 — slot, scope
//! chips, and an unmissable AS OF badge when scoped to a snapshot), and a
//! right-aligned scope text [`Input`] (Task 4, spec §3.1/§3.11) — every
//! keystroke while it's focused feeds the frame's scope through
//! `ShellView`'s own `InputEvent` subscription; this function only
//! renders it and the chips from the cached [`ScopeBarModel`]. Its focus
//! interplay (click-to-focus, Esc-restores-pre-focus-text, shell chords
//! suppressed while it has focus) lives in `ShellView::handle_key_down` —
//! see that method's filter-focused branch.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Div, ElementId, Entity, Hsla, IntoElement, MouseButton, Pixels, SharedString, Stateful,
    Window, div,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, TitleBar, h_flex};

use super::chip;
use super::scale;
use crate::fonts;
use crate::scopebar::ScopeBarModel;
use crate::tips;

/// Compact width of the filter field (brief: "~200px").
const FILTER_WIDTH: f32 = 200.0;

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
/// one.
fn chip(
    id: ElementId,
    label: impl Into<SharedString>,
    fg: Hsla,
    bg: Hsla,
    radius: Pixels,
    selector: impl Fn() -> String + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded(radius)
        .bg(bg)
        .text_color(fg)
        .child(label.into())
        .debug_selector(selector)
}

pub fn toolbar(
    filter_input: &Entity<InputState>,
    model: &ScopeBarModel,
    on_chip_close: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_chip_open: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    on_pick: impl Fn(&mut Window, &mut App) + Clone + 'static,
    on_save: impl Fn(&mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // Muted/foreground for the rest (no raw colours) — the same scheme
    // `keybindings_view::key_chip` uses for its own chips.
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;
    let chip_radius = theme.radius;

    let mut chips_row = h_flex().gap_1().items_center();
    for (i, c) in model.chips.iter().enumerate() {
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
        chips_row = chips_row.child(
            h_flex()
                .items_center()
                .gap_1()
                .child(
                    // The chip body opens the dimension picker on this
                    // column (Phase 4a §3.3) — the close glyph below
                    // stays a separate hit target so clicking it drops
                    // the dimension instead of opening the picker.
                    // `c.summary`/`c.full`/`c.tip_selector` are all
                    // `build_model`'s own fields (fix round 1): attaching
                    // the tooltip here costs a `SharedString` clone (a
                    // refcount bump, or a stack copy for anything under
                    // `SmolStr`'s inline cap) per render, never a fresh
                    // `format!`/heap `String` the way the first cut of
                    // this task did.
                    chip(
                        ElementId::NamedInteger(SharedString::new_static("scope-chip"), i as u64),
                        c.summary.clone(),
                        chip_fg,
                        chip_bg,
                        chip_radius,
                        move || format!("scope-chip-{body_column}"),
                    )
                    .tooltip(tips::tip_with(
                        c.tip_selector.clone(),
                        c.full.clone(),
                        Some("frame::pick"),
                        Some(SharedString::new_static("click: pick values")),
                    ))
                    .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                        on_open(&open_column, window, cx)
                    }),
                )
                .child(
                    div()
                        .id(ElementId::NamedInteger(
                            SharedString::new_static("scope-chip-close"),
                            i as u64,
                        ))
                        .child(Icon::new(IconName::Close).text_color(chip_fg))
                        .debug_selector(move || format!("scope-chip-close-{close_column}"))
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
    if let Some(t) = &model.text_chip {
        chips_row = chips_row.child(
            chip(
                "scope-text-chip".into(),
                t.clone(),
                chip_fg,
                chip_bg,
                chip_radius,
                || "scope-text-chip".to_string(),
            )
            .tooltip(tips::tip_with(
                SharedString::new_static("tip-scope-text-chip"),
                model.text_tip.clone().unwrap_or_default(),
                Some("frame::focus_text"),
                None,
            )),
        );
    }
    if let Some(expr) = &model.expr {
        chips_row = chips_row.child(
            chip(
                "scope-expr-chip".into(),
                expr.clone(),
                chip_fg,
                chip_bg,
                chip_radius,
                || "scope-expr-chip".to_string(),
            )
            .tooltip(tips::tip_with(
                SharedString::new_static("tip-scope-expr-chip"),
                model.expr_full.clone().unwrap_or_default(),
                None,
                Some(SharedString::new_static(":filter <expr> sets it")),
            )),
        );
    }
    // The `+` pick chip (scope-save spec's amendment): a mouse door onto
    // the dimension picker (`mod+p`/`frame::pick`) for a trader who has
    // not memorised the chord — always painted, empty scope or not,
    // since picking a dimension is exactly how a scope starts.
    chips_row = chips_row.child(
        chip(
            "scope-pick-chip".into(),
            SharedString::new_static("+"),
            chip_fg,
            chip_bg,
            chip_radius,
            || "scope-pick-chip".to_string(),
        )
        .tooltip(tips::tip(
            "tip-scope-pick-chip",
            "Pick a dimension",
            Some("frame::pick"),
            None,
        ))
        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            on_pick(window, cx)
        }),
    );
    if model.savable {
        // The `save` chip: the mouse form of `scope::save_current`,
        // withdrawn rather than merely disabled while the frame has
        // nothing to save (`ScopeBarModel::savable`'s own doc has the
        // reasoning — a chip that always does nothing is worse than no
        // chip).
        chips_row = chips_row.child(
            chip(
                "scope-save-chip".into(),
                SharedString::new_static("save"),
                chip_fg,
                chip_bg,
                chip_radius,
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
    if let Some(named) = &model.impossible {
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
    // `chips_row` used to be conditionally omitted when the frame had
    // nothing to show (`has_chips`, now gone) — the `+` pick chip above
    // is always painted, so the row is never empty any more.
    TitleBar::new().child(
        h_flex()
            .w_full()
            .items_center()
            .child(div().text_color(theme.foreground).child("geode"))
            // The frame readout (§4.4): slot + label, the scope chips,
            // and — when scoped to a snapshot rather than the live data —
            // an unmissable AS OF badge, deliberately the one warning-
            // toned element on this row (a stray as-of scope is exactly
            // the kind of thing that must never go unnoticed). Mono face,
            // matching every other data-adjacent readout in the shell.
            .child(
                h_flex()
                    .flex_1()
                    .justify_center()
                    .gap_3()
                    .font_family(fonts::MONO)
                    .text_sm()
                    .debug_selector(|| "frame-readout".to_string())
                    .when_some(
                        model.as_of_badge.as_ref().zip(model.as_of_full.as_ref()),
                        |el, (badge, full)| {
                            // The existing warning treatment on the whole
                            // bar (slot + chips + badge) — a stray as-of
                            // scope must be unmissable, not a small badge
                            // easy to miss at the edge of the eye.
                            // `scope-asof` names the badge text itself
                            // for tests. `badge`/`full` are both
                            // `build_model`'s own finished strings
                            // (`ScopeBarModel::as_of_badge`/`as_of_full`)
                            // — the tooltip's TITLE is the full resolved
                            // timestamp (final review, spec §5.1: a
                            // trader hovering to see exactly when must
                            // not get the same elided text the badge
                            // already shows), and the elided badge text
                            // moves to the detail line. Both clones below
                            // are refcount bumps, never a fresh `format!`.
                            let as_of = chip::chip_paint(theme, chip::Tone::Warning);
                            el.when_some(as_of.fill, |el, fill| el.bg(fill))
                                .px_2()
                                .rounded(theme.radius)
                                .child(
                                    div()
                                        .id("scope-asof")
                                        .text_color(as_of.text)
                                        .debug_selector(|| "scope-asof".to_string())
                                        .tooltip(tips::tip_with(
                                            SharedString::new_static("tip-scope-asof"),
                                            full.clone(),
                                            Some("frame::as_of"),
                                            Some(badge.clone()),
                                        ))
                                        .child(badge.clone()),
                                )
                        },
                    )
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(model.slot_label.clone()),
                    )
                    .child(chips_row),
            )
            // A muted search icon in the `prefix` slot rather than a
            // "filter" placeholder (user direction), matching the palette
            // and the dialogs' shared `dialog::filter_row` — every text
            // field in the shell names itself the same way. This one keeps
            // `Input`'s own chrome (no `appearance(false)`), since it sits
            // on the title bar rather than inside a panel that already
            // draws a border for it.
            .child(
                Input::new(filter_input)
                    .prefix(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                    .w(scale::design(FILTER_WIDTH)),
            ),
    )
}
