//! The uniform modal utility (Task 9, user direction: "the app will use
//! dialogs heavily; they must be created one standard way so they behave
//! uniformly") — now a **self-owned, instant** modal layer, not
//! gpui-component's own `Dialog`.
//!
//! ## Why this changed
//!
//! gpui-component's `Dialog` (pinned checkout `crates/ui/src/dialog/
//! dialog.rs`) hardwires a 250ms fade+slide entrance
//! (`static ANIMATION_DURATION: LazyLock<Duration> = ... Duration::from_secs_f64(0.25)`),
//! with no opt-out at the pinned rev — every call to `.with_animation(...)`
//! there is unconditional. Next to the command palette (`palette::render`,
//! which just appears, fully opaque, the next frame `self.palette` goes
//! `Some`) the settings dialog felt slow and inconsistent: two "open an
//! overlay" actions in the same app, one instant and one animated for no
//! product reason. Root-caused and fixed by user direction: stop routing
//! through gpui-component's dialog layer at all for our own modals, and
//! own the chrome outright — instant by design, the same way the palette
//! already is, not a re-tuned animation.
//!
//! ## What's the same
//!
//! [`open_shell_dialog`] REMAINS the single mandatory door for opening any
//! modal in Geode — call it instead of touching `ShellView`'s modal state
//! (or `window.open_dialog`) directly. Every call site still gets the same
//! open-time hygiene for free:
//!
//! 1. cancel any pending keymap sequence (`Matcher::cancel()` — the same
//!    hygiene [`toggle_palette`](super::ShellView::toggle_palette) already
//!    gives palette-open, so a modal opening mid-sequence, e.g. the first
//!    "g" of a "g g" binding, doesn't leave a stale pending keystroke
//!    sitting in `self.matcher` for whatever key closes the modal to
//!    resume matching against);
//! 2. close the palette if one is open (same reasoning: the palette has
//!    its own exclusive key handling, so a modal opening over it would
//!    otherwise leave a `PaletteState` alive underneath with no way to
//!    reach it).
//!
//! ## What's different
//!
//! Instead of delegating to `window.open_dialog` (gpui-component's own
//! dialog layer, animated, stacked, focus-managed by that crate), this now
//! stores a [`ShellModal`] directly on `ShellView` — `view.modal =
//! Some(ShellModal { title, build })` — and `ShellView::render` paints the
//! backdrop/panel/title-row/close-button chrome itself, palette-style: it
//! exists fully-opaque the very next frame, no `with_animation` anywhere in
//! the path. `handle_key_down`'s modal branch (see that method's doc
//! comment) is what makes Escape close it and what swallows every other
//! shell chord while it's open — the equivalent of the old `has_active_
//! dialog` guard, but for state this module owns instead of state
//! gpui-component's `Root` owns. That guard still also checks `window.
//! has_active_dialog(cx)` alongside `self.modal.is_some()`: gpui-component
//! popovers (e.g. a `Select` dropdown's own overlay) still use that crate's
//! layer machinery internally, so this doesn't touch or replace it — it
//! just no longer *is* it for our own modals.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, MouseButton, SharedString, Window, div, hsla, px};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconName, Sizable as _, box_shadow, h_flex, v_flex};

use super::ShellView;

/// A modal's content builder, called as `build(shell, window, cx)` — see
/// [`ShellModal::build`]'s own doc comment for why the first argument is a
/// plain `&ShellView` and not `Entity<ShellView>`. A named alias rather than
/// spelling the `Rc<dyn Fn(...) -> AnyElement>` out at each of its two use
/// sites (clippy's `type_complexity`, `-D warnings`-enforced in this repo).
type ModalBuilder = Rc<dyn Fn(&ShellView, &mut Window, &mut App) -> AnyElement>;

