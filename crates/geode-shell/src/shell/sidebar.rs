//! The left icon rail (Task 4): vertical workspace indicators (same
//! visibility rule the old status-bar strip used — active always shown,
//! non-empty always shown, empty+inactive hidden) plus a bottom-anchored
//! profile icon that dispatches `settings::open`.
//!
//! **Inventory decision (not gpui-component's `Sidebar<E>`):** the pinned
//! checkout's `crates/ui/src/sidebar/mod.rs` `Sidebar` is a ~255px-default
//! (48px collapsed), `ListState`-virtualized, animated collapse/expand nav
//! panel whose content is a list of `SidebarItem`-implementing groups with
//! headers/menus — built for a wide app-navigation drawer (see its
//! `SidebarCollapsible`, `DEFAULT_WIDTH`/`COLLAPSED_WIDTH`, and its own
//! 200ms width-transition machinery). Our rail is the opposite shape: a
//! fixed ~40px strip with a handful of always-static icons and no
//! collapse/expand state at all. Forcing that onto `Sidebar<E>` would mean
//! implementing `SidebarItem`/`Collapsible` for workspace indicators just
//! to fight its animation and list virtualization for content that never
//! changes size. So this is a hand-built strip; it borrows only the
//! matching color tokens (`sidebar`, `sidebar_foreground`, `sidebar_border`,
//! `sidebar_primary`) so it still themes consistently with the rest of the
//! library.
//!
//! **Avatar:** uses gpui-component's `avatar::Avatar` (pinned checkout:
//! `crates/ui/src/avatar/avatar.rs`) for the profile icon, at its `small`
//! (24px) size. No `.name(...)` is set — there is no real user identity
//! yet, and `Avatar` without a name falls back to a plain `IconName::User`
//! glyph rather than synthesizing fake initials. Upstream paints that
//! placeholder glyph in `theme.background` on a `secondary` disc — near
//! zero contrast on our `sidebar`-colored rail — so we override the text
//! color to `sidebar_foreground` (caller styles win via `refine_style`).
//!
//! **Workspace discs are avatar-shaped but hand-built:** `Avatar` cannot
//! render theme-tokened text — its `.name(...)` branch hard-codes a
//! hashed-hue disc and text color on an *inner* fallback element
//! (`avatar.rs:107-110`) that caller styles never reach, and the
//! composable `BaseAvatar`/`AvatarFallback` primitives live in the
//! library-internal `gpui_base` crate, which gpui-component does not
//! re-export. So the indicators take `Avatar::small()`'s scale (24px,
//! 1px `theme.border` ring, `text_xs` label) as a rounded square on the
//! theme's global `radius` token (per user direction — full circles
//! didn't read well at this size) and color it
//! with our own tokens: active = `sidebar_primary` label on a 20%
//! `sidebar_primary` tint, inactive = `sidebar_foreground` on
//! `secondary` — matching the profile avatar's family visually.

use gpui::prelude::*;
use gpui::{Context, IntoElement, MouseButton, div, px};
use gpui_component::avatar::Avatar;
use gpui_component::{ActiveTheme as _, Sizable as _, v_flex};

use crate::actions::ActionId;
use crate::shell::ShellView;

/// Fixed width of the sidebar icon rail, in pixels.
pub const WIDTH: f32 = 40.0;

/// Build the sidebar. `active`/`non_empty` mirror the arguments the old
/// `status_bar` workspace strip took (Task 4 moved that strip here).
/// Takes `cx: &Context<ShellView>` (not just `&App`, unlike `status_bar`/
/// `toolbar`) because the workspace indicators and the profile icon are
/// clickable — `cx.listener` needs the entity context to dispatch back
/// into `ShellView` through the normal `dispatch` chain.
pub fn sidebar(active: u8, non_empty: &[u8], cx: &Context<ShellView>) -> impl IntoElement {
    let theme = cx.theme();

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
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
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
                        .size(px(24.))
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
                        .child(n.to_string()),
                ),
        );
    }

    let profile = div()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .pb_2()
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _event, window, cx| {
                view.dispatch(&ActionId("settings::open".to_string()), None, window, cx);
                cx.notify();
            }),
        )
        .child(Avatar::new().small().text_color(theme.sidebar_foreground));

    v_flex()
        .flex_none()
        .w(px(WIDTH))
        .h_full()
        .items_center()
        .justify_between()
        .bg(theme.sidebar)
        .border_r_1()
        .border_color(theme.sidebar_border)
        .child(indicators)
        .child(profile)
}
