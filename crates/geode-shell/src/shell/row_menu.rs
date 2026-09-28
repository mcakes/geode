//! The row menu: what a tile row's single-valued columns let the user do
//! (spec §3), built by [`crate::dimension::menu_rows`]. The shell owns it
//! so every grid gets the same menu from one `dimension_context`. Opened by
//! `tile::context_menu` at the cursor row; closed by a pick, `escape`, a
//! press outside it, a chord's dispatch, a dialog or the palette opening.

use geode_core::context::DimensionContext;
use gpui::{Context, Pixels, Point, Window};

use super::ShellView;
use crate::defaults::AddPlacement;
use crate::dimension::{RowPick, menu_rows};
use crate::menu::{Menu, MenuHost};

/// Status notice: the row names no value any kind or action takes.
pub(crate) const NO_ROW_ACTIONS: &str = "no actions for this row";

/// The open row menu.
pub struct RowMenu {
    menu: Menu<RowPick>,
    context: DimensionContext,
    /// Window point the menu hangs from; `None` → the focused tile's
    /// top-left.
    at: Option<Point<Pixels>>,
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
}

impl ActionCx<'_, '_> {
    /// Split a new tile of `kind` beside the focused one, restored from
    /// `state` (as `g m` does).
    pub fn open_tile(&mut self, kind: &str, state: Option<toml::Table>) {
        self.shell
            .add_tile(kind, AddPlacement::Split(None), state, self.window, self.cx);
    }

    /// Show `text` as the shell's status notice.
    pub fn notice(&mut self, text: &'static str) {
        self.shell.notice = Some(text);
        self.cx.notify();
    }
}

impl ShellView {
    /// Open the menu on `context`, hung at `at`. A no-op while a modal is
    /// open; with no rows the notice says so and nothing opens. Closes the
    /// palette, the command line, the stack list and the add-a-filter menu
    /// first, and takes the shell root's focus, as the add-a-filter menu
    /// does.
    pub(crate) fn open_row_menu(
        &mut self,
        context: DimensionContext,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        let rows = menu_rows(&context, &self.services.roster);
        if rows.is_empty() {
            self.notice = Some(NO_ROW_ACTIONS);
            cx.notify();
            return;
        }
        self.close_palette(window, cx);
        self.leave_command_line(window, cx);
        self.close_stack_list(cx);
        self.close_add_filter_menu(cx);
        self.matcher.cancel();
        self.row_menu = Some(RowMenu {
            menu: Menu::new(rows, self.services.keymap.bindings()),
            context,
            at,
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

    /// The menu closes itself (`escape`, a press outside it): cancel any
    /// chord prefix typed while it was open and give focus back home. The
    /// menu never opens from the scope bar's text field, so the return
    /// never goes there.
    pub(crate) fn dismiss_row_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.row_menu.take().is_none() {
            return;
        }
        self.matcher.cancel();
        self.overlay_return_to_filter = false;
        self.return_focus_from_overlay(window, cx);
        cx.notify();
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
                };
                action.run(&open.context, &mut acx);
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
