//! Bottom status bar prepared from arguments and the active theme, with no retained
//! state or I/O. Count, pending keys, configuration messages, diagnostics, ingestion,
//! and historical-time indicators occupy the left region; the active theme name
//! occupies the right. Workspace indicators belong to the sidebar. StatusBar supplies
//! the status_bar and status_bar_border theme tokens.

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

/// Render optional status segments in order: count, nonempty pending keys, reload
/// failure, write failure, restart requirement, shell notice, diagnostics summary,
/// ingestion activity, and historical time. Configuration errors use danger, restart
/// and diagnostics use warning, and ordinary notices are muted. Diagnostics clicks
/// invoke the supplied callback. The historical badge requires both the shortened and
/// full timestamps; its tooltip shows the full timestamp. Theme name appears on the
/// right. Inputs remain separate because callers already hold these values
/// independently.
#[allow(clippy::too_many_arguments)]
pub fn status_bar(
    pending: &[Keystroke],
    count: Option<u32>,
    reload_message: Option<&str>,
    write_error_message: Option<&str>,
    restart_message: Option<&str>,
    // Shell action refusal, cleared by the next dispatch.
    notice: Option<&str>,
    diagnostics_summary: Option<&str>,
    on_diagnostics_click: impl Fn(&mut Window, &mut App) + 'static,
    // Current ingestion activity, shown as a loading label and a two-pixel strip along
    // the bar's top edge. None hides both.
    ingest: Option<&IngestActivity>,
    as_of: Option<&str>,
    // Full resolved historical timestamp for the badge tooltip. Supply alongside as_of,
    // whose shortened text is painted and repeated in the tooltip detail.
    as_of_full: Option<&SharedString>,
    theme_name: &str,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();

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
    // Build pending chips only when nonempty, avoiding an empty status element and
    // unnecessary formatting work on idle paints.
    if !pending.is_empty() {
        bar = bar.left(super::kbd::binding(pending));
    }
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
        // Shell action refusals stay muted and clear on the next dispatch.
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
        // Keep historical time visible even when a tile is maximised. Reuse the
        // prepared badge text in the tooltip detail and show the full resolved
        // timestamp as its title.
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
