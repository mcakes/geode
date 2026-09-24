//! The uniform modal utility (Task 9, user direction: "the app will use
//! dialogs heavily; they must be created one standard way so they behave
//! uniformly") — now a **self-owned, instant** modal layer, not
//! gpui-component's own `Dialog`.
//!
//! ## Why this changed
//!
//! gpui-component's `Dialog` (pinned release
//! `gpui-component-0.6.2/src/dialog/dialog.rs`) hardwires a 250ms
//! fade+slide entrance
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
use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, Hsla, MouseButton, Pixels, SharedString,
    Window, div, hsla, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, Theme, box_shadow, h_flex, v_flex,
};

use super::ShellView;
use super::control::{ControlPaint, PointerStates as _};
use super::scale;
use crate::dialogmode::{self, DialogMode, FocusTarget};
use crate::footer::{self, Hint};
use crate::keymap::{Keystroke, Modifiers};

/// A modal's content builder, called as `build(shell, window, cx)` — see
/// [`ShellModal::build`]'s own doc comment for why the first argument is a
/// plain `&ShellView` and not `Entity<ShellView>`. A named alias rather than
/// spelling the `Rc<dyn Fn(...) -> AnyElement>` out at each of its two use
/// sites (clippy's `type_complexity`, `-D warnings`-enforced in this repo).
type ModalBuilder = Rc<dyn Fn(&ShellView, &mut Window, &mut App) -> AnyElement>;

/// A modal's optional key-handling seam (Part B, the keybinding dialog):
/// offered every keystroke while this modal is open, *before*
/// [`ShellView::handle_key_down`]'s own Escape-closes-the-modal fallback —
/// see that method's modal branch for the exact order. Returns `true` when
/// the keystroke was consumed (no further handling — not even Escape's own
/// close behavior); `false` lets the modal's built-in handling proceed,
/// which today means only "Escape closes" (every other key was already
/// unconditionally swallowed by the modal guard, with or without this
/// seam).
///
/// Takes the shell's own [`Keystroke`] (`crate::keymap::Keystroke`, already
/// run through [`super::convert_keystroke`] by the caller), not gpui's raw
/// one — a modal's key handler wants the same normalized shape every other
/// keymap-consuming path in this crate does (`vimnav::VimListNav::press`'s
/// own parameter, for one), not `gpui::Keystroke`'s platform-specific
/// fields.
///
/// `&mut ShellView` (not `Entity<ShellView>`) for the same reason
/// [`ShellModal::build`] takes a plain `&ShellView`: this is invoked from
/// inside [`ShellView::handle_key_down`], which already holds `&mut self` —
/// an `Entity<ShellView>::update` here would be a reentrant access of the
/// entity currently mid-method-call, not mid-render, but gpui's guard
/// applies equally to both. A plain reborrow carries none of that risk and
/// is exactly what the call site already has in hand.
pub type ModalKeyHandler =
    Rc<dyn Fn(&mut ShellView, &Keystroke, &mut Window, &mut Context<ShellView>) -> bool>;

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
    /// What a dialog paints in the title row between the title and the
    /// close button (§18.1): a count crumb, the mode pill. Built per
    /// frame like `build`, and for the same `&ShellView` reason. `None`
    /// for a dialog with nothing to say there (the picker, the as-of
    /// selector).
    pub title_extra: Option<TitleExtraBuilder>,
    /// This modal's optional key-handling seam (Part B) — see
    /// [`ModalKeyHandler`]'s own doc comment. Both shipped dialogs (the
    /// keybinding dialog, and the settings dialog since its row-list
    /// rewrite) now pass a handler; `open_shell_dialog` still sets this to
    /// `None` unconditionally for any future modal without key needs.
    pub on_key: Option<ModalKeyHandler>,
}

/// A modal's title-row extra builder (§18.1) — see [`ShellModal::
/// title_extra`]. Same `&ShellView`-not-`Entity<ShellView>` shape as
/// [`ModalBuilder`], and for the identical reason: it runs from inside
/// [`ShellView::render`] with `self` reborrowed, not read through the
/// entity.
pub type TitleExtraBuilder = Rc<dyn Fn(&ShellView, &mut App) -> AnyElement>;

/// Give the open modal a title-row extra (§18.1). Called straight after
/// [`open_shell_dialog_with_key`] by the dialogs that want one, rather
/// than as an eighth parameter on a door six callers already pass
/// through — a modal without one is the common case.
pub fn set_title_extra(
    view: &mut ShellView,
    build: impl Fn(&ShellView, &mut App) -> AnyElement + 'static,
) {
    if let Some(modal) = view.modal.as_mut() {
        modal.title_extra = Some(Rc::new(build));
    }
}

