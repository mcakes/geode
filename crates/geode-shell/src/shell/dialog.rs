//! Shared shell modal lifecycle, focus synchronization, and rendering helpers.
//!
//! `ShellView` owns a stack of [`ShellModal`]s and renders the top one's chrome without
//! animation. Open through [`open_shell_dialog`] or [`open_shell_dialog_with_key`] so
//! pending key sequences, competing overlays, the retained input, and focus are
//! reconciled. Dialog state must be installed before opening; `sync_dialog_text` reads
//! that state to choose the shared input's text and focus.
//!
//! Content and key handlers receive the shell's existing borrow. They must not
//! synchronously read or update its entity again. Pointer callbacks may capture the
//! entity for access when their event runs after rendering.
//!
//! The shell's modal key branch offers keys to the dialog before its Escape-close
//! fallback and blocks shell chord matching while the modal is open. Component popovers
//! can still use gpui-component's separate overlay machinery.

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

/// Content builder called from [`ShellView::render`] with its existing shell borrow.
/// See [`ShellModal::build`] for the entity-access constraint.
type ModalBuilder = Rc<dyn Fn(&ShellView, &mut Window, &mut App) -> AnyElement>;

/// A modal's optional handler for normalized shell keystrokes. Runs before the shell's
/// Escape-close fallback; `true` consumes the key, including Escape. `false` permits
/// the modal fallback and text-input handling, but never resumes shell chord matching
/// while the modal is open.
///
/// The caller already holds `&mut ShellView`. Use that borrow: synchronously updating
/// its entity here would be reentrant access.
pub type ModalKeyHandler =
    Rc<dyn Fn(&mut ShellView, &Keystroke, &mut Window, &mut Context<ShellView>) -> bool>;

/// Which dialog a stack entry is. Each kind but `Plain` owns one `ShellView`
/// state field, so a kind appears at most once in the stack (see [`can_open`]);
/// a second instance would overwrite the live one's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Settings,
    Keybindings,
    /// The dimension picker (`picker.rs`).
    Picker,
    AsOf,
    ScopeExpr,
    /// Every `choicedialog` target (tile kinds, grouping, log level): they share
    /// the one `choice_dialog` field.
    Choice,
    /// Every object-dialog domain: they share the one `object_dialog` field.
    Object,
    /// A modal with no state field of its own.
    Plain,
}

impl DialogKind {
    /// The status notice for a request refused because this kind is already
    /// open lower in the stack.
    pub(crate) fn already_open_notice(self) -> &'static str {
        match self {
            DialogKind::Settings => "settings is already open underneath",
            DialogKind::Keybindings => "keybindings is already open underneath",
            DialogKind::Picker => "the picker is already open underneath",
            DialogKind::AsOf => "as-of is already open underneath",
            DialogKind::ScopeExpr => "the expression dialog is already open underneath",
            DialogKind::Choice => "a choice list is already open underneath",
            DialogKind::Object => "a configuration dialog is already open underneath",
            DialogKind::Plain => "a dialog is already open underneath",
        }
    }

    /// Every kind, for [`is_already_open_notice`].
    const ALL: [DialogKind; 8] = [
        DialogKind::Settings,
        DialogKind::Keybindings,
        DialogKind::Picker,
        DialogKind::AsOf,
        DialogKind::ScopeExpr,
        DialogKind::Choice,
        DialogKind::Object,
        DialogKind::Plain,
    ];
}

/// Whether `notice` is one of [`DialogKind::already_open_notice`]'s strings.
/// `close_modal` uses this to drop a refusal notice once the stack it named
/// is empty: the kind it referred to no longer exists, so the notice would
/// otherwise sit in the status bar describing a dialog nothing points to.
pub(crate) fn is_already_open_notice(notice: &str) -> bool {
    DialogKind::ALL
        .iter()
        .any(|kind| kind.already_open_notice() == notice)
}

/// Whether a dialog of `kind` may be pushed now. Openers call this before
/// installing their state, because a second instance of a kind would overwrite
/// the live one's state field. A request for the kind already on top does
/// nothing; one for a kind lower in the stack says so in the status bar.
pub(crate) fn can_open(view: &mut ShellView, kind: DialogKind) -> bool {
    let Some(at) = view.modals.iter().position(|m| m.kind == kind) else {
        return true;
    };
    if at + 1 < view.modals.len() {
        view.notice = Some(kind.already_open_notice());
    }
    false
}

