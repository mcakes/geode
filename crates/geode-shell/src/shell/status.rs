//! The bottom status bar (Task 4, spec §3): pending keystroke display, the
//! config-reload indicator, and the active theme name. `status_bar` is a
//! pure function of its arguments — no stored state, no I/O — so
//! `ShellView` (or its tests) can call it with whatever matcher/theme
//! snapshot they have on hand.
//!
//! Workspace indicators moved to the sidebar (Task 4 — `shell::sidebar`);
//! this bar no longer knows the active workspace or which ones are
//! non-empty. Per the phase-1c plan (`docs/superpowers/plans/
//! 2026-08-29-phase-1c-shell-polish.md`, task 4 and its resize-binding
//! section: "no `ShellMode`, ... no mode indicator"), there is no mode
//! indicator either — Geode has never had a modal-editing concept for this
//! bar to report.
//!
//! **Inventory decision:** migrated onto gpui-component's `StatusBar`
//! (pinned checkout: `crates/ui/src/status_bar.rs`) rather than a hand-
//! rolled `h_flex` — its `left`/`right`/center-`child` regions are exactly
//! this bar's shape (pending+reload on the left, theme name on the right,
//! nothing in the center), and it pulls in the dedicated `status_bar`/
//! `status_bar_border` theme tokens (confirmed present in the pinned
//! `theme_color.rs`) instead of this bar's previous `sidebar`-token
//! workaround, which predated those tokens' use here.

use gpui::prelude::*;
use gpui::{App, IntoElement, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::status_bar::StatusBar;

use crate::fonts;
use crate::keymap::Keystroke;

/// Fixed height of the status bar, in pixels (spec target: ~26px).
pub const HEIGHT: f32 = 26.0;

/// Build the status bar: left — the count prefix (§3.3, in the mono face)
/// when one is in flight, then the pending keystrokes as space-separated
/// text, then the reload indicator when `reload_message` is `Some` (Task
/// 1c-1: a danger-toned `config: N error(s) — keeping last good` marker,
/// `None` when config is healthy), then the write-failure indicator when
/// `write_error_message` is `Some` (Phase 4c §7.1: a config write the
/// dialogs applied in memory and could not persist, rolled back — shown
/// here rather than only in the dialog, because the write outlives the
/// dialog that started it), then the restart indicator when
/// `restart_message` is `Some` (Phase 3 §4.5: a `sources`/`datasets`
/// reload the frame cannot pick up live, so this asks for a restart in the
/// same `warning` token the readout's AS OF badge uses), then the data
/// status indicator when `data_status_message` is `Some` (Phase 3 §5.1:
/// a source's degraded health, or events the app bridge's bounded
/// channel had to refuse — same `warning` token, same reasoning), then
/// the as-of indicator when `as_of` is `Some` (Phase 4a §3.6: the frame
/// is scoped to a past instant — an unmissable `AS OF {t} · :live to
/// return` segment in the same warning tokens the toolbar's own AS OF
/// badge uses, since spec §4.5 says nothing on screen may look live when
/// it is not); right — the active theme name in `muted_foreground`. All
/// colors come from `cx.theme()`; no other input is read, so the same
/// call always renders the same tree for the same arguments.
///
/// Nine plain, independently-`Option`al inputs rather than a bundling
/// struct (clippy's `too_many_arguments`, `-D warnings`-enforced):
/// `render.rs`'s one call site already has each of these as its own
/// separate local (`self.matcher.pending()`, `self.last_reload.
/// status_message()`, ...) — wrapping them in a struct just to satisfy
/// the lint would move that assembly cost into `render.rs` for no
/// reader benefit, since this function's own body treats every field
/// independently anyway.
#[allow(clippy::too_many_arguments)]
pub fn status_bar(
    pending: &[Keystroke],
    count: Option<u32>,
    reload_message: Option<&str>,
    write_error_message: Option<&str>,
    restart_message: Option<&str>,
    data_status_message: Option<&str>,
    as_of: Option<&str>,
    theme_name: &str,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();

    let pending_text = pending
        .iter()
        .map(format_keystroke)
        .collect::<Vec<_>>()
        .join(" ");

    let mut bar = StatusBar::new().flex_none().w_full().h(px(HEIGHT));
    if let Some(count) = count {
        bar = bar.left(
            div()
                .font_family(fonts::MONO)
                .text_color(theme.muted_foreground)
                .child(format!("{count}")),
        );
    }
    bar = bar.left(
        div()
            .font_family(fonts::MONO)
            .text_color(theme.muted_foreground)
            .child(pending_text),
    );
    if let Some(message) = reload_message {
        bar = bar.left(div().text_color(theme.danger).child(message.to_string()));
    }
    if let Some(message) = write_error_message {
        // Danger, not warning: unlike every other segment here this one
        // reports work the app agreed to do and then could not, and it is
        // the only place a trader whose dialog is already closed learns
        // that the change they watched land was rolled back.
        bar = bar.left(
            div()
                .text_color(theme.danger)
                .debug_selector(|| "config-write-error".to_string())
                .child(message.to_string()),
        );
    }
    if let Some(message) = restart_message {
        bar = bar.left(
            div()
                .text_color(theme.warning)
                .debug_selector(|| "restart-required".to_string())
                .child(message.to_string()),
        );
    }
    if let Some(message) = data_status_message {
        bar = bar.left(
            div()
                .text_color(theme.warning)
                .debug_selector(|| "data-status".to_string())
                .child(message.to_string()),
        );
    }
    if let Some(t) = as_of {
        // The same warning-toned badge treatment the toolbar's own AS OF
        // readout uses (`shell::toolbar`'s "scope-asof" child) — an
        // unmissable second reminder in the one place a maximised tile
        // cannot hide (spec §3.6/§4.5).
        bar = bar.left(
            div()
                .bg(theme.warning.opacity(0.25))
                .text_color(theme.warning_foreground)
                .px_2()
                .rounded(px(4.))
                .debug_selector(|| "status-as-of".to_string())
                .child(format!("AS OF {t} · :live to return")),
        );
    }

    bar.right(
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