/// Reclaim every key gpui-component binds by default that collides with
/// Geode's own navigation vocabulary, via `gpui::NoAction` — `gpui`'s own
/// mechanism for this: a `NoAction` binding suppresses every
/// equal-or-weaker binding it outranks, so no action dispatches at all and
/// the raw `KeyDownEvent` reaches our key listeners instead
/// (`gpui::Keymap::bindings_for_input`, `gpui-pre-0.3.5/src/keymap.rs`,
/// pinned release, sorts candidate bindings by context depth — deepest
/// wins — before applying `NoAction` suppression). Each reclaim below is
/// scoped differently, on purpose:
///
/// 1. **`tab`/`shift-tab`**, scoped to `"GeodeModal"`. `Root` binds bare
///    `tab`/`shift-tab` (unconditionally, no `cx.propagate()`) to its own
///    focus-cycling actions in a `"Root"` key context wrapping the entire
///    window (`gpui-component-0.6.2/src/root.rs`, pinned release) — so
///    without a reclaim, those keystrokes are fully consumed there before
///    `ShellView`'s own raw `on_key_down` (and so a modal's
///    [`ModalKeyHandler`], e.g. the settings dialog's value-stepping) ever
///    sees them (verified against `Window::dispatch_key_event`: an action
///    binding that doesn't propagate returns before
///    `finish_dispatch_key_event` — what fires raw key listeners — ever
///    runs). Scoped to `"GeodeModal"` — the key context [`render_modal`]'s
///    panel carries only while a Geode modal is on screen — rather than
///    `"Root"` itself: `Root`'s focus cycling is a live affordance outside
///    modals too, so reclaiming it app-wide would silently remove that
///    everywhere, not just inside our own modals, which nobody asked for.
///    `"GeodeModal"` sits deeper in the dispatch path than `"Root"`
///    whenever it's present, so this binding outranks and suppresses
///    `Root`'s Tab/TabPrev only then; outside a Geode modal `"GeodeModal"`
///    is absent from the context stack, this binding is not enabled at
///    all, and `Root`'s focus cycling is untouched.
///
///    **This bullet alone does not cover a modal in normal mode.**
///    `"GeodeModal"` rides the modal PANEL, so it is on the dispatch
///    stack only while something *inside* that panel holds focus — an
///    `Input` in filter mode, say. Bullet 5 covers the other half.
///
/// 2. **`ctrl-f`**, scoped to `"Input"` and app-wide (not `"GeodeModal"`).
///    gpui-component binds `ctrl-f` to the editor `Search` action in the
///    `"Input"` context on non-macOS (the binding itself,
///    `gpui-base-0.6.2/src/input/base/state.rs`, pinned release); at the
///    pinned release the handler (`on_action_search`,
///    `gpui-base-0.6.2/src/input/editor/search.rs`) propagates when the
///    input is not `searchable`, but a searchable input still swallows it,
///    and Geode must own the chord regardless of any input's flag (at the
///    old git rev 0e2fb7a the handler returned without propagating at all,
///    which is why this reclaim was first added: without it, `ctrl+f` —
///    which both list dialogs and (per Task 5) the command palette use for
///    "page down" (`crate::listfilter::nav_command`) — would work on macOS,
///    where gpui-component doesn't bind it at all, and die silently on
///    Windows and Linux). The app-wide `NoAction` reclaim is kept as the
///    binding's guarantee rather than its only rescue. This one can't be
///    scoped to `"GeodeModal"` the way `tab` is: the command palette is not
///    a [`render_modal`] surface, so a `"GeodeModal"`-scoped binding would
///    never reach it. Going app-wide instead costs nothing, because
///    gpui-component's `Search` action is dead weight in this app — nothing
///    here uses a searchable input — unlike `Root`'s Tab cycling, which is
///    a real affordance this reclaim must not disturb outside modals.
///    (A pass-through action of
///    our own would NOT work here either: `gpui` dispatches every matched
///    binding in sequence — `Window::dispatch_key_event`, pinned release
///    (`gpui-pre-0.3.5/src/window.rs`) — so calling `cx.propagate()` from
///    our own action would simply hand the keystroke on to `Search` right
///    after.)
///
/// 3. **`ctrl-a`**, scoped to `"GeodeModal > Input"` (Phase 4a §3.3, the
///    dimension pickers) — a compound predicate, not the bare
///    `"GeodeModal"` bullet 1 uses, for a reason worth spelling out: it
///    was tried first, and it does not work. `KeyBindingContextPredicate::
///    depth_of` (pinned release, `gpui-pre-0.3.5/src/keymap/context.rs`)
///    resolves an `Identifier("GeodeModal")` predicate at the tree depth
///    of the *panel* div that carries that context — an ANCESTOR of the
///    focused `Input`, hence *shallower* than gpui-component's own
///    `"Input"`-scoped `SelectAll`/`MoveHome` binding — and depth strictly
///    outranks declaration order (`Keymap::bindings_for_input`'s own doc
///    comment: "Precedence is defined by the depth in the tree ... in the
///    case of multiple bindings at the same depth, the ones added later
///    take precedence"), so a bare `"GeodeModal"` reclaim here is simply
///    never reached: the deeper, unrelated `Input` binding always wins
///    first, exactly backwards from bullet 1's `tab` case (there the
///    competing `Root` binding sits ABOVE `"GeodeModal"`, so the deeper
///    reclaim wins outright). `"GeodeModal > Input"` (gpui's `Descendant`
///    predicate, `parent > child`) instead resolves at exactly the same
///    depth as a bare `"Input"` predicate would (its own `child` half is
///    still an `Identifier` evaluated against the *deepest* context, so
///    `depth_of` finds the same maximal depth either way) while still
///    only matching where a `GeodeModal` ancestor is actually present —
///    turning `"Input"`'s own depth-tie into a same-depth declaration-
///    order tie, which `init_reclaimed_keybindings` wins by running after
///    `gpui_component::init` (same ordering bullet 2 already relies on).
///    gpui-component binds `ctrl-a` in the `"Input"` context on every
///    platform, just to two different actions — `SelectAll` off macOS,
///    `MoveHome` (the emacs idiom) on it (same file as bullet 2's
///    `ctrl-f`) — and the picker's values stage needs the key for its own
///    "tick every value the filter currently shows"
///    (`shell::picker::PickerState::tick_all_shown`).
///
///    What this actually reclaims: `"GeodeModal"` is [`render_modal`]'s
///    OWN key context, present on every Geode modal's panel — not a
///    context private to the picker — so this binding reaches `ctrl-a`
///    inside every dialog's `Input` while a Geode modal is open: the
///    keybinding dialog's and settings dialog's shared filter field
///    (`dialog_input`, `filter_row`) lose native select-all/move-home
///    exactly as much as the picker's own filter and values stage do.
///    That is accepted, not a gap: select-all in a one-line filter box
///    (the only `Input` any of these three dialogs render) is a workflow
///    nobody uses on a single line short enough to see whole, so trading
///    it away everywhere a Geode modal is open, in exchange for the
///    picker's real "tick every value" affordance, costs nothing real —
///    unlike bullet 2's app-wide `ctrl-f` reclaim, still scoped no wider
///    than it needs to be: outside a Geode modal (the scope bar's text
///    field, the command line, …), `ctrl-a` keeps working natively, since
///    `"GeodeModal"` is absent from the context stack there. The
///    `Descendant` predicate's job is purely the depth fix above — making
///    the reclaim reach `ctrl-a` at all — not narrowing which modal it
///    reaches inside.
///
/// 4. **`tab`**, scoped to `"GeodeCommandLine"` (Phase 3 §3.4). The
///    per-tile command line's own input reuses gpui-component's `Input`
///    single-line, same as every other field here, so it hits the exact
///    same `Root`-swallows-`tab` problem bullet 1 fixes for modals — but
///    the command line is a shell-owned overlay, not a
///    [`render_modal`] surface, so `"GeodeModal"` never wraps it.
///    `commandline_view::render` gives the strip its own
///    `"GeodeCommandLine"` key context for exactly this reclaim, the same
///    narrow-scope shape as bullet 1 rather than the app-wide shape
///    bullet 2 falls back to: unlike `ctrl-f`'s dead-weight `Search`
///    action, `Root`'s Tab cycling is a real affordance the palette's and
///    dialogs' own inputs still leave alone, so this must not reach them.
///
/// 5. **`tab`/`shift-tab`**, scoped to `"GeodeModalOpen"` (whole-branch
///    review, Important 1) — bullet 1's other half, for the state bullet
///    1 cannot see. `"GeodeModal"` rides the modal PANEL, so it joins the
///    dispatch stack only while focus is *inside* the panel; the dialog
///    interaction model (`crate::dialogmode`) made **normal mode** the
///    default state of every modal list dialog, and normal mode parks
///    focus on `ShellView::focus_handle` — the window root, deliberately,
///    so bare letters arrive as verbs at `handle_key` instead of being
///    eaten by an `Input`. In that state the panel's context is absent,
///    `Root`'s `Tab` wins on its own, and `window.focus_next` walks focus
///    off the shell root onto whatever focusable sits behind the modal,
///    where `Input`-context bindings go live. This was invisible while
///    normal mode was momentary (rebind capture only); Task 3 made it the
///    resting state.
///
///    `"GeodeModalOpen"` is `ShellView::render`'s own key context on the
///    root element, present exactly while `self.modal.is_some()` — the
///    one context guaranteed to be on the stack whenever a Geode modal is
///    up, whatever holds focus. A raw-key-listener check could not fix
///    this: `Root`'s action has already fired by the time
///    `finish_dispatch_key_event` runs (bullet 1's own mechanism), so the
///    suppression has to be a keymap binding. It is a *separate* context
///    from `"GeodeModal"`, not the same name reused on the root, so that
///    bullet 3's `"GeodeModal > Input"` reclaim keeps meaning "an `Input`
///    inside the modal panel" rather than silently widening to every
///    `Input` in the window while a modal happens to be open. Both
///    contexts are live together in filter mode — two `NoAction`s at
///    different depths, same outcome — and neither is enabled with no
///    modal open, so `Root`'s cycling is untouched everywhere else.
///
/// 6. **`tab`/`shift-tab`**, scoped to `"GeodeShell"` (controller ruling
///    2026-09-20) — bullets 1, 4 and 5 generalised. Those three each
///    reclaimed the key for one surface, which left the case none of them
///    covers: a plain focused TILE, no modal, no overlay. There `Root`'s
///    cycling won, so a bare `tab` never reached
///    `ShellView::handle_key_down` and a module's own `tab` binding could
///    not fire at all — found on the timeseries tile, whose `tab` steps
///    the header's chip cursor (`timeseries::next`, timeseries spec §9.4)
///    and whose keymap-level tests all passed while the real app did
///    nothing. `"GeodeShell"` is `ShellView::render`'s unconditional key
///    context on the root element, so this reclaim covers the whole shell
///    and outranks `Root` by depth exactly as bullet 1 does. Bullets 1, 4
///    and 5 are strictly redundant with it now and are kept: each states
///    the reclaim its own surface depends on, and a surface that ever
///    moves out from under the shell root must not lose `tab` silently.
///    What is given up is `Root`'s focus cycling inside the shell — an
///    affordance nothing in Geode's keyboard model uses, since focus is
///    moved by the shell's own verbs.
///
/// Called once from `geode-app`'s `main` (after `gpui_component::init`,
/// same ordering requirement — later registrations outrank earlier ones)
/// AND from every test that opens a real modal window
/// (`shell::tests::dialog_test_shell`) — one definition rather than copies
/// that could drift, and the only way
/// `tab_and_shift_tab_step_the_selected_value` (`shell::tests`) actually
/// proves the `tab` mechanism instead of merely assuming it holds in
/// production — `shell::tests::picker`'s own e2e test drives the picker's
/// `tab` (toggle) AND `ctrl-a`/`ctrl-x` (tick-all/clear-all) through the
/// identical real pipeline, so bullet 1's mechanism is proven twice over
/// and bullet 3's is proven directly: since gpui-component binds `ctrl-a`
/// to `MoveHome` even on macOS (bullet 3's own doc), this environment
/// exercises a real collision, unlike `ctrl-f`'s below. There is no
/// equivalent test for `ctrl-f`: the behaviour it fixes only manifests on
/// a non-macOS build with a real focused input, which this environment
/// cannot exercise.
pub fn init_reclaimed_keybindings(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeModal")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodeModal")),
        gpui::KeyBinding::new("ctrl-f", gpui::NoAction, Some("Input")),
        gpui::KeyBinding::new("ctrl-a", gpui::NoAction, Some("GeodeModal > Input")),
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeCommandLine")),
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeModalOpen")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodeModalOpen")),
        // Spec §20.5: the palette overlay is not a `GeodeModal`, so it
        // never had the reclaim — `Root` could cycle focus off the query
        // field on `tab`.
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodePalette")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodePalette")),
        // User report 2026-09-18: `shift+up` in a market-data cell editor
        // moved the GRID's row selection. gpui-base's `Input` binds
        // `shift-up`/`shift-down` to `SelectUp`/`SelectDown`; a single-line
        // input returns from `select_up` without stopping propagation, so
        // the action bubbled to the enclosing `DataTable` — the same action
        // type, re-exported by gpui-component — which moved its selection
        // out from under the tile's cursor, and gpui dispatches bindings
        // before the shell's root key listener ever runs. Reclaimed in the
        // `Input` context so the keystroke falls through to the shell's own
        // keymap (a module's insert-mode binding, or nothing). Every input
        // in Geode is single-line, where the two actions were no-ops, so
        // nothing is given up; a multi-line input would want these back.
        gpui::KeyBinding::new("shift-up", gpui::NoAction, Some("Input")),
        gpui::KeyBinding::new("shift-down", gpui::NoAction, Some("Input")),
        // Controller ruling 2026-09-20: the same reclaim, widened from
        // "while a modal is open" to the whole shell. `Root` binds
        // `tab`/`shift-tab` to its own focus cycling in the `"Root"` key
        // context (`gpui-component-0.6.2/src/root.rs`), gpui dispatches a
        // matched binding before any `on_key_down` listener (bullet 1's
        // mechanism), and the shell root sits BELOW `Root` on the dispatch
        // stack — so a `NoAction` on the shell root's own `"GeodeShell"`
        // context (carried unconditionally by `ShellView::render`) is the
        // deeper match and suppresses the cycling everywhere inside the
        // shell. Without it a bare `tab` never reached
        // `ShellView::handle_key_down` while a tile was focused, and the
        // timeseries tile's `tab = timeseries::next` — the chip cursor,
        // timeseries spec §9.4 — was dead in the real app while every
        // test that drove it through the keymap directly passed. The
        // dialogs', command line's and palette's own reclaims above are
        // now redundant with this one and kept anyway: each names the
        // surface it belongs to, and a surface that ever stops being
        // inside `GeodeShell` must not silently lose its `tab`.
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeShell")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodeShell")),
    ]);
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
    window: &mut Window,
    cx: &mut Context<ShellView>,
    title: impl Into<SharedString>,
    build: F,
) where
    F: Fn(&ShellView, &mut Window, &mut App) -> AnyElement + 'static,
{
    open_shell_dialog_with_key(view, window, cx, title, build, None, false);
}

