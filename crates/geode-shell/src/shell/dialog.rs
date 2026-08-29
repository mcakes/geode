//! The uniform dialog utility (Task 9, user direction: "the app will use
//! dialogs heavily; they must be created one standard way so they behave
//! uniformly").
//!
//! [`open_shell_dialog`] is the single mandatory door for opening any dialog
//! in Geode. Call it instead of `window.open_dialog` directly — every call
//! site gets the same open-time hygiene for free:
//!
//! 1. cancel any pending keymap sequence (`Matcher::cancel()` — the same
//!    hygiene [`toggle_palette`](super::ShellView::toggle_palette) already
//!    gives palette-open, so a dialog opening mid-sequence, e.g. the first
//!    "g" of a "g g" binding, doesn't leave a stale pending keystroke sitting
//!    in `self.matcher` for whatever key closes the dialog to resume
//!    matching against);
//! 2. close the palette if one is open (same reasoning: the palette has its
//!    own exclusive key handling, so a dialog opening over it would otherwise
//!    leave a `PaletteState` alive underneath with no way to reach it);
//! 3. delegate to gpui-component's own dialog layer (`window.open_dialog`)
//!    with our standard conventions.
//!
//! This module does **not** duplicate chord suppression while a dialog is
//! open — that's handled centrally by the `has_active_dialog` guard at the
//! top of [`ShellView::handle_key_down`](super::ShellView::handle_key_down)
//! (final-review fix, landed in commit fabaced). The two halves together are
//! the uniform behavior: this module's hygiene runs once, at open; that
//! guard runs on every keystroke for as long as the dialog stays open.

use gpui::{App, Context, Window};
use gpui_component::WindowExt as _;
use gpui_component::dialog::Dialog;

use super::ShellView;

/// Open a dialog through Geode's one standard door. `build` is handed
/// straight to `window.open_dialog` — gpui-component's own dialog-layer
/// builder closure, `Fn(Dialog, &mut Window, &mut App) -> Dialog` (not
/// `FnOnce`: gpui-component's pinned rev requires `Fn` because the dialog
/// layer may need to rebuild the dialog's chrome across frames; adapted from
/// the brief's `FnOnce` sketch to match the actual `WindowExt::open_dialog`
/// signature at the pinned rev).
///
/// Takes `&mut ShellView` (not `Entity<ShellView>`) so this reads naturally
/// both from `ShellView::dispatch`'s own action arms (which already hold
/// `&mut self`) and from a small wrapper like `settings_view::open` that
/// only needs an `Entity<ShellView>` clone to hand into the dialog's content
/// closure for later (the dialog renders on its own schedule, not
/// synchronously here) — that wrapper gets the entity handle via
/// `cx.entity()` before calling in, rather than this utility taking one.
pub fn open_shell_dialog<F>(
    view: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
    build: F,
) where
    F: Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
{
    // Same pending-sequence hygiene `toggle_palette` gives palette-open (see
    // that method's doc comment): without this, an unfinished sequence like
    // the first "g" of "g g" would sit in `self.matcher` across the whole
    // dialog session and then resume matching against whatever key closes
    // the dialog.
    view.matcher.cancel();
    // Close an open palette the same way `toggle_palette`'s own close arm
    // does — the palette has no idea a dialog just opened over it, and its
    // exclusive key handling would otherwise still think it owns every
    // keystroke underneath the dialog.
    view.palette = None;
    cx.notify();

    window.open_dialog(cx, build);
}
