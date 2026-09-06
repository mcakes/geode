//! The top toolbar row (Task 4). User direction: the toolbar IS the native
//! title bar — no separate strip underneath eating extra vertical real
//! estate. Built on gpui-component's [`TitleBar`] (pinned checkout:
//! `crates/ui/src/title_bar.rs`), which already draws the platform window
//! controls (macOS traffic lights overlay it via `traffic_light_position`;
//! Windows/Linux get real caption buttons) and owns window-drag/double-
//! click — `TitleBar::title_bar_options()` feeds `main.rs`'s
//! `WindowOptions`, and the `window_title` example
//! (`examples/window_title/src/main.rs`) is the reference for putting
//! content inside it via `TitleBar::new().child(...)`.
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

use gpui::prelude::*;
use gpui::{App, Div, Entity, Hsla, IntoElement, MouseButton, Window, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, TitleBar, h_flex};

use crate::fonts;
use crate::scopebar::ScopeBarModel;

/// Compact width of the filter field (brief: "~200px").
const FILTER_WIDTH: f32 = 200.0;

/// One painted chip: a rounded, colored label with a debug selector so
/// e2e tests can find it (`debug_bounds`/`simulate_click`) without this
/// module knowing anything about test infrastructure.
fn chip(label: String, fg: Hsla, bg: Hsla, selector: String) -> Div {
    div()
        .px_2()
        .py_0p5()
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .child(label)
        .debug_selector(move || selector.clone())
}

pub fn toolbar(
    filter_input: &Entity<InputState>,
    model: &ScopeBarModel,
    on_chip_close: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // Muted/foreground for the rest (no raw colours) — the same scheme
    // `keybindings_view::key_chip` uses for its own chips.
    let chip_fg = theme.muted_foreground;
    let chip_bg = theme.muted;

    let mut chips_row = h_flex().gap_1().items_center();
    for c in &model.chips {
        let column = c.column.clone();
        let close_selector = format!("scope-chip-close-{column}");
        let on_close = on_chip_close.clone();
        chips_row = chips_row.child(
            h_flex()
                .items_center()
                .gap_1()
                .child(chip(
                    c.summary.clone(),
                    chip_fg,
                    chip_bg,
                    format!("scope-chip-{column}"),
                ))
                .child(
                    // Chip body click is inert this task (Task 5 wires it
                    // to the picker); only this close glyph acts.
                    div()
                        .child(Icon::new(IconName::Close).text_color(chip_fg))
                        .debug_selector(move || close_selector.clone())
                        .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                            on_close(&column, window, cx)
                        }),
                ),
        );
    }
    if let Some(t) = &model.text {
        chips_row = chips_row.child(chip(
            format!("text \"{t}\""),
            chip_fg,
            chip_bg,
            "scope-text-chip".to_string(),
        ));
    }
    if let Some(expr) = &model.expr {
        chips_row = chips_row.child(chip(
            expr.clone(),
            chip_fg,
            chip_bg,
            "scope-expr-chip".to_string(),
        ));
    }
    if let Some(named) = &model.impossible {
        // The contradiction chip: `theme.danger`/`danger_foreground`, not
        // the muted scheme every other chip uses — a scope that can match
        // nothing must read as an error, not routine state.
        chips_row = chips_row.child(chip(
            named.clone(),
            theme.danger_foreground,
            theme.danger.opacity(0.25),
            "scope-impossible-chip".to_string(),
        ));
    }
    let has_chips = !model.chips.is_empty()
        || model.text.is_some()
        || model.expr.is_some()
        || model.impossible.is_some();

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
                    .when_some(model.as_of.as_ref(), |el, t| {
                        // The existing warning treatment on the whole bar
                        // (slot + chips + badge) — a stray as-of scope
                        // must be unmissable, not a small badge easy to
                        // miss at the edge of the eye. `scope-asof` names
                        // the badge text itself for tests.
                        el.bg(theme.warning.opacity(0.25))
                            .px_2()
                            .rounded(px(4.))
                            .child(
                                div()
                                    .text_color(theme.warning_foreground)
                                    .debug_selector(|| "scope-asof".to_string())
                                    .child(format!("AS OF {t}")),
                            )
                    })
                    .child(
                        div()
                            .text_color(theme.muted_foreground)
                            .child(match &model.slot {
                                Some((n, label)) => format!("{n} · {label}"),
                                None => "view default".to_string(),
                            }),
                    )
                    .when(has_chips, |el| el.child(chips_row)),
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
                    .w(px(FILTER_WIDTH)),
            ),
    )
}