/// [`open_shell_dialog`], plus an optional [`ModalKeyHandler`] (Part B: the
/// keybinding dialog needs first refusal on every keystroke while it's
/// open, to drive rebind-capture and the shared list-navigation vocabulary
/// — see that type's own doc comment; the settings dialog joined it with
/// the row-list rewrite, for that same navigation plus value-stepping).
/// `open_shell_dialog` is simply this with `on_key: None` — no production
/// modal uses it today, but it stays as the door for any future
/// handler-less modal. Still the same one door, same open-time hygiene —
/// this is the one place that constructs a [`ShellModal`],
/// `open_shell_dialog` included.
///
/// [`ShellView::dialog_input`] is emptied on every open, whatever
/// `focus_filter` says: the field is shared between dialogs and outlives
/// each one, so a dialog whose own fresh state starts with an empty query
/// would otherwise be ranked against the *previous* dialog's leftover
/// text the moment anything focused the field.
///
/// `focus_filter` additionally focuses it, **for a dialog without a mode**
/// — a *filter-first* dialog passes `true` (the first character typed must
/// reach the filter, not fall on the floor), and `open_shell_dialog`
/// passes `false`. A modal dialog's initial focus comes from
/// [`sync_dialog_text`] instead, which this function calls unconditionally
/// at the end (spec §16.1): the keybinding dialog and the object dialog
/// both open in normal mode, where bare letters are verbs and the filter
/// must not own them until `/` says so, and they pass `false` for the
/// mode-less half of this parameter's job. A dialog that passes `true`
/// must actually render that input — [`filter_row`] — since gpui
/// dispatches keys down the *rendered* focus path and would otherwise
/// route them to the window root, past `ShellView`'s own key listener.
pub fn open_shell_dialog_with_key<F>(
    view: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
    title: impl Into<SharedString>,
    build: F,
    on_key: Option<ModalKeyHandler>,
    focus_filter: bool,
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
    // does (`close_palette` — palette-input-polish task: also returns focus
    // to the shell root, since the palette's own `Entity<InputState>` may
    // currently hold it) — the palette has no idea a modal just opened over
    // it, and its exclusive key handling would otherwise still think it
    // owns every keystroke underneath the modal.
    view.close_palette(window, cx);
    // Cancel an open command line for the identical reason (fix round 1,
    // finding 1 — `cancel_command_line`'s own doc comment): the modal
    // branch in `handle_key_down` is checked ahead of the command line's,
    // so once a modal is open every key goes to it instead, and the line
    // would otherwise sit there `Some`, still painted, but permanently
    // deaf to escape/enter/tab.
    view.cancel_command_line(window, cx);

    // Recorded after the palette close above (which may itself have just
    // returned focus to the field) and before the dialog takes focus, for
    // `close_modal` (see `ShellView::overlay_return_to_filter`).
    view.overlay_return_to_filter = view.filter_field_focused(window, cx);

    view.modal = Some(ShellModal {
        title: title.into(),
        title_extra: None,
        build: Rc::new(build),
        on_key,
    });

    // Reset by value, not by rebuilding the entity — the same lifecycle
    // `toggle_palette` gives `palette_input` (see `ShellView::
    // dialog_input`'s own doc comment). `set_value` does not emit
    // `InputEvent::Change` (checked against the pinned release, same as
    // `toggle_palette`'s own comment records), so this reset never
    // reaches the subscription; each dialog's fresh state already starts
    // with an empty query. Unconditional since the keybinding dialog went
    // modal: a dialog that opens *unfocused* can still focus this field
    // later (`/`), and it would then inherit whatever the last dialog
    // left in it while its own `state.query` said empty.
    view.dialog_input
        .update(cx, |input, cx| input.set_value("", window, cx));
    if focus_filter {
        let handle = view.dialog_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }
    // A dialog opened from a MOUSE-DOWN — the scope bar's chips and
    // grouping readout, the sidebar's gear — keeps the focus it just
    // took (grouping-picker work, 2026-09-19): gpui focuses the
    // `track_focus`ed element under the pointer on the bubble phase of
    // that same mouse-down unless `prevent_default` was called by an
    // earlier listener, and the chip's own listener (this call) runs
    // before the shell root's, so without this the root took the field's
    // focus back a moment after the dialog opened and typing went
    // nowhere (`the_pick_chip_is_always_present_and_opens_the_picker`
    // pins it on the `+` chip). On a KEY dispatch the flag is inert for
    // this shell: every dispatch resets it first, the only key-event
    // readers are `div`'s enter/space keyboard-click emulation for a
    // FOCUSED element with click listeners (nothing in the shell is
    // one), and neither platform crate reads `DispatchEventResult::
    // default_prevented` — so the key paths pay nothing for sharing the
    // door. gpui-component's own `Button` does the same in its
    // mouse-down ("avoid focus on mouse down").
    window.prevent_default();
    // The open-door seam of [`sync_dialog_text`]'s five seam classes
    // (spec §16.1/§16.6, folded to five by §17.3): a no-op for the
    // mode-less dialogs the `focus_filter` branch above
    // just served, and the *initial* focus for a modal one —
    // `keybindings_view::open`, `objectdialog::open` and
    // `settings_view::open` all set their state before calling this
    // door, so the sync sees the mode they open in.
    sync_dialog_text(view, window, cx);

    cx.notify();
}