/// Whether dispatching `action` opens a shell dialog. With a dialog open, an
/// unclaimed chord reaches the shell only for these actions and the palette
/// toggle, so a stray chord cannot change tiles hidden behind the modal. Mirrors
/// the dialog-opening arms of `ShellView::dispatch`;
/// `opens_dialog_matches_what_dispatch_pushes` holds the two together.
pub(crate) fn opens_dialog(action: &crate::actions::ActionId) -> bool {
    matches!(
        action.0.as_str(),
        "settings::open"
            | "keybindings::open"
            | "config::views"
            | "config::groupings"
            | "config::scopes"
            | "config::schema"
            | "config::sources"
            | "config::colors"
            | "config::expressions"
            | "frame::pick"
            | "scope::save_current"
            | "frame::as_of"
            | "frame::scope_expression"
            | "frame::add_expression"
            | "frame::grouping"
            | "tile::add"
            | "tile::open_with"
            | "log::level"
    ) || action.0.starts_with("frame::pick_")
}

/// The shared input's text and caret as the entry beneath a push left them.
/// The input is one entity reused at every depth, and some dialogs (the
/// expression dialog) keep their value only in it.
pub struct SavedInput {
    pub text: String,
    pub cursor: usize,
}

/// One open modal, owned and rendered by `ShellView`. Its `Rc` closures can be cloned
/// out of `self.modals` before invocation, releasing that field's borrow.
pub struct ShellModal {
    /// Which state field this entry owns; see [`DialogKind`].
    pub kind: DialogKind,
    pub title: SharedString,
    /// Build fresh content during [`ShellView::render`] using its existing borrow.
    /// Synchronous entity reads or updates would reenter the shell while it is being
    /// rendered. Capture an entity only for callbacks that run later.
    pub build: ModalBuilder,
    /// Optional content between the title and close button, such as a count or mode
    /// pill. Runs during shell rendering with the same borrow constraint as `build`.
    pub title_extra: Option<TitleExtraBuilder>,
    /// Optional key handler offered each key before the shell's modal fallback. See
    /// [`ModalKeyHandler`].
    pub on_key: Option<ModalKeyHandler>,
    /// Optional pointer route for the dialog's one-screen back step. See
    /// [`set_back`].
    pub back: Option<ModalBack>,
    /// Set while another entry covers this one; restored by [`refocus_top`].
    pub saved_input: Option<SavedInput>,
}

/// A multi-screen dialog's back step for the title row's Back button. `available`
/// reads the dialog's current state during rendering, with the same borrow contract as
/// [`ShellModal::build`]; the button paints only while it returns `true`. `step`
/// performs exactly the transition Escape's final back rung performs, discarding
/// whatever earlier Escape rungs would discard, so one click leaves one whole screen.
/// It must refuse by doing nothing when the state has no back step, because a click
/// can arrive after the state changed under the painted button.
#[derive(Clone)]
pub struct ModalBack {
    available: Rc<dyn Fn(&ShellView) -> bool>,
    step: ModalBackStep,
}

/// The transition half of [`ModalBack`]. It receives the shell's own borrow from the
/// click listener, so it must not update the shell entity again.
type ModalBackStep = Rc<dyn Fn(&mut ShellView, &mut Window, &mut Context<ShellView>)>;

/// Title-row builder with the same render-time borrow contract as `ModalBuilder`.
pub type TitleExtraBuilder = Rc<dyn Fn(&ShellView, &mut App) -> AnyElement>;

/// Attach title-row content after opening a modal. Does nothing if no modal is open.
pub fn set_title_extra(
    view: &mut ShellView,
    build: impl Fn(&ShellView, &mut App) -> AnyElement + 'static,
) {
    if let Some(modal) = view.modals.last_mut() {
        modal.title_extra = Some(Rc::new(build));
    }
}

/// Register the open modal's back step after opening it. Does nothing if no modal is
/// open. See [`ModalBack`] for the contract of both closures.
pub fn set_back(
    view: &mut ShellView,
    available: impl Fn(&ShellView) -> bool + 'static,
    step: impl Fn(&mut ShellView, &mut Window, &mut Context<ShellView>) + 'static,
) {
    if let Some(modal) = view.modals.last_mut() {
        modal.back = Some(ModalBack {
            available: Rc::new(available),
            step: Rc::new(step),
        });
    }
}

