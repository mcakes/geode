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
use gpui::{
    AnyElement, App, Context, Entity, Focusable as _, MouseButton, SharedString, Window, div, hsla,
    px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, box_shadow, h_flex, v_flex};

use super::ShellView;
use crate::keymap::Keystroke;

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
    /// This modal's optional key-handling seam (Part B) — see
    /// [`ModalKeyHandler`]'s own doc comment. Both shipped dialogs (the
    /// keybinding dialog, and the settings dialog since its row-list
    /// rewrite) now pass a handler; `open_shell_dialog` still sets this to
    /// `None` unconditionally for any future modal without key needs.
    pub on_key: Option<ModalKeyHandler>,
}

/// Reclaim every key gpui-component binds by default that collides with
/// Geode's own navigation vocabulary, via `gpui::NoAction` — `gpui`'s own
/// mechanism for this: a `NoAction` binding suppresses every
/// equal-or-weaker binding it outranks, so no action dispatches at all and
/// the raw `KeyDownEvent` reaches our key listeners instead
/// (`gpui::Keymap::bindings_for_input`, `crates/gpui/src/keymap.rs`, pinned
/// checkout, sorts candidate bindings by context depth — deepest wins —
/// before applying `NoAction` suppression). Two reclaims live here today,
/// scoped differently on purpose:
///
/// 1. **`tab`/`shift-tab`**, scoped to `"GeodeModal"`. `Root` binds bare
///    `tab`/`shift-tab` (unconditionally, no `cx.propagate()`) to its own
///    focus-cycling actions in a `"Root"` key context wrapping the entire
///    window (`crates/ui/src/root.rs`, pinned checkout) — so without a
///    reclaim, those keystrokes are fully consumed there before
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
/// 2. **`ctrl-f`**, scoped to `"Input"` and app-wide (not `"GeodeModal"`).
///    gpui-component binds `ctrl-f` to its editor `Search` action in the
///    `"Input"` key context on non-macOS (`crates/base/src/input/base/
///    state.rs`, pinned rev), and that handler returns without
///    `cx.propagate()` when the input isn't searchable — so `ctrl+f`,
///    which both list dialogs and (per Task 5) the command palette use for
///    "page down" (`crate::listfilter::nav_command`), would work on macOS
///    (where gpui-component doesn't bind it at all) and die silently on
///    Windows and Linux. This one can't be scoped to `"GeodeModal"` the
///    way `tab` is: the command palette is not a [`render_modal`] surface,
///    so a `"GeodeModal"`-scoped binding would never reach it. Going
///    app-wide instead costs nothing, because gpui-component's `Search`
///    action is dead weight in this app — nothing here uses a searchable
///    input — unlike `Root`'s Tab cycling, which is a real affordance this
///    reclaim must not disturb outside modals. (A pass-through action of
///    our own would NOT work here either: `gpui` dispatches every matched
///    binding in sequence — `Window::dispatch_key_event`, pinned checkout
///    — so calling `cx.propagate()` from our own action would simply hand
///    the keystroke on to `Search` right after.)
///
/// 3. **`ctrl-a`**, scoped to `"GeodeModal > Input"` (Phase 4a §3.3, the
///    dimension pickers) — a compound predicate, not the bare
///    `"GeodeModal"` bullet 1 uses, for a reason worth spelling out: it
///    was tried first, and it does not work. `KeyBindingContextPredicate::
///    depth_of` (pinned checkout, `crates/gpui/src/keymap/context.rs`)
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
/// `focus_filter` focuses [`ShellView::dialog_input`] on open — every list
/// dialog passes `true` (the filter-first dialog UX: the first character
/// typed must reach the filter, not fall on the floor);
/// `open_shell_dialog` passes `false`. A modal that passes `true` must
/// actually render that input — [`filter_row`] — since gpui dispatches
/// keys down the *rendered* focus path and would otherwise route them to
/// the window root, past `ShellView`'s own key listener.
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

    view.modal = Some(ShellModal {
        title: title.into(),
        build: Rc::new(build),
        on_key,
    });

    if focus_filter {
        // Reset by value, not by rebuilding the entity — the same
        // lifecycle `toggle_palette` gives `palette_input` (see
        // `ShellView::dialog_input`'s own doc comment). `set_value` does
        // not emit `InputEvent::Change` (checked against the pinned
        // checkout, same as `toggle_palette`'s own comment records), so
        // this reset never reaches the subscription; each dialog's fresh
        // state already starts with an empty query.
        view.dialog_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let handle = view.dialog_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    cx.notify();
}

/// The drop shadow every floating overlay panel in this shell wears —
/// [`render_modal`]'s panel and the command palette's
/// (`palette::render`), so the two read as siblings rather than as two
/// unrelated components. Lifted here from `render_modal`, which used to
/// build it inline, when the palette adopted it.
///
/// It is the exact double shadow gpui-component's own `Dialog` builds in
/// its `with_animation("slide-down", ..)` closure (pinned checkout,
/// `crates/ui/src/dialog/dialog.rs`), evaluated at `delta = 1.0` — that
/// animation's fully-open state, which is the only state Geode's own
/// instant modals and palette ever have. Keeping the values identical is
/// what lets our self-owned chrome sit next to that crate's own popovers
/// without looking like a different design system.
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
/// checkout, `crates/ui/src/command/state.rs:838-846`) — so the icon is
/// left at its default size to match. `prefix` survives
/// `appearance(false)`: that flag guards only the background and border
/// (`crates/ui/src/input/input.rs:578-584`), never the prefix child.
///
/// `frozen` renders a muted, static copy of the query *instead of* the
/// live input: the keybinding dialog passes `Some(query)` while it is
/// listening for a binding, when the input is blurred and a caret would
/// be a lie about where keystrokes are going. It keeps the icon, and
/// hand-matches `Input`'s own medium-size prefix gap (`px(6.)`,
/// `input.rs:504-508`), so entering and leaving capture doesn't shift the
/// query text sideways.
pub fn filter_row(input: &Entity<InputState>, frozen: Option<&str>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let row = div().w_full().border_b_1().border_color(theme.border);
    let search_icon = || Icon::new(IconName::Search).text_color(theme.muted_foreground);
    match frozen {
        Some(query) => row
            .py_1()
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(theme.muted_foreground)
                    .child(search_icon())
                    .child(query.to_string()),
            )
            .into_any_element(),
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
/// overlay uses — `overlay_color`, pinned checkout `crates/ui/src/dialog/
/// dialog.rs`), and over it — horizontally centered, top edge anchored at
/// [`MODAL_TOP_RATIO`] so every dialog starts at the same line — a panel
/// on `cx.theme().popover`/
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
        .child(div().text_lg().child(title))
        .child(
            Button::new("shell-modal-close")
                .small()
                .ghost()
                .icon(IconName::Close)
                .on_click(cx.listener(|view, _event, window, cx| {
                    view.close_modal(window, cx);
                })),
        );

    let panel = v_flex()
        .id("shell-modal-panel")
        // A key context, on a modal that is otherwise "plain chrome, not
        // an action-dispatch layer" (see `handle_key_down`'s modal branch
        // in `shell/mod.rs`) — added not to dispatch anything of our own,
        // but to SUPPRESS one: gpui-component's `Root` binds bare
        // `tab`/`shift-tab` to its own focus-cycling actions in a `"Root"`
        // key context that wraps the entire window
        // (`crates/ui/src/root.rs`, pinned checkout), unconditionally,
        // and those handlers never call `cx.propagate()` — so a plain
        // `tab` keystroke is fully consumed by `Root` before
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
