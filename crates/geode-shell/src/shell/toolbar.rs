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
//! summary, and an unmissable AS OF badge when scoped to a snapshot), and
//! a right-aligned filter [`Input`] to prove the row hosts real content
//! (user direction). The filter is deliberately **inert**: nothing reads
//! its value yet — it becomes the global text filter (spec §4.1) once the
//! data phase wires a consumer to the `InputState` `ShellView` owns. Its
//! focus interplay (click-to-focus, Esc-back-to-shell-root, shell chords
//! suppressed while it has focus) lives in `ShellView::handle_key_down` —
//! this function only renders it.

use gpui::prelude::*;
use gpui::{App, Entity, IntoElement, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, TitleBar, h_flex};

use crate::fonts;
use crate::scopebar::ScopeBarModel;

/// Compact width of the filter field (brief: "~200px").
const FILTER_WIDTH: f32 = 200.0;

pub fn toolbar(
    filter_input: &Entity<InputState>,
    model: &ScopeBarModel,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();

    // Task 3: chips render as one joined summary line, the same shape
    // `Frame::readout` (deleted this task) used to build — Task 4 gives
    // each chip its own painted element; this keeps the toolbar showing
    // the same information in the meantime.
    let mut parts: Vec<String> = model.chips.iter().map(|c| c.summary.clone()).collect();
    if let Some(t) = &model.text {
        parts.push(format!("text \"{t}\""));
    }
    if model.expr.is_some() {
        parts.push("expr".to_string());
    }
    if let Some(named) = &model.impossible {
        parts.push(named.clone());
    }
    let scope_summary = parts.join(" · ");

    TitleBar::new().child(
        h_flex()
            .w_full()
            .items_center()
            .child(div().text_color(theme.foreground).child("geode"))
            // The frame readout (§4.4): slot + label, the scope summary,
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
                        el.bg(theme.warning.opacity(0.25))
                            .px_2()
                            .rounded(px(4.))
                            .child(
                                div()
                                    .text_color(theme.warning_foreground)
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
                    .when(!scope_summary.is_empty(), |el| {
                        el.child(
                            div()
                                .text_color(theme.foreground)
                                .child(scope_summary.clone()),
                        )
                    }),
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
