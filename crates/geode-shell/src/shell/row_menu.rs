//! The row menu: what a tile row's single-valued columns let the user do
//! (spec §3), built by [`crate::dimension::menu_rows`]. The shell owns it
//! so every grid gets the same menu from one `dimension_context`. Opened by
//! `tile::context_menu` at the cursor row, or by a right press on a tile
//! that answers `press_context`, at the pointer; closed by a pick,
//! `escape`, a press outside it, a chord's dispatch, a dialog or the
//! palette opening.

use std::rc::Rc;

use geode_core::context::DimensionContext;
use geode_core::positions::{CommandOutcome, outcome_notice};
use geode_core::query::{AsOf, DistinctParams};
use geode_core::scope::Scope;
use gpui::prelude::*;
use gpui::{Context, Pixels, Point, SharedString, Window};

use super::dialog::{self, ConfirmAnswer, ConfirmHandler, DialogKind};
use super::{ACTION_KEY, ShellEvent, ShellView, choicedialog};
use crate::defaults::AddPlacement;
use crate::dimension::{RowPick, menu_rows};
use crate::menu::{Menu, MenuHost};
use crate::tiling::TileId;

/// Status notice: the row names no value any kind or action takes.
pub const NO_ROW_ACTIONS: &str = "no actions for this row";

/// Status notice: an action's confirm was refused because a plain dialog
/// is already on top.
pub const CONFIRM_REFUSED: &str = "another dialog is already open";

/// The open row menu.
pub struct RowMenu {
    menu: Menu<RowPick>,
    context: DimensionContext,
    /// Window point the menu hangs from; `None` → the focused tile's
    /// top-left.
    at: Option<Point<Pixels>>,
    /// The scope bar's text field held focus when the menu opened: a
    /// dismissal hands it back there.
    return_to_filter: bool,
}

impl RowMenu {
    pub(crate) fn menu(&self) -> &Menu<RowPick> {
        &self.menu
    }

    pub(crate) fn at(&self) -> Option<Point<Pixels>> {
        self.at
    }

    /// Section titles as `# {title}`, separators as `|`, action titles.
    #[cfg(test)]
    pub(crate) fn titles(&self) -> Vec<String> {
        use crate::menu::Row;
        self.menu
            .rows()
            .iter()
            .map(|r| match r {
                Row::Section(t) => format!("# {t}"),
                Row::Separator => "|".into(),
                Row::Action(a) => a.title().to_string(),
            })
            .collect()
    }
}

/// What a [`crate::dimension::DimensionAction`] runs against: the shell,
/// its window and its context, once the row menu has closed.
pub struct ActionCx<'a, 'b> {
    shell: &'a mut ShellView,
    window: &'a mut Window,
    cx: &'a mut Context<'b, ShellView>,
    /// The roster index of the action running.
    action: usize,
}

/// What a yes to [`ActionCx::confirm`] runs.
pub type OnYes = Rc<dyn Fn(&mut ActionCx<'_, '_>)>;

impl ActionCx<'_, '_> {
    /// A choice dialog titled `title` over `column`'s distinct live values
    /// (unscoped), minus `exclude`; loading until they arrive. A pick calls
    /// the running action's `chosen` with the picked value. No
    /// values left closes it with the notice `empty`; a failed fetch closes
    /// it with `could not load {column} values: {reason}`. Asks for nothing
    /// when a choice list is already open.
    pub fn choose_value(
        &mut self,
        context: DimensionContext,
        column: &str,
        title: SharedString,
        exclude: Option<String>,
        empty: &'static str,
    ) {
        self.shell.next_picker_tag += 1;
        let tag = self.shell.next_picker_tag;
        let opened = choicedialog::open_action_values(
            self.shell,
            self.action,
            context,
            column.to_string(),
            title,
            exclude,
            empty,
            tag,
            self.window,
            self.cx,
        );
        if opened {
            self.cx.emit(ShellEvent::DistinctRequested(DistinctParams {
                key: ACTION_KEY,
                tag,
                column: column.to_string(),
                scope: Scope::default(),
                as_of: AsOf::Live,
            }));
        }
    }