/// Whether the open modal currently offers a back step. Read during rendering to decide
/// whether the title row paints its Back button.
pub(crate) fn back_available(view: &ShellView) -> bool {
    view.modals
        .last()
        .and_then(|modal| modal.back.as_ref())
        .is_some_and(|back| (back.available)(view))
}

/// The Back button's click: run the registered step when one is currently available,
/// then synchronize the shared input's text and focus from the resulting state, as
/// every pointer transition must.
pub(crate) fn step_back(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    // Clone out of `view.modals` so the step can take the whole view.
    let Some(back) = view.modals.last().and_then(|modal| modal.back.clone()) else {
        return;
    };
    if !(back.available)(view) {
        return;
    }
    (back.step)(view, window, cx);
    sync_dialog_text(view, window, cx);
    cx.notify();
}

/// Suppress component bindings that compete with shell raw-key handling. Register after
/// `gpui_component::init`: deeper contexts win first, then later bindings at the same
/// depth. `NoAction` suppresses competing actions before raw listeners run; an action
/// that merely propagates would allow the next competing action to consume the key.
///
/// Tab/Shift-Tab are reclaimed throughout `GeodeShell`, preventing `Root` focus cycling
/// from consuming tile and overlay navigation. The narrower modal, command-line, and
/// palette bindings also state each surface's dependency. `GeodeModal` belongs to the
/// panel and is absent from the focused path when Normal mode parks focus on the shell.
/// `GeodeModalOpen` belongs to the shell root while a modal is open, so it covers that
/// focus state too.
///
/// Ctrl-F is reclaimed in every `Input`, including the palette, for list page
/// navigation. This suppresses the component's non-macOS Search binding even if the
/// input is searchable.
///
/// Ctrl-A uses `GeodeModal > Input` to match at the focused Input's depth; a bare
/// ancestor `GeodeModal` predicate would lose to SelectAll/MoveHome. This removes
/// native Ctrl-A behavior from every Input inside a modal, allowing picker selection
/// commands. Keeping the panel and root contexts distinct prevents this reclaim from
/// reaching inputs behind the modal.
///
/// Shift-Up/Shift-Down are reclaimed in `Input` because single-line selection actions
/// can propagate to an enclosing DataTable and move its row selection. This also
/// suppresses those native selection actions in any multiline input.
pub fn init_reclaimed_keybindings(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeModal")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodeModal")),
        gpui::KeyBinding::new("ctrl-f", gpui::NoAction, Some("Input")),
        gpui::KeyBinding::new("ctrl-a", gpui::NoAction, Some("GeodeModal > Input")),
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeCommandLine")),
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeModalOpen")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodeModalOpen")),
        // The palette has its own context because it is not a modal panel.
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodePalette")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodePalette")),
        // Prevent single-line Input selection actions from reaching an enclosing
        // DataTable before the shell can route the raw key.
        gpui::KeyBinding::new("shift-up", gpui::NoAction, Some("Input")),
        gpui::KeyBinding::new("shift-down", gpui::NoAction, Some("Input")),
        // Cover focused tiles as well as overlays. The shell root's context is deeper
        // than Root's focus-cycling context.
        gpui::KeyBinding::new("tab", gpui::NoAction, Some("GeodeShell")),
        gpui::KeyBinding::new("shift-tab", gpui::NoAction, Some("GeodeShell")),
    ]);
}

/// Open a modal without a custom key handler. Shares all lifecycle and focus handling
/// with [`open_shell_dialog_with_key`]; `build` runs each rendered frame.
pub fn open_shell_dialog<F>(
    view: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
    kind: DialogKind,
    title: impl Into<SharedString>,
    build: F,
) where
    F: Fn(&ShellView, &mut Window, &mut App) -> AnyElement + 'static,
{
    open_shell_dialog_with_key(view, window, cx, kind, title, build, None, false);
}