/// Make gpui agree with the open modal dialog's pure state (spec §16.1):
/// focus goes where [`crate::dialogmode::focus_target`] says, and the
/// shared `Input` holds the dialog's effective query.
///
/// Called at five seam classes and nowhere else (§16.6's four, plus the
/// mouse-parity handlers §17.1 rule 3 adds to the second class) — the
/// tail of the modal branch in `ShellView::handle_key_down`,
/// [`open_shell_dialog_with_key`], the object dialog's two
/// confirm-button closures, and every mouse handler that ends a dialog
/// transition: the frozen filter row's click
/// ([`enter_filter_by_mouse`]), both dialogs' browse/list row clicks
/// (the keybinding dialog's, and the object dialog's `on_row_clicked` /
/// `on_edit_row_clicked`), and, since §17's amendment, the object
/// dialog's edit-stage mouse verbs — the tick click (`on_tick_clicked`),
/// a row drop (`on_row_dropped`) and the chain field's completion click
/// (`on_completion_clicked`). A click never reaches the key path, so
/// each of these handlers is its transition's only tail; `press_verb`
/// is the audited exception, since it can only arm a `Confirm` and
/// moves neither mode nor query. A no-op when no modal dialog with a
/// mode is open; the picker is the one filter-only dialog that still
/// keeps its own open-time focus (§16.4) — the as-of dialog USED to as
/// well, until review round 2's finding 1: its `tab`/`escape`/`enter`
/// arms mutate `AsOfState::query` directly (`open_field` clears it,
/// `set_query` re-feeds it), and nothing wrote that back to the shared
/// `Input`, so the field kept showing stale typed text the model no
/// longer held — `tab` then `down` then `enter` could commit the row
/// under the STALE filter rather than the one under the highlight. The
/// as-of arm below is checked first and returns unconditionally: this
/// dialog has no `mode`/`listening` to weigh, so its query is always
/// the truth and the field always follows it, focused, the whole time
/// the dialog is open. The three `if let` arms below it (the settings
/// dialog joined on 2026-09-12, spec §18) are mutually exclusive by
/// `close_modal`'s contract (it clears every dialog state together), so
/// their order carries no meaning.
///
/// The text write is guarded by a compare because `InputState::set_value`
/// emits no `InputEvent::Change`: writing unconditionally would be
/// harmless for the mirror but would move the caret on every keystroke.
/// The compare reads `InputState::text()`, a borrowed rope, rather than
/// `value()`, which copies the whole text into a fresh string — this runs
/// per keystroke, and PHILOSOPHY counts that churn as a defect. Focusing
/// an already-focused handle is idempotent.
pub(crate) fn sync_dialog_text(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    // The as-of dialog checked first and returned from unconditionally:
    // it has no `DialogMode`/`listening` to route through the `mode`
    // arms below, its query always lives in the shared field, and it is
    // always focused while the dialog is open (review round 2, finding
    // 1 — see this function's own doc comment).
    if let Some(state) = shell.as_of_dialog.as_ref() {
        let query = state.query();
        let input = shell.dialog_input.clone();
        if input.read(cx).text() != query {
            input.update(cx, |i, cx| i.set_value(query, window, cx));
        }
        input.read(cx).focus_handle(cx).focus(window, cx);
        return;
    }
    let (mode, listening, query) = if let Some(state) = shell.keybindings.as_ref() {
        (state.mode, state.listening.is_some(), state.query.as_str())
    } else if let Some(state) = shell.object_dialog.as_ref() {
        (state.mode, false, state.effective_query())
    } else if let Some(state) = shell.settings.as_ref() {
        (state.mode, false, state.effective_query())
    } else {
        return;
    };
    let input = shell.dialog_input.clone();
    if input.read(cx).text() != query {
        input.update(cx, |i, cx| i.set_value(query, window, cx));
    }
    match dialogmode::focus_target(mode, listening) {
        FocusTarget::Input => input.read(cx).focus_handle(cx).focus(window, cx),
        FocusTarget::Shell => shell.focus_handle.focus(window, cx),
    }
}

/// The drop shadow every floating overlay panel in this shell wears —
/// [`render_modal`]'s panel and the command palette's
/// (`palette::render`), so the two read as siblings rather than as two
/// unrelated components. Lifted here from `render_modal`, which used to
/// build it inline, when the palette adopted it.
///
/// It is the exact double shadow gpui-component's own `Dialog` builds in
/// its `with_animation("slide-down", ..)` closure (pinned release,
/// `gpui-component-0.6.2/src/dialog/dialog.rs`), evaluated at
/// `delta = 1.0` — that animation's fully-open state, which is the only
/// state Geode's own instant modals and palette ever have. Keeping the
/// values identical is what lets our self-owned chrome sit next to that
/// crate's own popovers without looking like a different design system.
///
/// The `hsla(0., 0., 0., 0.1)` here is a *shadow*, not a UI color: it is
/// the same neutral black-at-10% in either theme mode, which is why it is
/// a literal rather than a `cx.theme()` token (there is no shadow token to
/// read, and tinting a shadow with a theme color is what would actually
/// look wrong).
pub(crate) fn overlay_panel_shadow() -> Vec<gpui::BoxShadow> {
    vec![
        box_shadow(px(0.), px(20.), px(25.), px(-5.), hsla(0., 0., 0., 0.1)),
        box_shadow(px(0.), px(8.), px(10.), px(-6.), hsla(0., 0., 0., 0.1)),
    ]
}

/// What [`filter_row`] paints instead of the live `Input`: the query as
/// static muted text, plus whether `/` is currently the way back to
/// typing it.
///
/// A caret blinking in a field that is not receiving the keys is the
/// single most misleading thing a modal surface can show, so every
/// dialog freezes the row whenever the `Input` does not own the
/// keystrokes. But "the input is frozen" and "`/` opens the filter" are
/// two different claims, and exactly one frozen state separates them:
/// the keybinding dialog while it is *listening* for a capture, where
/// `press_while_listening` swallows every keystroke and `/` becomes the
/// new binding rather than a request to filter. Painting §18.1's
/// `press / to filter` placeholder there would put a hint for a key that
/// does something else on screen beside a footer already reading
/// "Listening — type keys" — two claims contradicting each other, which
/// is worse than the bare search icon that state showed before.
///
/// Hence the second field. It is not "should we show the hint": it is
/// "is the hint TRUE here", which is why the caller that knows (the
/// dialog, which owns the capture state) answers it rather than
/// `filter_row` guessing from the query.
pub struct FrozenFilter<'a> {
    /// The query to echo as static text. Empty is the state every
    /// normal-mode dialog opens in.
    pub query: &'a str,
    /// Whether a bare `/` would enter filter mode from here. `false`
    /// only while the keybinding dialog is capturing a keystroke.
    pub slash_filters: bool,
    /// The shell, so the frozen row's mouse-down can reach
    /// [`enter_filter_by_mouse`] (§17.1 rule 1). Only the frozen branch
    /// needs it — the live `Input` branches are already focused.
    pub entity: Entity<ShellView>,
}

