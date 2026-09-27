//! Bottom status bar prepared from arguments and the active theme, with no retained
//! state or I/O. Stopped data threads, count, pending keys, configuration messages, diagnostics, ingestion,
//! and historical-time indicators occupy the left region. The right region is the
//! view-state section: the fullscreen indicator, then the active theme name.
//! Workspace indicators belong to the sidebar. StatusBar supplies
//! the status_bar and status_bar_border theme tokens.

use gpui::prelude::*;
use gpui::{App, IntoElement, MouseButton, SharedString, Window, div, px};
use gpui_component::status_bar::StatusBar;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Size, h_flex, progress::Progress,
};

use super::chip;
use super::control::{self, PointerStates as _};
use super::scale;
use crate::diagnostics::{IngestActivity, StoppedSegment};
use crate::fonts;
use crate::keymap::Keystroke;

/// Height of the status bar, in pixels at the design rem (`shell::scale`).
/// Layout arithmetic reads it through
/// [`height`] so the bar follows the font size with its own text.
pub const HEIGHT: f32 = 26.0;

/// [`HEIGHT`] at the window's current rem, for the tile-surface
/// arithmetic in `render` and the tests that mirror it.
pub fn height(window: &Window) -> f32 {
    scale::design_px(HEIGHT, window.rem_size())
}

/// The diagnostics summary's tooltip title: a click opens the diagnostics
/// page over the workspace, not a tile. Named so a test can assert the
/// copy, since gpui's test API cannot read painted text.
pub const DIAGNOSTICS_TIP_TITLE: &str = "Open the diagnostics page";

/// The fullscreen segment's text for `hidden` other tiles. The common
/// counts are literals so an idle repaint of a maximised tile formats
/// nothing.
pub fn fullscreen_label(hidden: usize) -> SharedString {
    const LABELS: [&str; 10] = [
        "fullscreen",
        "fullscreen · 1 hidden",
        "fullscreen · 2 hidden",
        "fullscreen · 3 hidden",
        "fullscreen · 4 hidden",
        "fullscreen · 5 hidden",
        "fullscreen · 6 hidden",
        "fullscreen · 7 hidden",
        "fullscreen · 8 hidden",
        "fullscreen · 9 hidden",
    ];
    match LABELS.get(hidden) {
        Some(label) => SharedString::new_static(label),
        None => format!("fullscreen · {hidden} hidden").into(),
    }
}

/// Render optional status segments in order: stopped data threads, count, nonempty
/// pending keys, reload failure, write failure, restart requirement, shell notice,
/// diagnostics summary, ingestion activity, and historical time on the left;
/// fullscreen, then the theme name, on the right. The fullscreen segment shows while
/// a main-tree tile is maximised, carrying the number of tiles it hides; clicking it
/// restores the layout through the supplied callback. Stopped threads and
/// configuration errors use danger, restart and diagnostics use warning, and ordinary
/// notices are muted. Clicks on the stopped segment and the diagnostics summary both
/// invoke the supplied diagnostics callback. The historical badge requires both the shortened and
/// full timestamps; its tooltip shows the full timestamp. Inputs remain separate
/// because callers already hold these values independently.
#[allow(clippy::too_many_arguments)]
pub fn status_bar(
    pending: &[Keystroke],
    count: Option<u32>,
    reload_message: Option<&str>,
    write_error_message: Option<&str>,
    restart_message: Option<&str>,
    // Shell action refusal, cleared by the next dispatch.
    notice: Option<&str>,
    // Stopped data threads, prepared by `Diagnostics::note_thread_stopped`;
    // None while every data thread lives.
    stopped: Option<&StoppedSegment>,
    diagnostics_summary: Option<&str>,
    on_diagnostics_click: impl Fn(&mut Window, &mut App) + Clone + 'static,
    // Current ingestion activity, shown as a loading label and a two-pixel strip along
    // the bar's top edge. None hides both.
    ingest: Option<&IngestActivity>,
    // Other tiles hidden by a fullscreen main-tree tile; None while nothing is
    // fullscreen.
    fullscreen_hidden: Option<usize>,
    on_fullscreen_click: impl Fn(&mut Window, &mut App) + 'static,
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
    if let Some(segment) = stopped {
        // First and in danger: a stopped data thread outranks every count
        // after it, which may describe a service that no longer runs. It
        // never clears; restarting Geode is the recovery. The click is the
        // diagnostics summary's own route, onto the tile whose sources
        // section lists each stopped thread.
        let stopped_click = on_diagnostics_click.clone();
        bar = bar.left(
            div()
                .id("data-stopped")
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .text_color(theme.danger)
                .pointer_states(control::paint(
                    theme,
                    control::Rest::Bare,
                    theme.status_bar,
                    theme.danger,
                ))
                .debug_selector(|| "data-stopped".to_string())
                .child(segment.text.clone())
                .tooltip(crate::tips::tip_with(
                    SharedString::new_static("tip-data-stopped"),
                    segment.detail.clone(),
                    None,
                    Some(SharedString::new_static("click to open diagnostics")),
                ))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    stopped_click(window, cx);
                }),
        );
    }
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
            // The diagnostics segment uses the shared bare-control pointer
            // states and horizontal padding to define its hover area.
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
                    DIAGNOSTICS_TIP_TITLE,
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

    if let Some(hidden) = fullscreen_hidden {
        // The right region is the view-state section: how the window is
        // being shown, beside the theme it is painted in. Muted, like the
        // notice: maximising is a layout the trader chose, not a fault.
        // Clickable like the diagnostics summary; the tooltip names the
        // key, resolved on hover rather than per frame.
        bar = bar.right(
            h_flex()
                .id("status-fullscreen")
                .gap_1()
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .text_color(theme.muted_foreground)
                .pointer_states(control::paint(
                    theme,
                    control::Rest::Bare,
                    theme.status_bar,
                    theme.muted_foreground,
                ))
                .debug_selector(|| "status-fullscreen".to_string())
                .child(Icon::new(IconName::Maximize).xsmall())
                .child(fullscreen_label(hidden))
                .tooltip(crate::tips::tip(
                    "tip-status-fullscreen",
                    "Restore the layout",
                    Some("workspace::fullscreen_tile"),
                    Some(SharedString::new_static("click to restore")),
                ))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    on_fullscreen_click(window, cx);
                }),
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
