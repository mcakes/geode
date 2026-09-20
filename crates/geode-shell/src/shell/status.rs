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
//! (pinned release: `gpui-component-0.6.2/src/status_bar.rs`) rather
//! than a hand-rolled `h_flex` — its `left`/`right`/center-`child`
//! regions are exactly this bar's shape (pending+reload on the left,
//! theme name on the right,
//! nothing in the center), and it pulls in the dedicated `status_bar`/
//! `status_bar_border` theme tokens (confirmed present in the pinned
//! `theme_color.rs`) instead of this bar's previous `sidebar`-token
//! workaround, which predated those tokens' use here.

use gpui::prelude::*;
use gpui::{App, IntoElement, MouseButton, SharedString, Window, div, px};
use gpui_component::status_bar::StatusBar;
use gpui_component::{ActiveTheme as _, Sizable as _, Size, progress::Progress};

use super::chip;
use super::control::{self, PointerStates as _};
use super::scale;
use crate::diagnostics::IngestActivity;
use crate::fonts;
use crate::keymap::Keystroke;

/// Height of the status bar, in pixels at the design rem (spec target:
/// ~26px; `shell::scale`). Layout arithmetic reads it through
/// [`height`] so the bar follows the font size with its own text.
pub const HEIGHT: f32 = 26.0;

