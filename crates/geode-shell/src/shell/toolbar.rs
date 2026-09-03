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
//! Content: the app title left, a reserved (empty, for now) middle region
//! for grouping/scope state (spec, a later phase), and a right-aligned
//! filter [`Input`] to prove the row hosts real content (user direction).
//! The filter is deliberately **inert**: nothing reads its value yet — it
//! becomes the global text filter (spec §4.1) once the data phase wires a
//! consumer to the `InputState` `ShellView` owns. Its focus interplay
//! (click-to-focus, Esc-back-to-shell-root, shell chords suppressed while
//! it has focus) lives in `ShellView::handle_key_down` — this function only
//! renders it.

use gpui::prelude::*;
use gpui::{App, Entity, IntoElement, div, px};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, Icon, IconName, TitleBar, h_flex};

/// Compact width of the filter field (brief: "~200px").
const FILTER_WIDTH: f32 = 200.0;

pub fn toolbar(filter_input: &Entity<InputState>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();

    TitleBar::new().child(
        h_flex()
            .w_full()
            .items_center()
            .child(div().text_color(theme.foreground).child("geode"))
            // Reserved for grouping/scope state (spec §post-data-phase);
            // an empty flexing spacer keeps the filter pinned to the right
            // edge without hardcoding a gap.
            .child(div().flex_1())
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