/// Install a modal after cancelling pending key sequences and competing overlays. The
/// retained shared input is cleared on every open because it outlives dialogs.
///
/// `focus_filter` focuses that input for a surface without a mode. Dialogs with mode
/// state install it before calling this function and pass `false`, allowing
/// `sync_dialog_text` to choose focus from their state. Callers guard against
/// replacing an already-open modal.
#[allow(clippy::too_many_arguments)]
// One door for every dialog; bundling these into a struct would only rename them.
pub fn open_shell_dialog_with_key<F>(
    view: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
    kind: DialogKind,
    title: impl Into<SharedString>,
    build: F,
    on_key: Option<ModalKeyHandler>,
    focus_filter: bool,
) where
    F: Fn(&ShellView, &mut Window, &mut App) -> AnyElement + 'static,
{
    // Backstop for an opener that skipped its own `can_open` check. By then that
    // opener may already have overwritten the live state, which is why each
    // opener checks first.
    if !can_open(view, kind) {
        return;
    }
    // Do not let a pending shell key sequence survive into or across the modal.
    view.matcher.cancel();
    // Closing the palette releases its exclusive key route and restores focus before
    // the modal records where to return it.
    view.close_palette(window, cx);
    // Cancel the command line before the modal takes its key route; otherwise the line
    // would remain visible but unable to receive its own controls.
    view.cancel_command_line(window, cx);
    // The scope bar's add-a-filter menu is transient chrome under a modal's
    // key route; it never survives one opening.
    view.add_filter_menu = None;

    // Recorded after the palette close above (which may itself have just
    // returned focus to the field) and before the dialog takes focus, for
    // `close_modal` (see `ShellView::overlay_return_to_filter`). Only the
    // stack's base records it: a nested push must not overwrite it with "the
    // dialog beneath had focus".
    if view.modals.is_empty() {
        view.overlay_return_to_filter = view.filter_field_focused(window, cx);
    }
    // The covered entry keeps the shared input's text and caret; the push below
    // clears the input for the new dialog.
    let (text, cursor) = {
        let input = view.dialog_input.read(cx);
        (input.value().to_string(), input.cursor())
    };
    if let Some(covered) = view.modals.last_mut() {
        covered.saved_input = Some(SavedInput { text, cursor });
    }

    view.modals.push(ShellModal {
        kind,
        title: title.into(),
        title_extra: None,
        build: Rc::new(build),
        on_key,
        back: None,
        saved_input: None,
    });

    // Reuse the retained input, clearing text left by the previous dialog even if this
    // one initially leaves it blurred. `set_value` emits no Change event.
    view.dialog_input
        .update(cx, |input, cx| input.set_value("", window, cx));
    if focus_filter {
        let handle = view.dialog_input.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }
    // Prevent the same opening mouse-down from bubbling to a tracked ancestor and
    // taking focus back from the modal's input.
    window.prevent_default();
    // Dialog state is installed before this call so synchronization can choose its
    // initial text and focus.
    sync_dialog_text(view, window, cx);

    cx.notify();
}