/// Enter filter mode from the frozen row's mouse-down. The shared entry helper
/// snapshots the query exactly as `/` does; [`sync_dialog_text`] reconciles focus
/// after the handler returns.
///
/// Cancel a keybinding capture first so its focus priority cannot keep keys on the
/// shell root while the dialog displays filter mode. An armed confirmation blocks
/// this transition until answered.
pub(crate) fn enter_filter_by_mouse(shell: &mut ShellView) {
    if let Some(state) = shell.keybindings.as_mut() {
        // Spec §20.1: not over an open question.
        if state.confirm.is_some() {
            return;
        }
        state.listening = None;
        dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
    } else if let Some(state) = shell.object_dialog.as_mut() {
        // `build_edit` still paints the frozen row during confirmation, so guard
        // the transition here as well as in keyboard routing.
        if state.confirm.is_some() {
            return;
        }
        state.enter_filter();
    } else if let Some(state) = shell.settings.as_mut() {
        dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
    }
}

/// The filter row every list dialog wears at the top: the shared
/// `Input`, chrome stripped (`appearance(false)`) with a bottom border
/// standing in for it — the palette's own `input_row` idiom
/// (`palette::render`), so the three filtering surfaces look alike.
///
/// A muted [`IconName::Search`] rides in the input's `prefix` slot rather
/// than a `"filter"` placeholder (user direction). That is also
/// gpui-component's own idiom for this exact surface — its command
/// palette builds the identical `Input::prefix(Icon::new(IconName::Search)
/// .text_color(muted_foreground))` + `appearance(false)` pair (pinned
/// release, `gpui-component-0.6.2/src/command/state.rs`, `Command`'s
/// searchable-header render) — so the icon is left at its default size
/// to match. `prefix` survives `appearance(false)`: that flag guards
/// only the background and border (`gpui-component-0.6.2/src/input/
/// input.rs`, `Input`'s render), never the prefix child.
///
/// `frozen` renders a muted, static copy of the query *instead of* the
/// live input: the keybinding dialog passes `Some(query)` whenever the
/// input is blurred — while listening for a binding, and (since it went
/// modal, `crate::dialogmode`) throughout normal mode — because a caret
/// would be a lie about where keystrokes are going. It keeps the icon, and
/// hand-matches `Input`'s own medium-size prefix gap (`px(6.)`, the
/// `gap_x` match in `gpui-component-0.6.2/src/input/input.rs`, `Input`'s
/// render), so entering and leaving capture doesn't shift the query
/// text sideways.
///
/// An empty frozen query — the state every normal-mode dialog OPENS in —
/// is the one case that paints something the query itself did not
/// supply: a muted `press / to filter` placeholder (§18.1). Nothing else
/// on screen says the key exists, and the alternative is a row showing
/// only an icon, which reads as a disabled control rather than an
/// unfocused one. It is gated on [`FrozenFilter::slash_filters`], because
/// "the input is frozen" and "`/` opens the filter" are not the same
/// claim — see that field's own doc.
pub fn filter_row(
    input: &Entity<InputState>,
    frozen: Option<FrozenFilter<'_>>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let row = div().w_full().border_b_1().border_color(theme.border);
    let search_icon = || Icon::new(IconName::Search).text_color(theme.muted_foreground);
    match frozen {
        // §17.1 rule 1: the frozen row is the mouse form of `/` — a
        // mouse-down anywhere on it is a pure `enter_filter_by_mouse` +
        // `sync_dialog_text`, the same shape every other mouse-driven
        // dialog transition in this crate takes (see `sync_dialog_text`'s
        // own doc comment for the five seam classes). `cursor_text`
        // (gpui's I-beam) tells the eye the row is typeable before the
        // click, which a plain arrow cursor over static text would not.
        Some(frozen) => {
            let entity = frozen.entity.clone();
            let row = row
                .py_1()
                .cursor_text()
                .debug_selector(|| "dialog-filter-frozen".to_string())
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    entity.update(cx, |shell, cx| {
                        enter_filter_by_mouse(shell);
                        sync_dialog_text(shell, window, cx);
                        cx.notify();
                    });
                });
            // §18.1: a frozen, EMPTY query is the state every
            // normal-mode dialog opens in — nothing yet says `/` exists.
            // A placeholder in the search icon's own row is the one
            // place a trader's eye already goes to check "is this thing
            // typeable right now". The selector rides the text itself,
            // not the row around it — a row paints regardless of what
            // its text says, so a selector on the row alone would still
            // be found even if the label emptied out from under it
            // (exactly the "markers, not values" failure this crate's
            // mutation harness exists to catch — see the harness's own
            // header comment).
            //
            // `slash_filters` is the second half of the condition, not a
            // refinement of it: a hint for a key that does something
            // else where it is painted is worse than no hint, and the
            // keybinding dialog's capture state is exactly that (see
            // `FrozenFilter`).
            let body = if frozen.query.is_empty() && frozen.slash_filters {
                div()
                    .debug_selector(|| "dialog-filter-placeholder".to_string())
                    .child("press / to filter")
                    .into_any_element()
            } else {
                // Either a real query to echo, or an empty one in a
                // state where `/` is not the filter's key — the bare
                // icon this row painted for every frozen query before
                // §18.1.
                div().child(frozen.query.to_string()).into_any_element()
            };
            row.child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(theme.muted_foreground)
                    .child(search_icon())
                    .child(body),
            )
            .into_any_element()
        }
        None => row
            .child(
                Input::new(input)
                    .appearance(false)
                    .prefix(search_icon())
                    .w_full(),
            )
            .into_any_element(),
    }
}

/// The name field a dialog shows while creating an object (§18.2): the
/// same shared `Input`, with a muted label (`New view · name`) where the
/// filter row has its search icon. The `Input` is the filter's — the
/// dialog's `set_query` subscription mirrors the name the way it mirrors
/// a query — so there is no second text buffer for THIS function to reset
/// or focus. It is not a promise that the field arrives empty on its
/// own: `set_value` does not emit the `Change` event that mirroring
/// relies on, so the caller entering the naming stage still has to clear
/// the shared `Input` itself (`render`'s `n` handling does, right beside
/// the focus call) or a leftover browse filter shows up pre-filled here.
pub fn name_row(input: &Entity<InputState>, label: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .w_full()
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "dialog-name-row".to_string())
        .child(
            Input::new(input)
                .appearance(false)
                .prefix(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label.to_string()),
                )
                .w_full(),
        )
        .into_any_element()
}

/// The small pill naming a modal dialog's current mode
/// (`crate::dialogmode`), for the top-right of its content.
///
/// A modal surface has no caret in normal mode and a caret in filter
/// mode, which is a real but easily-missed difference — the pill is what
/// makes "your letters are verbs right now" legible without the user
/// having to type one and find out. It lives here rather than in
/// `keybindings_view` because the 4c surfaces adopt the same vocabulary
/// and must wear the same badge; a second copy would drift.
///
/// Colours are `cx.theme()` tokens (house rule: never a raw colour).
/// Filter mode takes `primary`, the same token the fuzzy-match highlight
/// and the selected row use — it is the "you are typing into something"
/// state; normal mode takes the muted pair every other inert chip in
/// these dialogs wears ([`super::keybindings_view::key_chip`]'s own
/// `muted`/`muted_foreground`), because normal is the resting state, not
/// an alert.
///
/// The labels are lowercase where the spec writes `NORMAL`/`FILTER`:
/// deliberate, and a user ruling — lowercase is what this crate's key
/// rendering already uses everywhere (`palette::render_keystroke`'s
/// `ctrl+k`), and a shouted badge beside those chips would read as a
/// different design system.
///
/// The label rides in the `debug_selector` too, so a test can assert
/// *which* mode painted rather than only that something did — a pill
/// showing the same label in both modes is exactly the failure a
/// non-zero-bounds assertion cannot see.
pub(crate) fn mode_pill(mode: DialogMode, cx: &App) -> AnyElement {
    match mode {
        DialogMode::Normal => state_pill("normal", false, cx),
        DialogMode::Filter => state_pill("filter", true, cx),
    }
}

/// The pill the object dialog wears while its chain field is open
/// (Phase 4c §18.8). The field runs in `DialogMode::Filter` — that is
/// what hands the shared `Input` the keys — but its text is a *value*
/// being typed, not a query narrowing a list, and a pill reading
/// `filter` over it would say the wrong thing about what `enter` does.
/// Painted in the "you are typing into something" colours, since that
/// half of `filter`'s claim is still true.
pub(crate) fn chain_pill(cx: &App) -> AnyElement {
    state_pill("chain", true, cx)
}