/// One open modal's state, stored on `ShellView` (`view.modal`) rather than
/// in gpui-component's `Root`. `build` is `Rc`, not `Box`/`FnOnce`, so
/// [`ShellView::render`] can clone it out of `self.modal` — cheap (a
/// refcount bump, same as `title`'s `SharedString` clone) — *before*
/// invoking it, releasing the borrow on `self.modal` first. That "clone the
/// pieces out, then call" step is required here for the same reason
/// `whichkey`'s and the palette's own render inputs are extracted into
/// locals ahead of the render chain (see `ShellView::render`'s `pending`/
/// `registry` locals): the closure needs `window`/`cx` from the render
/// method's own scope while still being reached through a `self.modal.
/// as_ref()` borrow, and cloning the two cheap pieces out up front is the
/// same pattern applied one field further in, rather than a new one.
pub struct ShellModal {
    pub title: SharedString,
    /// Builds this modal's content fresh, called from
    /// [`ShellView::render`] on the frame it's open, as `build(self, window,
    /// cx)` — `self` is the *same* `ShellView` currently rendering, handed
    /// through as a plain `&ShellView` reborrow, not `Entity<ShellView>`.
    ///
    /// That distinction is load-bearing, not stylistic — found the hard way
    /// (first cut of this task used `Entity<ShellView>`/`view.read(cx)`
    /// here, matching the brief's own `Fn(&ShellView, ...)` sketch only
    /// loosely, and every settings-modal test failed at runtime with gpui's
    /// `entity_map.rs` panic: "cannot read geode_shell::shell::ShellView
    /// while it is already being updated"). The old gpui-component `Dialog`
    /// version's content closure (`settings_view::build`) called `Entity<
    /// ShellView>::read(cx)` synchronously and this was fine — but only
    /// because that closure ran inside gpui-component's *own*, separate
    /// `Root`-owned dialog-layer render pass, never nested inside
    /// `ShellView::render` itself, so `entity.read(cx)` there was reading a
    /// *different* render's entity, not a reentrant read of the one
    /// currently mid-render. Once this modal's content is built as a direct
    /// child of `ShellView::render`'s own returned tree (Task 9's whole
    /// point — no separate layer to defer to), any `Entity<ShellView>::
    /// read` reached synchronously from inside `build` — as opposed to
    /// later, from a field's own get/set closure invoked at that field's
    /// actual layout/paint time, safely outside `ShellView::render`'s own
    /// call frame, same as an ordinary event handler — hits gpui's
    /// currently-updating guard for that exact entity. Taking `&ShellView`
    /// instead is a plain Rust borrow, not an entity-handle access at all,
    /// so it carries none of that runtime check — and it's exactly what's
    /// available for free at the one call site inside `render` (`self`,
    /// reborrowed), which is what let `Entity<ShellView>` be dropped from
    /// this type's signature entirely rather than threaded through as a
    /// second parameter alongside it.
    pub build: ModalBuilder,
}

/// Open a modal through Geode's one standard door (Task 9's rule, unchanged
/// by the switch away from gpui-component's `Dialog` — see the module doc).
/// `build` renders the modal's content fresh each frame it's open; `title`
/// is shown in the chrome's title row (see [`render_modal`]).
///
/// `window` is accepted (rather than dropped from the signature) for
/// parity with every other `(view, window, cx)`-shaped entry point in this
/// crate (`settings_view::open`'s own signature, `ShellView::dispatch`'s
/// action arms) — this function's own body has nothing left to do with it
/// now that opening no longer calls `window.open_dialog`, hence `_window`.
pub fn open_shell_dialog<F>(
    view: &mut ShellView,
    _window: &mut Window,
    cx: &mut Context<ShellView>,
    title: impl Into<SharedString>,
    build: F,
) where
    F: Fn(&ShellView, &mut Window, &mut App) -> AnyElement + 'static,
{
    // Same pending-sequence hygiene `toggle_palette` gives palette-open (see
    // that method's doc comment): without this, an unfinished sequence like
    // the first "g" of "g g" would sit in `self.matcher` across the whole
    // modal session and then resume matching against whatever key closes
    // the modal.
    view.matcher.cancel();
    // Close an open palette the same way `toggle_palette`'s own close arm
    // does — the palette has no idea a modal just opened over it, and its
    // exclusive key handling would otherwise still think it owns every
    // keystroke underneath the modal.
    view.palette = None;

    view.modal = Some(ShellModal {
        title: title.into(),
        build: Rc::new(build),
    });
    cx.notify();
}