/// Mirror the active dialog's effective query and focus into the shared Input. The
/// as-of dialog always owns the input. Settings, keybindings, and object dialogs use
/// [`crate::dialogmode::focus_target`], with capture taking priority over list mode.
/// Several states can be `Some` while stacked; the top kind decides. Filter-only
/// dialogs keep their own focus path (see [`refocus_top`]).
///
/// Call after opening and at the end of keyboard or pointer transitions that change
/// dialog state. Pointer handlers must call it themselves because they do not pass
/// through the shell's modal key branch.
///
/// Compare the borrowed input rope before `set_value` to avoid resetting the caret or
/// copying the text on every keystroke. Programmatic writes emit no Change event;
/// dialog state remains the source of truth. Focusing an already focused handle is
/// idempotent.
pub(crate) fn sync_dialog_text(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    // The live dialog owns the shared input. Several kinds' states can be `Some`
    // at once while stacked, so the owner is the top kind, never the first
    // non-empty field.
    let (mode, listening, query) = match shell.top_kind() {
        Some(DialogKind::AsOf) => {
            // No list mode or capture state: its query always owns the focused input.
            let Some(state) = shell.as_of_dialog.as_ref() else {
                return;
            };
            let query = state.query();
            let input = shell.dialog_input.clone();
            if input.read(cx).text() != query {
                input.update(cx, |i, cx| i.set_value(query, window, cx));
            }
            input.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        Some(DialogKind::Keybindings) => {
            let Some(state) = shell.keybindings.as_ref() else {
                return;
            };
            (state.mode, state.listening.is_some(), state.query.as_str())
        }
        Some(DialogKind::Object) => {
            let Some(state) = shell.object_dialog.as_ref() else {
                return;
            };
            (state.mode, false, state.effective_query())
        }
        Some(DialogKind::Settings) => {
            let Some(state) = shell.settings.as_ref() else {
                return;
            };
            (state.mode, false, state.effective_query())
        }
        // Filter-only dialogs keep their own focus path (see `refocus_top`).
        _ => return,
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

/// Give the top dialog back the shared input and focus once whatever covered
/// it (a popped dialog, the palette) is gone. Restores the text and caret the
/// entry had when it was covered, then chooses focus: mode dialogs through
/// [`sync_dialog_text`], filter-only dialogs by focusing the input they always
/// type into. `set_value` emits no `Change`, and the restored text is what the
/// entry's state already holds.
pub(crate) fn refocus_top(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(top) = view.modals.last_mut() else {
        return;
    };
    let kind = top.kind;
    if let Some(saved) = top.saved_input.take() {
        let input = view.dialog_input.clone();
        input.update(cx, |i, cx| {
            i.set_value(saved.text, window, cx);
            i.set_selected_range(saved.cursor..saved.cursor, cx);
        });
    }
    match kind {
        DialogKind::Picker | DialogKind::Choice | DialogKind::ScopeExpr => {
            let handle = view.dialog_input.read(cx).focus_handle(cx);
            handle.focus(window, cx);
        }
        DialogKind::Plain => {}
        DialogKind::Settings | DialogKind::Keybindings | DialogKind::Object | DialogKind::AsOf => {
            sync_dialog_text(view, window, cx);
        }
    }
}

/// Shared double shadow for modal and palette panels. Both layers use neutral black at
/// 10% opacity in either theme; the values are fixed, with no animation.
pub(crate) fn overlay_panel_shadow() -> Vec<gpui::BoxShadow> {
    vec![
        box_shadow(px(0.), px(20.), px(25.), px(-5.), hsla(0., 0., 0., 0.1)),
        box_shadow(px(0.), px(8.), px(10.), px(-6.), hsla(0., 0., 0., 0.1)),
    ]
}

/// Static query display used when the shared Input does not own typing. `slash_filters`
/// distinguishes Normal mode from keybinding capture: during capture, `/` records a key
/// instead of entering the filter, so the empty-query hint must be hidden. Mouse entry
/// can still cancel capture and start filtering.
pub struct FrozenFilter<'a> {
    /// The query to echo as static text. Empty is the state every
    /// normal-mode dialog opens in.
    pub query: &'a str,
    /// Whether a bare `/` would enter filter mode from here. `false`
    /// only while the keybinding dialog is capturing a keystroke.
    pub slash_filters: bool,
    /// Shell handle for the frozen row's mouse-down to call `enter_filter_by_mouse`.
    pub entity: Entity<ShellView>,
}

/// Enter filter mode from the frozen row's mouse-down. The shared entry helper
/// snapshots the query exactly as `/` does; `sync_dialog_text` reconciles focus
/// after the handler returns.
///
/// Cancel a keybinding capture first so its focus priority cannot keep keys on the
/// shell root while the dialog displays filter mode. An armed confirmation blocks
/// this transition until answered.
pub(crate) fn enter_filter_by_mouse(shell: &mut ShellView) {
    match shell.top_kind() {
        Some(DialogKind::Keybindings) => {
            let Some(state) = shell.keybindings.as_mut() else {
                return;
            };
            // Keep the confirmation's exclusive input route until it is answered.
            if state.confirm.is_some() {
                return;
            }
            state.listening = None;
            dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
        }
        Some(DialogKind::Object) => {
            let Some(state) = shell.object_dialog.as_mut() else {
                return;
            };
            // `build_edit` still paints the frozen row during confirmation, so guard
            // the transition here as well as in keyboard routing.
            if state.confirm.is_some() {
                return;
            }
            state.enter_filter();
        }
        Some(DialogKind::Settings) => {
            let Some(state) = shell.settings.as_mut() else {
                return;
            };
            dialogmode::enter_filter(&mut state.mode, &mut state.filter_entry_query, &state.query);
        }
        _ => {}
    }
}

/// Shared filter row with a search icon and borderless Input. `frozen` replaces the
/// Input with static query text when typing belongs elsewhere, preserving prefix
/// spacing across focus changes. An empty frozen query shows the filter hint only when
/// [`FrozenFilter::slash_filters`] says `/` enters filtering.
pub fn filter_row(
    input: &Entity<InputState>,
    frozen: Option<FrozenFilter<'_>>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let row = div().w_full().border_b_1().border_color(theme.border);
    let search_icon = || Icon::new(IconName::Search).text_color(theme.muted_foreground);
    match frozen {
        // Pointer entry uses the same query snapshot as `/`, then synchronizes text and
        // focus. The text cursor marks the frozen row as editable.
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
            // Only show the filter hint when the query is empty and `/` enters
            // filtering. Place its selector on the text so tests verify that the hint
            // itself paints.
            let body = if frozen.query.is_empty() && frozen.slash_filters {
                div()
                    .debug_selector(|| "dialog-filter-placeholder".to_string())
                    .child("press / to filter")
                    .into_any_element()
            } else {
                // Echo the query, including an empty query while capture owns `/`.
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

/// Labelled shared Input for object naming or value entry. This renderer neither clears
/// nor focuses it: callers update the dialog's effective query and run
/// `sync_dialog_text`. Input changes flow back to the active dialog state.
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

/// Current list mode in the title row. Normal uses muted colors; Filter uses primary
/// colors to indicate text entry. Lowercase labels also form the debug selector, so
/// tests can distinguish the painted state.
pub(crate) fn mode_pill(mode: DialogMode, cx: &App) -> AnyElement {
    match mode {
        DialogMode::Normal => state_pill("normal", false, cx),
        DialogMode::Filter => state_pill("filter", true, cx),
    }
}

/// Object chain-field pill. Uses typing colors while naming the value-entry state
/// separately from list filtering.
pub(crate) fn chain_pill(cx: &App) -> AnyElement {
    state_pill("chain", true, cx)
}

/// Plain value-entry pill with typing colors and selector `dialog-mode-pill-edit`.
pub(crate) fn edit_pill(cx: &App) -> AnyElement {
    state_pill("edit", true, cx)
}

/// Choice-entry pill with typing colors and selector `dialog-mode-pill-choose`.
pub(crate) fn choose_pill(cx: &App) -> AnyElement {
    state_pill("choose", true, cx)
}

/// Render every ranked choice option in a scrolling viewport capped at
/// [`CHOICE_VISIBLE_ROWS`]. Highlight and click indices are ranked positions;
/// `on_click(row)` delegates completion behavior to the caller. Match characters use
/// [`super::keybindings_view::highlighted_text`]. Selectors are `{prefix}-choice-list`
/// and `{prefix}-choice-{text}`.
pub(crate) fn choice_rows(
    list: &crate::choice::ChoiceList,
    prefix: &'static str,
    scroll: &gpui::ScrollHandle,
    theme: &Theme,
    on_click: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    // Keep all ranked rows in the scroll container. Wheel scrolling and keyboard
    // scroll-follow share the viewport; highlight and clicks use ranked indices.
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
        let row = h_flex()
            .id(("choice-row", ranked.row))
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
        let row = super::listrow::paint_row(row, paint, position == list.ranked_highlighted());
        rows = rows.child(row);
    }
    rows.into_any_element()
}

/// Choice row height at the design rem, and the viewport's visible-row cap.
const CHOICE_ROW_HEIGHT: f32 = 28.0;
const CHOICE_VISIBLE_ROWS: usize = crate::choice::DEFAULT_CAP;

/// Shared state pill. `typing` selects primary colors instead of muted resting colors.
/// The label is part of the selector so tests can verify the state.
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

/// Outlined monospace classification badge, such as a layer or field destination. `fg`
/// and `border` color its text and outline; the theme supplies its radius. No
/// background fill is painted.
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

/// Swatch for an already-resolved data colour. The caller resolves the definition
/// against anchors, tokens, and the active theme; this helper paints that value with a
/// theme border, keeping data colour separate from chrome.
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

/// Maximum modal height as a fraction of the viewport. Smaller panels size to content;
/// taller panels scroll their content within this cap.
pub(crate) const MODAL_MAX_HEIGHT_RATIO: f32 = 0.8;

/// Shared top margin as a fraction of viewport height. Combined with the 0.8 height
/// cap, a full-height modal has equal 10% margins above and below.
pub(crate) const MODAL_TOP_RATIO: f32 = 0.1;

/// Render an instant modal with a full-window backdrop, a horizontally centered panel
/// anchored at [`MODAL_TOP_RATIO`], and title extras beside the close button. Viewport
/// dimensions come from the caller in the same coordinates as the shell.
///
/// The panel has a maximum height, not a fixed height. Its `flex_auto` content region
/// keeps its natural basis, then shrinks and scrolls under the cap. A zero-basis
/// `flex_1` would collapse content when the parent has no definite height to grow into.
///
/// Backdrop mouse-down closes the modal; panel mouse-down stops propagation so clicking
/// its content cannot also close it. Escape and other modal key routing belong to the
/// shell's key handler. `show_back` paints the Back button left of the title; the
/// caller reads it from [`back_available`] each frame.
pub(crate) fn render_modal(
    title: SharedString,
    title_extra: Option<AnyElement>,
    show_back: bool,
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
            h_flex()
                .gap_1()
                .items_center()
                .when(show_back, |row| row.child(back_button(cx)))
                .child(
                    div()
                        .text_lg()
                        .child(title)
                        .debug_selector(|| "shell-modal-title".to_string()),
                ),
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
        // Panel-local context for reclaimed bindings, including `GeodeModal > Input`.
        // Keep it distinct from the root's modal-open context to preserve that scope.
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
        // Anchor all modal top edges at the same viewport position.
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

/// The title row's Back button: the pointer route for the registered back step. Its
/// tooltip names Escape, the key whose final back rung the click performs. The click
/// stops propagation so no surface beneath the button also handles it.
fn back_button(cx: &mut Context<ShellView>) -> AnyElement {
    div()
        .id("shell-modal-back-site")
        .debug_selector(|| "shell-modal-back".to_string())
        .tooltip(crate::tips::tip_key(
            "tip-shell-modal-back",
            "Back",
            "escape",
        ))
        .child(
            Button::new("shell-modal-back")
                .small()
                .ghost()
                .icon(IconName::ChevronLeft)
                .on_click(cx.listener(|view, _event, window, cx| {
                    cx.stop_propagation();
                    step_back(view, window, cx);
                })),
        )
        .into_any_element()
}

/// Render [`crate::footer`]'s categorized hint rows in stable order, preserving empty
/// rows' height. Callers supply active hints tagged with their categories. Hints use
/// keystroke chips joined by separators. A selector gives each chip `<selector>-<key>`
/// and the first chip the bare `<selector>`, allowing tests to inspect both groups and
/// individual keys.
pub(crate) fn hint_rows(hints: &[Hint]) -> AnyElement {
    let chip = |spec: &str| {
        let ks = crate::keymap::parse_keystroke(spec, Modifiers::NONE)
            .expect("footer hint keystrokes are hardcoded valid");
        super::kbd::chip(&ks).into_any_element()
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
        // alone is a `text_xs` line, shorter than a hint, so an unpainted
        // hint (a chip and its word, whichever is taller) sets the height —
        // `invisible` lays out and paints nothing.
        if members.is_empty() {
            line = line.child(
                h_flex()
                    .invisible()
                    .gap_1()
                    .items_center()
                    .child(chip("space"))
                    .child(div().child("space")),
            );
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

/// Shared answer to an armed destructive confirmation. An unrecognized key yields
/// `None`; callers consume it so it cannot act on the underlying object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAnswer {
    Yes,
    No,
}

impl ConfirmAnswer {
    /// Bare `y`/Enter confirm; bare `n` or Escape with any modifiers cancel.
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

/// Confirmation question and action buttons replacing a dialog's action bar. Both
/// pointer answers synchronize dialog text and notify after the callback, including
/// cancellation callbacks that only clear the armed state. Selectors are
/// `{selector_prefix}-confirm`, `-yes`, and `-no`.
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

/// Value chip whose click steps forward and Shift-click steps backward. `on_step: None`
/// renders an inert value without fill, pointer states, or handler.
///
/// An active chip uses `selector` as its element ID for hover/pressed state and stops
/// propagation before invoking the callback, so the row does not also select. The
/// caller synchronizes dialog text after its state transition.
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

    /// Bare y/Enter confirm, bare n and any Escape cancel; other keys yield None for
    /// the caller to consume.
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