/// The pill while a plain value field is open (§19.1): `edit`, the same
/// `primary` "you are typing" pair `chain_pill` uses — `filter` would
/// misdescribe what `enter` does. Selector `dialog-mode-pill-edit`.
pub(crate) fn edit_pill(cx: &App) -> AnyElement {
    state_pill("edit", true, cx)
}

/// The pill while a `Choice` row's typeahead is open (spec 2026-09-19
/// §3.2): `choose`, the `primary` "you are typing" pair. Selector
/// `dialog-mode-pill-choose`.
pub(crate) fn choose_pill(cx: &App) -> AnyElement {
    state_pill("choose", true, cx)
}

/// The ranked options of an open choice field, painted in a dialog's
/// row list's place (spec 2026-09-19 §3.2/§3.3): one row per PAINTED
/// entry of `list`, the highlighted one in the selection colours every
/// list in these dialogs uses, matched characters highlighted through
/// `highlighted_text`. At most `list.painted_len()` rows (12), so no
/// scroll container: the query narrows the rest. `on_click(row)` is the
/// mouse form of `tab` on that row — the caller decides what that means.
/// Selectors: `{prefix}-choice-list` on the list, `{prefix}-choice-{text}`
/// on each row.
pub(crate) fn choice_rows(
    list: &crate::choice::ChoiceList,
    prefix: &'static str,
    scroll: &gpui::ScrollHandle,
    theme: &Theme,
    on_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    // Every ranked row, inside a viewport `CHOICE_VISIBLE_ROWS` tall at
    // most, scrolled by the wheel and by `scroll_to_item` from the key
    // paths — the palette's and every dialog row list's own shape (user
    // report 2026-09-19: the twelve-row window `ChoiceList` keeps for the
    // market-data picker gave the wheel nothing to scroll here). The
    // highlight is compared in RANKED space (`ranked_highlighted`), the
    // click hands back a ranked index, and the row colours come through
    // `row_paint`, the one door every list row's state colours take.
    let paint = super::listrow::row_paint(theme);
    let rows_len = list.ranked().len();
    let mut rows = v_flex()
        .id(gpui::SharedString::from(format!("{prefix}-choice-list")))
        .w_full()
        .h(scale::design(
            (rows_len.clamp(1, CHOICE_VISIBLE_ROWS) as f32) * CHOICE_ROW_HEIGHT,
        ))
        .overflow_y_scroll()
        .track_scroll(scroll)
        .debug_selector(move || format!("{prefix}-choice-list"));
    for (position, ranked) in list.ranked().iter().enumerate() {
        let text = &list.options()[ranked.row];
        let selector = format!("{prefix}-choice-{text}");
        let on_click = on_click.clone();
        let mut row = h_flex()
            .w_full()
            .h(scale::design(CHOICE_ROW_HEIGHT))
            // The container is a fixed-height column: without this a
            // flex item with an explicit height shrinks to its text to
            // fit, and forty rows squeeze into the viewport instead of
            // scrolling past it.
            .flex_shrink_0()
            .px_3()
            .items_center()
            .text_sm()
            .rounded(theme.radius)
            .debug_selector(move || selector.clone())
            .child(super::keybindings_view::highlighted_text(
                text,
                &ranked.indices,
                paint.accent,
            ))
            .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                on_click(position, window, cx);
            });
        if position == list.ranked_highlighted() {
            row = row.bg(paint.active).text_color(paint.text);
        } else {
            row = row.hover(move |s| s.bg(paint.hover));
        }
        rows = rows.child(row);
    }
    rows.into_any_element()
}

/// A choice row's height at the design rem, and how many the viewport
/// shows before it scrolls — the same twelve `ChoiceList`'s window holds
/// for the picker, so the two surfaces agree on how tall a list looks.
const CHOICE_ROW_HEIGHT: f32 = 28.0;
const CHOICE_VISIBLE_ROWS: usize = crate::choice::DEFAULT_CAP;

/// The one pill [`mode_pill`], [`chain_pill`] and [`edit_pill`] all
/// paint: `typing`
/// picks the `primary` pair (a focused text field owns the keys) over the
/// muted resting pair. The label rides in the selector so a test can
/// assert which state painted.
fn state_pill(label: &'static str, typing: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (fg, bg) = if typing {
        (theme.primary_foreground, theme.primary)
    } else {
        (theme.muted_foreground, theme.muted)
    };
    div()
        .font_family(crate::fonts::MONO)
        .text_xs()
        .text_color(fg)
        .bg(bg)
        .px_1p5()
        .py_0p5()
        .rounded(theme.radius)
        .flex_shrink_0()
        .debug_selector(move || format!("dialog-mode-pill-{label}"))
        .child(label)
        .into_any_element()
}

/// A bordered mono pill for a *classification* — a layer, `overridden`,
/// `drifted`, a field's destination (§18.1). Distinct from
/// [`super::keybindings_view::key_chip`]
/// (filled, for a keystroke) and [`mode_pill`] (filled, for a state): a
/// badge is outlined so a row wearing three of them still reads as one
/// row. `fg` colours text and `border` the outline; the fill is the
/// panel's own — a badge never paints a background, which is the whole
/// reason three of them stack readably where three filled chips do not.
///
/// `cx` is taken and unused on purpose: every other element helper in
/// this module reads the theme itself, and a caller that already has the
/// two tokens in hand should not have to remember that THIS one is the
/// exception. It also leaves room for the badge to start reading a token
/// of its own without touching six call sites.
///
/// Task 8's object dialog is its first caller — the browse rows' and
/// edit header's filled `overridden`/`layer`/`new` chips, plus each
/// field's `doc`/`pres` destination marker — and it lands with the
/// shared chrome rather than with its first user so that every surface
/// Task 8 touches reaches for the same helper instead of one of them
/// inventing a second.
pub(crate) fn badge(
    label: impl Into<SharedString>,
    fg: Hsla,
    border: Hsla,
    selector: Option<String>,
    cx: &App,
) -> AnyElement {
    let radius = cx.theme().radius_tokens().sm;
    let label = label.into();
    let mut el = div()
        .font_family(crate::fonts::MONO)
        .text_xs()
        .text_color(fg)
        .border_1()
        .border_color(border)
        .px_1()
        .rounded(radius)
        .flex_shrink_0()
        .child(label);
    if let Some(selector) = selector {
        el = el.debug_selector(move || selector.clone());
    }
    el.into_any_element()
}

/// A small filled square painting one resolved colour — the Colours
/// dialog's live swatch (Part 2c §6.1), on a browse row and beside the
/// edit header's own name. `colour` is already-resolved data (the
/// definition run through `geode_core::colour::resolve` over `shell::colours`' anchors and tokens
/// against the active theme), never a raw literal picked here — the one
/// deliberate exception to "no raw colour in chrome" this crate's other
/// chrome follows, because the whole point of this element is to show
/// the trader exactly what a name resolves to. The border stays
/// `theme.border` regardless, so the swatch never borrows the resolved
/// fill for its own outline.
pub(crate) fn swatch(colour: Hsla, selector: String, cx: &App) -> AnyElement {
    div()
        .w(scale::design(14.))
        .h(scale::design(14.))
        .rounded(cx.theme().radius_tokens().sm)
        .border_1()
        .border_color(cx.theme().border)
        .bg(colour)
        .debug_selector(move || selector.clone())
        .into_any_element()
}

/// Cap on the modal panel's height, as a fraction of the window's viewport
/// height — a *max*, not a fixed size (see [`render_modal`]'s `.max_h`
/// use): a small dialog's panel still hugs its own content, this only
/// stops a tall one from growing past 80% of the viewport, past which its
/// content region (also set up in [`render_modal`]) scrolls internally
/// instead. (The settings-dialog rewrite removed this constant's one
/// external reader — `settings_view::open`'s composite-era content
/// wrapper, which had to re-derive its own explicit height from this same
/// ratio; see the history note on [`render_modal`]'s height contract.)
pub(crate) const MODAL_MAX_HEIGHT_RATIO: f32 = 0.8;

