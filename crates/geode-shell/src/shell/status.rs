//! The bottom status bar (Task 4, spec §3): workspace indicators, pending
//! keystroke display, and the active theme name. `status_bar` is a pure
//! function of its arguments — no stored state, no I/O — so `ShellView`
//! (or its tests) can call it with whatever workspace/matcher/theme
//! snapshot they have on hand.

use gpui::prelude::*;
use gpui::{App, IntoElement, div, px};
use gpui_component::{ActiveTheme as _, h_flex};

use crate::keymap::Keystroke;

/// Fixed height of the status bar, in pixels (spec target: ~26px).
pub const HEIGHT: f32 = 26.0;

/// Build the status bar: left — workspace indicators 1..=9 (active is
/// `primary`-styled, non-empty ones normal, empty ones hidden except the
/// active one); middle — pending keystrokes as space-separated text; right
/// — the active theme name in `muted_foreground`. All colors come from
/// `cx.theme()`; no other input is read, so the same call always renders
/// the same tree for the same arguments.
pub fn status_bar(
    active_index: u8,
    non_empty_indices: &[u8],
    pending: &[Keystroke],
    theme_name: &str,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    // `sidebar` is present at the pinned gpui-component rev (checked against
    // the vendored theme_color.rs); the brief's `muted` fallback is not
    // needed here, but keep the intent noted for future rev bumps.
    let bar_bg = theme.sidebar;

    let mut indicators = h_flex().gap_2();
    for n in 1..=9u8 {
        let is_active = n == active_index;
        let is_non_empty = non_empty_indices.contains(&n);
        if !is_active && !is_non_empty {
            // Empty, inactive workspaces stay out of the strip entirely.
            continue;
        }
        indicators = indicators.child(
            div()
                .text_color(if is_active {
                    theme.primary
                } else {
                    theme.foreground
                })
                .child(n.to_string()),
        );
    }

    let pending_text = pending
        .iter()
        .map(format_keystroke)
        .collect::<Vec<_>>()
        .join(" ");

    h_flex()
        .flex_none()
        .w_full()
        .h(px(HEIGHT))
        .items_center()
        .justify_between()
        .px_2()
        .gap_3()
        .bg(bar_bg)
        .text_color(theme.foreground)
        .child(
            h_flex()
                .items_center()
                .gap_3()
                .child(indicators)
                .child(div().text_color(theme.muted_foreground).child(pending_text)),
        )
        .child(
            div()
                .text_color(theme.muted_foreground)
                .child(theme_name.to_string()),
        )
}

/// Minimal, status-strip-grade rendering of one keystroke (`ctrl+shift+g`).
/// Not a general-purpose formatter — good enough to show "what's pending".
fn format_keystroke(ks: &Keystroke) -> String {
    let mut parts = Vec::new();
    if ks.mods.ctrl {
        parts.push("ctrl");
    }
    if ks.mods.alt {
        parts.push("alt");
    }
    if ks.mods.shift {
        parts.push("shift");
    }
    if ks.mods.cmd {
        parts.push("cmd");
    }
    parts.push(&ks.key);
    parts.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Modifiers;

    #[test]
    fn format_keystroke_renders_modifiers_and_key() {
        let ks = Keystroke {
            mods: Modifiers::NONE,
            key: "g".to_string(),
        };
        assert_eq!(format_keystroke(&ks), "g");

        let ks = Keystroke {
            mods: Modifiers::CTRL.union(Modifiers {
                shift: true,
                ..Modifiers::NONE
            }),
            key: "g".to_string(),
        };
        assert_eq!(format_keystroke(&ks), "ctrl+shift+g");
    }
}