/// [`HEIGHT`] at the window's current rem, for the tile-surface
/// arithmetic in `render` and the tests that mirror it.
pub fn height(window: &Window) -> f32 {
    scale::design_px(HEIGHT, window.rem_size())
}

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
/// same `warning` token the readout's AS OF badge uses), then the
/// diagnostics summary when `diagnostics_summary` is `Some` (Phase 4b
/// §4.4: `Diagnostics::summary()` — source health, config errors and
/// dropped events, in one terse line — same `warning` token, same
/// reasoning); a click on that segment calls `on_diagnostics_click`
/// (Phase 4b Task 5 — `render.rs`'s call site opens the tile via
/// `ShellView::open_module("diagnostics", ..)` directly — this is the
/// one focus-or-add door left since `diagnostics::open` was retired
/// (user ruling 2026-09-09; the palette's `Diagnostics: Split` rows
/// always add or fill instead) — not by dispatching an action, so the
/// click is not recorded in the crash file's action tail — MIN-9, final
/// review — though `open_module` does mark the session dirty on its
/// own).
/// Then the as-of indicator when `as_of` is `Some` (Phase 4a §3.6: the frame
/// is scoped to a past instant — an unmissable `AS OF {t} · Return to
/// live in the palette` segment in the same warning tokens the toolbar's
/// own AS OF badge uses, since spec §4.5 says nothing on screen may look
/// live when it is not); right — the active theme name in
/// `muted_foreground`. All
/// colors come from `cx.theme()`; no other input is read, so the same
/// call always renders the same tree for the same arguments.
///
/// Eleven plain, independently-`Option`al inputs rather than a bundling
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
    // A stack verb's one-line refusal (tile-stacks spec §4), cleared by
    // the next dispatch.
    notice: Option<&str>,
    diagnostics_summary: Option<&str>,
    on_diagnostics_click: impl Fn(&mut Window, &mut App) + 'static,
    // What the ingest runner is loading right now, or `None` while idle
    // (spec 2026-09-19 §5.3) — paints a 2px loading strip on the bar's
    // top edge plus a `loading <source> · <n> queued` segment, both only
    // while `Some`.
    ingest: Option<&IngestActivity>,
    as_of: Option<&str>,
    // The as-of instant's full resolved timestamp (`ScopeBarModel::
    // as_of_full`), `Some` exactly when `as_of` is — the segment's
    // tooltip TITLE (final review, spec §5.1), with the painted `as_of`
    // text moving to the detail line.
    as_of_full: Option<&SharedString>,
    theme_name: &str,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();

    let pending_text = pending
        .iter()
        .map(format_keystroke)
        .collect::<Vec<_>>()
        .join(" ");

    // `h_full`, not a height of its own: the wrapper below is the one
    // owner of the bar's height (the design guide's "fix the common
    // owner" — two declarations of one length drift, and a window test
    // measuring the wrapper cannot see the inner one disagree).
    let mut bar = StatusBar::new().flex_none().w_full().h_full();
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
    if let Some(message) = notice {
        // A verb's one-line refusal (tile stacks spec §4): muted, cleared
        // by the next dispatch.
        bar = bar.left(
            div()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "shell-notice".to_string())
                .child(message.to_string()),
        );
    }
    if let Some(message) = diagnostics_summary {
        bar = bar.left(
            // The one clickable segment on the bar takes pointer states
            // (`control::PointerStates`, a bare glyph's) with a little
            // horizontal padding so the hover box has a shape.
            div()
                .id("diagnostics-summary")
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .text_color(theme.warning)
                .pointer_states(control::paint(
                    theme,
                    control::Rest::Bare,
                    theme.status_bar,
                    theme.warning,
                ))
                .debug_selector(|| "diagnostics-summary".to_string())
                .child(message.to_string())
                .tooltip(crate::tips::tip(
                    "tip-diagnostics-summary",
                    "Open the diagnostics tile",
                    None,
                    Some(SharedString::new_static("click to open")),
                ))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    on_diagnostics_click(window, cx);
                }),
        );
    }
    if let Some(activity) = ingest {
        bar = bar.left(
            div()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "ingest-loading".to_string())
                .child(activity.label.clone()),
        );
    }
    if let Some((t, full)) = as_of.zip(as_of_full) {
        // The same warning-toned badge treatment the toolbar's own AS OF
        // readout uses (`shell::toolbar`'s "scope-asof" child) — an
        // unmissable second reminder in the one place a maximised tile
        // cannot hide (spec §3.6/§4.5). `as_of_text` is built once and
        // reused for both the painted child and the tooltip's detail
        // line (a clone of the same `SharedString` — no second
        // `format!`); the tooltip's TITLE is `full`, the as-of instant's
        // whole resolved timestamp (final review, spec §5.1) — a trader
        // hovering to see exactly when must not get the same elided text
        // the segment already shows.
        let as_of_text: SharedString = format!("AS OF {t} · Return to live in the palette").into();
        // Through the chip door (`shell::chip`): `warning_foreground` over
        // the tint is the background family on a barely-tinted background
        // at the pinned rev.
        let as_of = chip::chip_paint(theme, chip::Tone::Warning);
        bar = bar.left(
            div()
                .id("status-as-of")
                .when_some(as_of.fill, |el, fill| el.bg(fill))
                .text_color(as_of.text)
                .px_2()
                .rounded(theme.radius)
                .debug_selector(|| "status-as-of".to_string())
                .tooltip(crate::tips::tip_with(
                    SharedString::new_static("tip-status-as-of"),
                    full.clone(),
                    Some("frame::as_of"),
                    Some(as_of_text.clone()),
                ))
                .child(as_of_text),
        );
    }

    let bar = bar.right(
        div()
            .text_color(theme.muted_foreground)
            .child(theme_name.to_string()),
    );

    // The bar is wrapped rather than grown: the loading strip is an
    // absolute overlay pinned to the top edge, so its presence never
    // moves or resizes the bar itself (the window test pins both
    // `origin.y` and `size.height` across the idle/loading transition).
    div()
        .relative()
        .flex_none()
        .w_full()
        .h(scale::design(HEIGHT))
        .debug_selector(|| "shell-status-bar".to_string())
        .child(bar)
        .when(ingest.is_some(), |el| {
            el.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(2.))
                    .debug_selector(|| "ingest-strip".to_string())
                    .child(
                        Progress::new("ingest-strip")
                            .loading(true)
                            .with_size(Size::Size(px(2.)))
                            .w_full(),
                    ),
            )
        })
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