/// Render one open modal's chrome: a full-window backdrop on
/// `cx.theme().overlay` (the same token gpui-component's own `Dialog`
/// overlay uses — `overlay_color`, pinned checkout `crates/ui/src/dialog/
/// dialog.rs`), centered over it a panel on `cx.theme().popover`/
/// `popover_foreground` with a `cx.theme().border` border and the *same*
/// double box-shadow that `Dialog`'s own entrance animation converges to at
/// `delta = 1.0` (i.e. its fully-open, fully-opaque end state) — reproduced
/// here as a constant instead of an animation, since there is no animation:
/// this modal is already at that end state the first frame it exists. A
/// title row carries `title` plus a small ghost close button
/// (`gpui_component::button::Button`, `IconName::Close`, matching that same
/// pinned `Dialog`'s own close-button styling).
///
/// `viewport_width`/`viewport_height` are the window's drawable size, same
/// convention as `palette::render`/`whichkey::render` — passed in rather
/// than read from `cx` so this stays a pure function of its arguments.
///
/// Interaction: a mouse-down on the backdrop closes the modal
/// (`cx.listener` — needs `Entity<ShellView>` access to clear `view.modal`,
/// so this takes `cx: &mut Context<ShellView>`, not just `&App`); a
/// mouse-down on the panel calls `cx.stop_propagation()` so the *same*
/// bubbling event never also reaches the backdrop's handler underneath it —
/// clicking inside the modal must never close it. Escape closing the modal
/// is handled by `ShellView::handle_key_down`'s modal branch instead of
/// here — that path also has to swallow every other shell chord while the
/// modal is open, which is a key-dispatch concern, not a rendering one.
pub(crate) fn render_modal(
    title: SharedString,
    content: AnyElement,
    viewport_width: f32,
    viewport_height: f32,
    cx: &mut Context<ShellView>,
) -> impl IntoElement {
    let theme = cx.theme();
    let overlay = theme.overlay;
    let popover = theme.popover;
    let popover_foreground = theme.popover_foreground;
    let border = theme.border;
    let radius = theme.radius_lg;

    // The exact double shadow `Dialog`'s own `with_animation("slide-down",
    // ..)` closure builds (pinned checkout, same file), evaluated at
    // `delta = 1.0` — its fully-open state, which is this modal's *only*
    // state.
    let shadow = vec![
        box_shadow(px(0.), px(20.), px(25.), px(-5.), hsla(0., 0., 0., 0.1)),
        box_shadow(px(0.), px(8.), px(10.), px(-6.), hsla(0., 0., 0., 0.1)),
    ];

    let title_row = h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .gap_3()
        .px_4()
        .pt_4()
        .child(div().text_lg().child(title))
        .child(
            Button::new("shell-modal-close")
                .small()
                .ghost()
                .icon(IconName::Close)
                .on_click(cx.listener(|view, _event, _window, cx| {
                    view.modal = None;
                    cx.notify();
                })),
        );

    let panel = v_flex()
        .id("shell-modal-panel")
        .occlude()
        .gap_3()
        .pb_4()
        .bg(popover)
        .text_color(popover_foreground)
        .border_1()
        .border_color(border)
        .rounded(radius)
        .shadow(shadow)
        .debug_selector(|| "shell-modal-panel".to_string())
        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        })
        .child(title_row)
        .child(div().px_4().child(content));

    div()
        .id("shell-modal-backdrop")
        .absolute()
        .left(px(0.))
        .top(px(0.))
        .w(px(viewport_width))
        .h(px(viewport_height))
        .flex()
        .items_center()
        .justify_center()
        .bg(overlay)
        .debug_selector(|| "shell-modal-backdrop".to_string())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _event, _window, cx| {
                view.modal = None;
                cx.notify();
            }),
        )
        .child(panel)
}
