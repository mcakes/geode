//! Workspace icon rail with a bottom settings button.
//!
//! The active workspace and all nonempty workspaces are visible; empty
//! inactive workspaces are hidden. The rail has a fixed design width scaled
//! with the window rem and uses the theme's sidebar tokens.
//!
//! Workspace indicators use custom rounded squares so their labels, fills,
//! and radius follow the theme. The active indicator uses `sidebar_primary`
//! on a tint of that colour; inactive indicators use `sidebar_foreground`
//! on `secondary`. The settings button uses an unnamed `Avatar`, which
//! shows a user glyph, with its text colour set for the sidebar surface.

use gpui::prelude::*;
use gpui::{Context, IntoElement, MouseButton, SharedString, Window, div};
use gpui_component::avatar::Avatar;
use gpui_component::{ActiveTheme as _, Sizable as _, v_flex};

use crate::actions::ActionId;
use crate::shell::ShellView;
use crate::shell::control::{self, PointerStates as _};
use crate::shell::scale;

/// Width of the sidebar icon rail, in pixels at the design rem
/// (`shell::scale`): the rail follows the font size with the icons it
/// holds. Layout arithmetic reads it through [`width`].
pub const WIDTH: f32 = 40.0;

/// [`WIDTH`] at the window's current rem, for the tile-surface arithmetic
/// in `render`/`drag` and the tests that mirror it.
pub fn width(window: &Window) -> f32 {
    scale::design_px(WIDTH, window.rem_size())
}

/// `workspace::switch_{n}` for n = 1..=9, as `&'static str`s so the
/// tooltip closure captures no allocation per render.
const WORKSPACE_SWITCH: [&str; 9] = [
    "workspace::switch_1",
    "workspace::switch_2",
    "workspace::switch_3",
    "workspace::switch_4",
    "workspace::switch_5",
    "workspace::switch_6",
    "workspace::switch_7",
    "workspace::switch_8",
    "workspace::switch_9",
];
const WORKSPACE_SITE: [&str; 9] = [
    "sidebar-workspace-1",
    "sidebar-workspace-2",
    "sidebar-workspace-3",
    "sidebar-workspace-4",
    "sidebar-workspace-5",
    "sidebar-workspace-6",
    "sidebar-workspace-7",
    "sidebar-workspace-8",
    "sidebar-workspace-9",
];
/// Complete tooltip selectors passed to `tips::tip`. Keep the prefix in
/// these static strings to avoid formatting it on each render.
/// `WORKSPACE_SITE` names the workspace elements themselves.
const WORKSPACE_TIP: [&str; 9] = [
    "tip-sidebar-workspace-1",
    "tip-sidebar-workspace-2",
    "tip-sidebar-workspace-3",
    "tip-sidebar-workspace-4",
    "tip-sidebar-workspace-5",
    "tip-sidebar-workspace-6",
    "tip-sidebar-workspace-7",
    "tip-sidebar-workspace-8",
    "tip-sidebar-workspace-9",
];
const WORKSPACE_TITLE: [&str; 9] = [
    "Workspace 1",
    "Workspace 2",
    "Workspace 3",
    "Workspace 4",
    "Workspace 5",
    "Workspace 6",
    "Workspace 7",
    "Workspace 8",
    "Workspace 9",
];

/// Build indicators for the active and nonempty workspaces and the
/// settings button. Listeners use the shell entity context to send
/// workspace and settings actions through normal dispatch.
pub fn sidebar(active: u8, non_empty: &[u8], cx: &Context<ShellView>) -> impl IntoElement {
    let theme = cx.theme();
    // Pointer states (design guide: every control owes a hover and a
    // pressed state; the cursor stays the arrow). An inactive disc is a
    // filled chip; the gear is a bare glyph in a box. The ACTIVE disc
    // takes none: it is the selected tab, and "selected" must stay
    // distinct from "hovered" (the guide's state table) — a click on it
    // switches to the workspace already shown.
    let disc_states = control::paint(
        theme,
        control::Rest::Filled(theme.secondary),
        theme.sidebar,
        theme.sidebar_foreground,
    );
    let gear_states = control::paint(
        theme,
        control::Rest::Bare,
        theme.sidebar,
        theme.sidebar_foreground,
    );

    let mut indicators = v_flex().w_full().items_center().gap_2().pt_2();
    for n in 1..=9u8 {
        let is_active = n == active;
        let is_non_empty = non_empty.contains(&n);
        if !is_active && !is_non_empty {
            // Empty, inactive workspaces stay out of the rail entirely —
            // same rule the removed status-bar strip used.
            continue;
        }
        indicators = indicators.child(
            div()
                .id(gpui::ElementId::NamedInteger(
                    SharedString::new_static("sidebar-workspace"),
                    n as u64,
                ))
                .debug_selector(move || WORKSPACE_SITE[(n - 1) as usize].to_string())
                .tooltip(crate::tips::tip(
                    WORKSPACE_TIP[(n - 1) as usize],
                    WORKSPACE_TITLE[(n - 1) as usize],
                    Some(WORKSPACE_SWITCH[(n - 1) as usize]),
                    None,
                ))
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _event, window, cx| {
                        view.dispatch(
                            &ActionId(format!("workspace::switch_{n}")),
                            None,
                            window,
                            cx,
                        );
                        cx.notify();
                    }),
                )
                .child(
                    // Rounded square at `Avatar::small()` scale (24px,
                    // 1px `theme.border` ring, `text_xs` label) — see the
                    // module docs for why this is not a real `Avatar`.
                    div()
                        .id(gpui::ElementId::NamedInteger(
                            SharedString::new_static("sidebar-workspace-disc"),
                            n as u64,
                        ))
                        .size(scale::design(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(if is_active {
                            theme.sidebar_primary.opacity(0.2)
                        } else {
                            theme.secondary
                        })
                        .text_color(if is_active {
                            theme.sidebar_primary
                        } else {
                            theme.sidebar_foreground
                        })
                        .text_xs()
                        .when(!is_active, |d| d.pointer_states(disc_states))
                        .child(n.to_string()),
                ),
        );
    }

    let profile = div()
        .id("sidebar-profile")
        .debug_selector(|| "sidebar-profile".to_string())
        .tooltip(crate::tips::tip(
            "tip-sidebar-profile",
            "Settings",
            Some("settings::open"),
            None,
        ))
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .pb_2()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _event, window, cx| {
                view.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
                cx.notify();
            }),
        )
        .child(
            // The hover box around the avatar, the shape a ghost icon
            // button has: the avatar keeps its own circle and glyph
            // colour, the box behind it takes the pointer states.
            div()
                .id("sidebar-profile-box")
                .size(scale::design(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .pointer_states(gear_states)
                .child(Avatar::new().small().text_color(theme.sidebar_foreground)),
        );

    v_flex()
        .flex_none()
        .w(scale::design(WIDTH))
        .h_full()
        .items_center()
        .justify_between()
        // The zoom test compares the painted rail with `width(window)` to
        // keep its width aligned with the space reserved by the tile surface.
        .debug_selector(|| "shell-sidebar".to_string())
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(indicators)
        .child(profile)
}