    /// A y/n dialog asking `question`; yes (`y`, `enter`, the button) runs
    /// `on_yes` with a fresh ActionCx after the dialog closes; no (`n`,
    /// `escape` with or without modifiers, Cancel) closes it. Any other bare
    /// key is consumed; a chord or a shift-modified key passes on. Refused,
    /// with the notice [`CONFIRM_REFUSED`], while a plain dialog (another
    /// confirm among them) is already on top: the stack holds one plain
    /// dialog at a time, and a refusal must not read as a silent no.
    pub fn confirm(&mut self, question: SharedString, on_yes: OnYes) {
        if self.shell.top_kind() == Some(DialogKind::Plain) {
            self.notice(CONFIRM_REFUSED);
            return;
        }
        let action = self.action;
        let yes: ConfirmHandler = Rc::new(move |shell, window, cx| {
            shell.close_modal(window, cx);
            let mut acx = ActionCx {
                shell,
                window,
                cx,
                action,
            };
            on_yes(&mut acx);
        });
        let no: ConfirmHandler = Rc::new(|shell, window, cx| shell.close_modal(window, cx));
        let entity = self.cx.entity();
        let (build_yes, build_no) = (yes.clone(), no.clone());
        let build = move |_: &ShellView, _: &mut Window, cx: &mut gpui::App| {
            let selector = format!("action-question-{question}");
            gpui::div()
                .debug_selector(move || selector.clone())
                .child(dialog::confirm_row(
                    question.to_string(),
                    "Yes",
                    "action",
                    &entity,
                    build_yes.clone(),
                    build_no.clone(),
                    cx,
                ))
                .into_any_element()
        };
        let on_key: dialog::ModalKeyHandler = Rc::new(move |shell, ks, window, cx| {
            match ConfirmAnswer::from_key(ks) {
                Some(ConfirmAnswer::Yes) => yes(shell, window, cx),
                Some(ConfirmAnswer::No) => no(shell, window, cx),
                None => {}
            }
            // Chords pass on (a dialog chord may push over the confirm),
            // and so does a shift-modified key, whose mods are not NONE;
            // every bare key, and escape with any modifiers, is the
            // confirm's.
            ks.mods == crate::keymap::Modifiers::NONE || ConfirmAnswer::from_key(ks).is_some()
        });
        dialog::open_shell_dialog_with_key(
            self.shell,
            self.window,
            self.cx,
            DialogKind::Plain,
            "Confirm",
            build,
            Some(on_key),
            false,
        );
    }

    /// Split a new tile of `kind` beside the focused one, restored from
    /// `state` (as `g m` does).
    pub fn open_tile(&mut self, kind: &str, state: Option<toml::Table>) {
        self.shell
            .add_tile(kind, AddPlacement::Split(None), state, self.window, self.cx);
    }

    /// Show `text` as the shell's status notice.
    pub fn notice(&mut self, text: impl Into<SharedString>) {
        self.shell.notice = Some(text.into());
        self.cx.notify();
    }

    /// Hand `url` to the OS (or to a test's [`crate::dimension::UrlOpener`]).
    /// Nothing reports whether a handler took it; say only what Geode did.
    pub fn open_url(&mut self, url: &str) {
        if let Some(opener) = self.cx.try_global::<crate::dimension::UrlOpener>() {
            let open = opener.0.clone();
            open(url, self.cx);
        } else {
            self.cx.open_url(url);
        }
    }
}

impl ShellView {
    /// Show a position-service command's answer as the status notice, in
    /// the wording the action's `sent` notice shares
    /// (`geode_core::positions::outcome_notice`). The app's drain calls
    /// this for every `DataEvent::Command`.
    pub fn note_command(&mut self, outcome: &CommandOutcome, cx: &mut Context<Self>) {
        self.notice = Some(outcome_notice(outcome).into());
        cx.notify();
    }

    /// Run the roster action at `action`'s `chosen` with `value` picked
    /// from its `ActionCx::choose_value`. Nothing when the index names no
    /// action.
    pub(crate) fn run_action_chosen(
        &mut self,
        action: usize,
        context: &DimensionContext,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(chosen) = self.services.roster.actions().get(action).cloned() else {
            return;
        };
        let mut acx = ActionCx {
            shell: self,
            window,
            cx,
            action,
        };
        chosen.chosen(context, value, &mut acx);
    }

    /// Open the menu on `context`, hung at `at`. A no-op while a modal is
    /// open; with no rows the notice says so and nothing opens. Closes the
    /// palette, the command line, the stack list and the add-a-filter menu
    /// first, and takes the shell root's focus, as the add-a-filter menu
    /// does. `by_pointer` is a right press: that press moved focus to the
    /// tile, so the menu never hands focus back to the scope bar's field.
    pub(crate) fn open_row_menu(
        &mut self,
        context: DimensionContext,
        at: Option<Point<Pixels>>,
        by_pointer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let rows = menu_rows(&context, &self.services.roster, &self.text_dims);
        if rows.is_empty() {
            self.notice = Some(NO_ROW_ACTIONS.into());
            cx.notify();
            return;
        }
        self.close_palette(window, cx);
        self.leave_command_line(window, cx);
        self.close_stack_list(cx);
        self.matcher.cancel();
        // Recorded before the root takes focus: the root is about to hold
        // it, so this is the only record of whether the field did.
        let return_to_filter = !by_pointer && self.filter_field_focused(window, cx);
        self.row_menu = Some(RowMenu {
            menu: Menu::new(rows, self.services.keymap.bindings()),
            context,
            at,
            return_to_filter,
        });
        if !window
            .focused(cx)
            .is_some_and(|focused| focused == self.focus_handle)
        {
            self.focus_handle.focus(window, cx);
        }
        self.note_keyboard_focus_move(window, cx);
        cx.notify();
    }

