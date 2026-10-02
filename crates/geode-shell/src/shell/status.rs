//! Bottom status bar prepared from arguments and the active theme, with no retained
//! state or I/O. Every text argument arrives as a prepared `SharedString`, so a
//! paint clones reference counts instead of formatting segment text (the
//! pending-key chips still format through `shell::kbd`). Stopped data threads,
//! count, pending keys, configuration messages, diagnostics, ingestion,
//! and historical-time indicators occupy the left region. The right region is the
//! view-state section: the link group the focused tile follows, the fullscreen
//! indicator, then the active theme name.
//! Workspace indicators belong to the sidebar. StatusBar supplies
//! the status_bar and status_bar_border theme tokens.

use geode_core::link::Group;
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

/// The following segment's text: the link group the focused tile follows
/// and, when the group's scope names exactly one underlying, that
/// underlying. The bare form is a literal; the other is formatted here,
/// once per change, by the caller that caches it (`ShellView::link_label`),
/// never per paint.
pub fn following_label(group: Group, underlying: Option<&str>) -> SharedString {
    match underlying {
        None => SharedString::new_static(match group {
            Group::A => "following A",
            Group::B => "following B",
            Group::C => "following C",
            Group::D => "following D",
        }),
        Some(underlying) => format!("following {} \u{00b7} {underlying}", group.letter()).into(),
    }
}

/// Render optional status segments in order: stopped data threads, count, nonempty
/// pending keys, reload failure, write failure, restart requirement, shell notice,
/// diagnostics summary, ingestion activity, and historical time on the left;
/// following, fullscreen, then the theme name, on the right. The following segment
/// shows while the focused tile follows a link group, naming the group and its
/// single underlying; clicking it opens the link group chooser through the supplied
/// callback. The fullscreen segment shows while
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
    count: Option<&SharedString>,
    reload_message: Option<&SharedString>,
    write_error_message: Option<&SharedString>,
    restart_message: Option<&SharedString>,
    // A shell action's refusal, or its report of what it did (a row menu
    // action's `opened <url>`); cleared by the next dispatch.
    notice: Option<&SharedString>,
    // Stopped data threads, prepared by `Diagnostics::note_thread_stopped`;
    // None while every data thread lives.
    stopped: Option<&StoppedSegment>,
    diagnostics_summary: Option<&SharedString>,
    on_diagnostics_click: impl Fn(&mut Window, &mut App) + Clone + 'static,
    // Current ingestion activity, shown as a loading label and a two-pixel strip along
    // the bar's top edge. None hides both.
    ingest: Option<&IngestActivity>,
    // The prepared `following_label` for the focused tile; None while it
    // follows no link group.
    following: Option<&SharedString>,
    on_following_click: impl Fn(&mut Window, &mut App) + 'static,
    // Other tiles hidden by a fullscreen main-tree tile; None while nothing is
    // fullscreen.
    fullscreen_hidden: Option<usize>,
    on_fullscreen_click: impl Fn(&mut Window, &mut App) + 'static,
    // The prepared `AS OF … · Return to live in the palette` badge text
    // (`ScopeBarModel::as_of_status`); None while live.
    as_of_label: Option<&SharedString>,
    // Full resolved historical timestamp for the badge tooltip. Supply alongside
    // as_of_label, which is painted and repeated in the tooltip detail.
    as_of_full: Option<&SharedString>,
    theme_name: &SharedString,
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
                .child(count.clone()),
        );
    }
    // Build pending chips only when nonempty, avoiding an empty status element and
    // unnecessary formatting work on idle paints.
    if !pending.is_empty() {
        bar = bar.left(super::kbd::binding(pending));
    }
    if let Some(message) = reload_message {
        bar = bar.left(div().text_color(theme.danger).child(message.clone()));
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
                .child(message.clone()),
        );
    }
    if let Some(message) = restart_message {
        bar = bar.left(
            div()
                .text_color(theme.warning)
                .debug_selector(|| "restart-required".to_string())
                .child(message.clone()),
        );
    }
    if let Some(message) = notice {
        // Notices (refusals and reports) stay muted and clear on the next
        // dispatch. One line, ellipsized: a notice can carry dynamic text (a URL)
        // longer than the left region has room for.
        bar = bar.left(
            div()
                .min_w_0()
                .truncate()
                .text_color(theme.muted_foreground)
                .debug_selector(|| "shell-notice".to_string())
                .child(message.clone()),
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
                .child(message.clone())
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
    if let Some((label, full)) = as_of_label.zip(as_of_full) {
        // Keep historical time visible even when a tile is maximised. Reuse the
        // prepared badge text in the tooltip detail and show the full resolved
        // timestamp as its title.
        // Through the chip door (`shell::chip`): `warning_foreground` over
        // the tint is the background family on a barely-tinted background
        // at the pinned rev.
        let as_of = chip::chip_paint_on(theme, chip::Tone::Warning, theme.status_bar);
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
                    Some(label.clone()),
                ))
                .child(label.clone()),
        );
    }

    if let Some(label) = following {
        // First in the view-state section: whose scope the focused tile is
        // showing. Muted like the fullscreen segment beside it: following a
        // group is a choice, not a fault. The click and the tooltip's key
        // both open the chooser that changes it.
        bar = bar.right(
            div()
                .id("status-following")
                .px_1()
                .rounded(theme.radius_tokens().sm)
                .text_color(theme.muted_foreground)
                .pointer_states(control::paint(
                    theme,
                    control::Rest::Bare,
                    theme.status_bar,
                    theme.muted_foreground,
                ))
                .debug_selector(|| "status-following".to_string())
                .child(label.clone())
                .tooltip(crate::tips::tip(
                    "tip-status-following",
                    "Link group",
                    Some("tile::link_group"),
                    Some(SharedString::new_static("click to change")),
                ))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    on_following_click(window, cx);
                }),
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
            .child(theme_name.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The label names the group, and the underlying only when the
    /// group's scope names exactly one: a group with no scope, or one
    /// spanning several underlyings, reads as the group alone.
    #[test]
    fn the_following_label_names_the_group_and_its_single_underlying() {
        assert_eq!(following_label(Group::A, None).as_ref(), "following A");
        assert_eq!(following_label(Group::D, None).as_ref(), "following D");
        assert_eq!(
            following_label(Group::A, Some("SPX.Z")).as_ref(),
            "following A \u{00b7} SPX.Z"
        );
        assert_eq!(
            following_label(Group::C, Some("NDX")).as_ref(),
            "following C \u{00b7} NDX"
        );
        for group in Group::ALL {
            assert!(
                following_label(group, None).ends_with(group.letter()),
                "{group:?}"
            );
        }
    }
}