/// Fraction of the viewport height every modal's top edge sits below the
/// backdrop's top (user direction: dialogs share one top edge rather than
/// centering vertically — differently-sized dialogs centering to
/// different heights defeats spatial memory). 0.1 pairs with
/// [`MODAL_MAX_HEIGHT_RATIO`]'s 0.8: a full-height dialog gets the same
/// 10% margin below as above, and anything smaller hangs from the shared
/// top edge.
pub(crate) const MODAL_TOP_RATIO: f32 = 0.1;

/// Render one open modal's chrome: a full-window backdrop on
/// `cx.theme().overlay` (the same token gpui-component's own `Dialog`
/// overlay uses — `overlay_color`, pinned release
/// `gpui-component-0.6.2/src/dialog/dialog.rs`), and over it —
/// horizontally centered, top edge anchored at
/// [`MODAL_TOP_RATIO`] so every dialog starts at the same line — a panel
/// on `cx.theme().popover`/
/// `popover_foreground` with a `cx.theme().border` border and the *same*
/// double box-shadow that `Dialog`'s own entrance animation converges to at
/// `delta = 1.0` (i.e. its fully-open, fully-opaque end state) — reproduced
/// here as a constant instead of an animation, since there is no animation:
/// this modal is already at that end state the first frame it exists. A
/// title row carries `title`, then (§18.1) whatever `title_extra` built —
/// a count crumb, the mode pill — then a small ghost close button
/// (`gpui_component::button::Button`, `IconName::Close`, matching that same
/// pinned `Dialog`'s own close-button styling). `title_extra` and the
/// close button share one right-hand `h_flex`, so `.justify_between()`
/// still reads as exactly two sides: the title, and everything else.
///
/// `viewport_width`/`viewport_height` are the window's drawable size, same
/// convention as `palette::render`/`whichkey::render` — passed in rather
/// than read from `cx` so this stays a pure function of its arguments.
///
/// **Height contract** (content-collapse fix): the panel gets
/// `.max_h(viewport_height * MODAL_MAX_HEIGHT_RATIO)` — a cap, so a small
/// future dialog's panel still sizes to its own content instead of always
/// ballooning to 80% of the window. The content region below the title row
/// is `.flex_auto().overflow_y_scrollbar()`: `flex_auto` (flex-grow AND
/// flex-shrink, but flex-*basis* `auto`, i.e. based on the region's own
/// content — unlike `flex_1`'s zero basis, which discards that content
/// size and collapses to nothing when the panel above it has no definite
/// height of its own to grow into) lets the region size itself to whatever
/// its content naturally needs, then shrink and scroll if the panel's
/// `max_h` cap ends up smaller than that. (History: the composite-era
/// settings dialog additionally needed its own explicitly-heighted content
/// wrapper here, because `gpui_component::setting::Settings`' root demanded
/// `size_full` — a percentage with nothing definite to resolve against.
/// The settings-dialog rewrite removed that composite; both list dialogs
/// now size their row lists explicitly, so `flex_auto` + `max_h` alone is
/// the whole contract again.)
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
    title_extra: Option<AnyElement>,
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

    let shadow = overlay_panel_shadow();

    let title_row = h_flex()
        .w_full()
        .justify_between()
        .items_center()
        .gap_3()
        .px_4()
        .pt_4()
        .child(
            div()
                .text_lg()
                .child(title)
                .debug_selector(|| "shell-modal-title".to_string()),
        )
        .child(
            h_flex().gap_2().items_center().children(title_extra).child(
                Button::new("shell-modal-close")
                    .small()
                    .ghost()
                    .icon(IconName::Close)
                    .on_click(cx.listener(|view, _event, window, cx| {
                        view.close_modal(window, cx);
                    })),
            ),
        );

    let panel = v_flex()
        .id("shell-modal-panel")
        // A key context, on a modal that is otherwise "plain chrome, not
        // an action-dispatch layer" (see `handle_key_down`'s modal branch
        // in `shell/mod.rs`) — added not to dispatch anything of our own,
        // but to SUPPRESS one: gpui-component's `Root` binds bare
        // `tab`/`shift-tab` to its own focus-cycling actions in a `"Root"`
        // key context that wraps the entire window
        // (`gpui-component-0.6.2/src/root.rs`, pinned release),
        // unconditionally, and those handlers never call `cx.propagate()`
        // — so a plain `tab` keystroke is fully consumed by `Root` before
        // `ShellView`'s own raw `on_key_down` ever sees it (verified
        // against `Window::dispatch_key_event`: an action binding that
        // doesn't propagate returns before `finish_dispatch_key_event`,
        // which is what fires raw key listeners, ever runs). `"GeodeModal"`
        // is this panel's own, narrower context, present only while a
        // Geode modal is on screen and sitting DEEPER in the dispatch
        // path than `Root`'s outer wrapper — [`init_reclaimed_keybindings`]
        // binds `tab`/`shift-tab` to `NoAction` here, which (being the
        // deeper, and so higher-precedence, match — `gpui`'s
        // `Keymap::bindings_for_input` sorts by context depth) suppresses
        // `Root`'s binding while a modal is open and leaves it untouched
        // everywhere else in the app.
        .key_context("GeodeModal")
        .occlude()
        .gap_3()
        .pb_4()
        .bg(popover)
        .text_color(popover_foreground)
        .border_1()
        .border_color(border)
        .rounded(radius)
        .shadow(shadow)
        .max_h(px(viewport_height * MODAL_MAX_HEIGHT_RATIO))
        .debug_selector(|| "shell-modal-panel".to_string())
        .on_mouse_down(MouseButton::Left, |_event, _window, cx| {
            cx.stop_propagation();
        })
        .child(title_row)
        .child(
            div()
                .flex_auto()
                .overflow_y_scrollbar()
                .px_4()
                .child(content),
        );

    div()
        .id("shell-modal-backdrop")
        .absolute()
        .left(px(0.))
        .top(px(0.))
        .w(px(viewport_width))
        .h(px(viewport_height))
        .flex()
        // Horizontally centered, top-anchored at MODAL_TOP_RATIO (not
        // `items_center`'s vertical centering — see that constant's doc):
        // every dialog's top edge lands on the same line.
        .items_start()
        .justify_center()
        .pt(px(viewport_height * MODAL_TOP_RATIO))
        .bg(overlay)
        .debug_selector(|| "shell-modal-backdrop".to_string())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _event, window, cx| {
                view.close_modal(window, cx);
            }),
        )
        .child(panel)
}

/// The footer hint rows every modal dialog paints — the one renderer of
/// [`crate::footer`]'s layout (spec §19): a row per [`crate::footer::HintRow`] that has
/// something in it, in `HintRow::ALL` order, each led by its dim
/// fixed-width label so the eye can find "edit" or "go" without reading
/// the line. Hints on a row are joined with ` · `; a hint's keys paint
/// as [`key_chip`]s (with `between` between the first two, for a range
/// like `1 – 9`). A hint with a selector gives every one of its chips
/// `"<selector>-<key>"` and its first chip the bare `"<selector>"` as
/// well, so a test can ask both "is this group painted" and "is this
/// particular key taught in it" — the second is what lets a test prove
/// `tab` is named in normal mode rather than merely that some stepping
/// key is.
///
/// The dialogs only decide which hints are live; nothing here lets a
/// caller pick a row, which is what keeps `space` under `space` on
/// every surface.
pub(crate) fn hint_rows(
    hints: &[Hint],
    chip_fg: Hsla,
    chip_bg: Hsla,
    chip_radius: Pixels,
) -> AnyElement {
    let chip = |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        super::keybindings_view::key_chip(&ks, chip_fg, chip_bg, chip_radius)
    };
    let mut lines = v_flex().gap_0p5();
    for (row, members) in footer::rows(hints) {
        let label = row.label();
        let mut line = h_flex()
            .gap_1()
            .items_center()
            .flex_wrap()
            .debug_selector(move || format!("hint-row-{label}"))
            .child(
                div()
                    .w(scale::design(30.))
                    .flex_shrink_0()
                    .text_xs()
                    .child(label),
            );
        // An empty row keeps a full row's height, so the footer never
        // grows or shrinks with the selected row's vocabulary: the label
        // alone is a `text_xs` line, shorter than a chip, so an unpainted
        // chip sets the height — `invisible` lays out and paints nothing.
        if members.is_empty() {
            line = line.child(div().invisible().child(chip("space")));
        }
        let last = members.len().saturating_sub(1);
        for (i, hint) in members.into_iter().enumerate() {
            for (k, key) in hint.keys.iter().enumerate() {
                if k == 1
                    && let Some(between) = hint.between
                {
                    line = line.child(div().child(between));
                }
                let chip = chip(key);
                line = line.child(match hint.selector {
                    Some(selector) => {
                        let keyed = format!("{selector}-{key}");
                        let inner = div().debug_selector(move || keyed.clone()).child(chip);
                        if k == 0 {
                            div()
                                .debug_selector(move || selector.to_string())
                                .child(inner)
                                .into_any_element()
                        } else {
                            inner.into_any_element()
                        }
                    }
                    None => chip,
                });
            }
            let word = if i == last {
                hint.word.to_string()
            } else {
                format!("{} ·", hint.word)
            };
            line = line.child(div().child(word));
        }
        lines = lines.child(line);
    }
    lines.into_any_element()
}