    /// Close the menu, if open, leaving focus to the caller (a dialog or
    /// the palette opening over it).
    pub(crate) fn close_row_menu(&mut self, cx: &mut Context<Self>) {
        if self.row_menu.take().is_some() {
            cx.notify();
        }
    }

    /// What the tile cell's captured right press schedules: the menu, one
    /// effect later. Every mouse-down listener, capture and bubble, runs
    /// inside the one dispatch before any effect flushes, so an occupant
    /// that records its pressed row in a listener (the `press_context`
    /// contract) has done so by the time this reads it.
    pub(crate) fn open_row_menu_after_press(
        &mut self,
        id: TileId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.defer_in(window, move |view, window, cx| {
            view.open_row_menu_from_press(id, at, window, cx);
        });
    }

    /// A right press on tile `id` at `at`: the menu on its pressed row, if
    /// the occupant opens one (`press_context`); otherwise nothing.
    pub(crate) fn open_row_menu_from_press(
        &mut self,
        id: TileId,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(context) = self
            .occupants
            .get(&id)
            .and_then(|o| o.content.press_context(cx))
        else {
            return;
        };
        self.open_row_menu(context, Some(at), true, window, cx);
    }

    /// The menu closes itself (`escape`, a press outside it): cancel any
    /// chord prefix typed while it was open and give focus back: to the
    /// scope bar's text field if it held focus when the menu opened (the
    /// palette opened from the field, or a user chord bound in the
    /// workspace context) and a key opened it, home otherwise.
    pub(crate) fn dismiss_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drop_row_menu(window, cx) {
            cx.notify();
        }
    }

    /// [`Self::dismiss_row_menu`] without the notify, for `render`, which
    /// drops a menu with no point to hang from. False if none was open.
    pub(super) fn drop_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(open) = self.row_menu.take() else {
            return false;
        };
        self.matcher.cancel();
        self.overlay_return_to_filter = open.return_to_filter;
        self.return_focus_from_overlay(window, cx);
        true
    }

    /// The menu's keys while it is open: `j`/`down` and `k`/`up` step,
    /// `enter` picks, `escape` closes; any other bare key is consumed.
    /// Returns false for a chord, which passes to the matcher (and whose
    /// dispatch closes the menu).
    pub(crate) fn handle_row_menu_key(
        &mut self,
        key: &str,
        is_chord: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(open) = self.row_menu.as_mut() else {
            return false;
        };
        if is_chord {
            return false;
        }
        match key {
            "escape" => self.dismiss_row_menu(window, cx),
            "j" | "down" => open.menu.step(1),
            "k" | "up" => open.menu.step(-1),
            "enter" => {
                if let Some(i) = open.menu.highlighted() {
                    self.menu_pick(i, window, cx);
                }
            }
            _ => {} // any other bare key is consumed
        }
        cx.notify();
        true
    }
}

impl MenuHost for ShellView {
    /// An enabled row closes the menu, then runs; a disabled row is inert
    /// (its reason is already in its lane).
    fn menu_pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Ok(pick)) = self.row_menu.as_ref().and_then(|m| m.menu.pick(index)) else {
            return;
        };
        let Some(open) = self.row_menu.take() else {
            return;
        };
        // A chord prefix typed while the menu was open must not complete
        // after the pick.
        self.matcher.cancel();
        cx.notify();
        match pick {
            RowPick::Open { kind } => {
                let state = self
                    .services
                    .roster
                    .factory(kind)
                    .and_then(|f| f.launch_state(&open.context));
                self.add_tile(kind, AddPlacement::Split(None), state, window, cx);
            }
            RowPick::Action { index } => {
                let Some(action) = self.services.roster.actions().get(index).cloned() else {
                    return;
                };
                let mut acx = ActionCx {
                    shell: self,
                    window,
                    cx,
                    action: index,
                };
                action.run(&open.context, &mut acx);
            }
            RowPick::Color { column, value } => {
                choicedialog::open_value_color(self, column, value, window, cx);
            }
        }
    }

    fn menu_hover(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(open) = self.row_menu.as_mut()
            && open.menu.highlight(index)
        {
            cx.notify();
        }
    }
}