/// The answer to a destructive question, on every surface that asks one
/// (spec §20.1): the object dialog's `d`/`r`/`o` and the keybindings
/// dialog's `d`/`r`. `None` means the key is neither answer — the caller
/// claims and drops it, because a stray letter must not act on the
/// object behind an unanswered question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAnswer {
    Yes,
    No,
}

impl ConfirmAnswer {
    /// `y`/`enter` bare are yes; `n` bare and `escape` with any modifiers
    /// are no (modifier-agnostic on `escape` for the same reason every
    /// dialog's close is: `shift+escape` must not be a key that visibly
    /// does nothing).
    pub fn from_key(ks: &Keystroke) -> Option<ConfirmAnswer> {
        let bare = ks.mods == Modifiers::NONE;
        match ks.key.as_str() {
            "y" | "enter" if bare => Some(ConfirmAnswer::Yes),
            "n" if bare => Some(ConfirmAnswer::No),
            "escape" => Some(ConfirmAnswer::No),
            _ => None,
        }
    }
}

/// What a confirm button runs. `Rc` so the two closures can be cloned
/// into gpui's `'static` click handlers.
pub type ConfirmHandler = Rc<dyn Fn(&mut ShellView, &mut Window, &mut Context<ShellView>)>;

/// The confirm block every dialog paints in place of its action bar
/// while a destructive question stands (spec §20.1): the question in
/// `theme.warning`, a `danger` button labelled with the verb, and a ghost
/// `Cancel`. Both handlers are mouse-side answers and so end in
/// [`sync_dialog_text`] and a `cx.notify()` here, once, rather than in
/// each caller (spec §16.1: a click never passes through the key path).
/// The notify is unconditional on both buttons: a yes handler usually
/// notifies on its own way through, but a no handler only clears the
/// armed state, and the symmetry keeps a third consumer honest — neither
/// button may leave the disarmed (or written) dialog painting its
/// question. Selectors: `"{selector_prefix}-confirm"`, `-yes`, `-no`.
pub(crate) fn confirm_row(
    prompt: String,
    yes_label: &'static str,
    selector_prefix: &'static str,
    entity: &Entity<ShellView>,
    on_yes: ConfirmHandler,
    on_no: ConfirmHandler,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let go_ahead = entity.clone();
    let leave_it = entity.clone();
    let block = format!("{selector_prefix}-confirm");
    let yes_sel = format!("{selector_prefix}-confirm-yes");
    let no_sel = format!("{selector_prefix}-confirm-no");
    let yes_id = SharedString::from(yes_sel.clone());
    let no_id = SharedString::from(no_sel.clone());
    h_flex()
        .w_full()
        .gap_3()
        .items_center()
        .debug_selector(move || block.clone())
        .child(div().text_sm().text_color(theme.warning).child(prompt))
        .child(
            div().debug_selector(move || yes_sel.clone()).child(
                Button::new(yes_id)
                    .small()
                    .danger()
                    .label(yes_label)
                    .on_click(move |_event, window, cx| {
                        let on_yes = on_yes.clone();
                        go_ahead.update(cx, |shell, cx| {
                            on_yes(shell, window, cx);
                            sync_dialog_text(shell, window, cx);
                            cx.notify();
                        });
                    }),
            ),
        )
        .child(div().debug_selector(move || no_sel.clone()).child(
            Button::new(no_id).small().ghost().label("Cancel").on_click(
                move |_event, window, cx| {
                    let on_no = on_no.clone();
                    leave_it.update(cx, |shell, cx| {
                        on_no(shell, window, cx);
                        sync_dialog_text(shell, window, cx);
                        cx.notify();
                    });
                },
            ),
        ))
        .into_any_element()
}

/// What a value chip's click runs: `forward` is `!shift`.
pub type StepHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// A steppable row's value, painted as a chip that is the mouse form of
/// `space`/`shift+space` (spec §20.3): click steps forward, shift+click
/// steps back. `on_step: None` paints the plain value with no fill and
/// no handler — the four cases where the keys are inert too (a read-only
/// domain, a one-option `Choice`, an armed confirm, an open text field).
/// `stop_propagation` so the row's own select does not also run; the
/// handler itself ends in [`sync_dialog_text`] at the caller, since it
/// mutates the dialog off the key path (§17.1 rule 3).
///
/// A steppable chip is a control and takes `states`
/// (`control::PointerStates`: hover and pressed fills); its `selector`
/// doubles as its element id, which the pressed state needs. The inert
/// form takes neither — a hover fill promises a click.
pub(crate) fn value_chip(
    text: String,
    selector: String,
    fg: Hsla,
    bg: Hsla,
    radius: Pixels,
    states: ControlPaint,
    on_step: Option<StepHandler>,
) -> AnyElement {
    let selector: SharedString = selector.into();
    let base = div()
        .font_family(crate::fonts::MONO)
        .text_sm()
        .flex_shrink_0();
    match on_step {
        None => base
            .debug_selector(move || selector.to_string())
            .text_color(fg)
            .child(text)
            .into_any_element(),
        Some(on_step) => base
            .id(selector.clone())
            .debug_selector(move || selector.to_string())
            .px_1p5()
            .py_0p5()
            .rounded(radius)
            .bg(bg)
            .text_color(fg)
            .pointer_states(states)
            .child(text)
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                cx.stop_propagation();
                on_step(!event.modifiers.shift, window, cx);
            })
            .into_any_element(),
    }
}

#[cfg(test)]
mod confirm_tests {
    use super::ConfirmAnswer;
    use crate::keymap::{Keystroke, Modifiers};

    fn ks(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke {
            mods,
            key: key.to_string(),
        }
    }
    const SHIFT: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: true,
        cmd: false,
    };

    /// Spec §20.1: one router for every destructive question. `y`/`enter`
    /// bare say yes, `n` bare and `escape` with ANY modifiers say no, and
    /// everything else is `None` — claimed and dropped by the caller.
    #[test]
    fn the_confirm_router_answers_four_keys_and_drops_the_rest() {
        assert_eq!(
            ConfirmAnswer::from_key(&ks("y", Modifiers::NONE)),
            Some(ConfirmAnswer::Yes)
        );
        assert_eq!(
            ConfirmAnswer::from_key(&ks("enter", Modifiers::NONE)),
            Some(ConfirmAnswer::Yes)
        );
        assert_eq!(
            ConfirmAnswer::from_key(&ks("n", Modifiers::NONE)),
            Some(ConfirmAnswer::No)
        );
        assert_eq!(
            ConfirmAnswer::from_key(&ks("escape", Modifiers::NONE)),
            Some(ConfirmAnswer::No)
        );
        assert_eq!(
            ConfirmAnswer::from_key(&ks("escape", SHIFT)),
            Some(ConfirmAnswer::No)
        );
        assert_eq!(ConfirmAnswer::from_key(&ks("y", SHIFT)), None, "Y is not y");
        assert_eq!(ConfirmAnswer::from_key(&ks("enter", Modifiers::CTRL)), None);
        assert_eq!(ConfirmAnswer::from_key(&ks("d", Modifiers::NONE)), None);
    }
}
