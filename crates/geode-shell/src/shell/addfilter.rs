//! The scope bar's menu for adding dimension and expression filters. The
//! expression dialog offers named expressions beside typed ones, so they
//! have no row of their own.
//!
//! The `+` control opens the menu below its glyph. Rows show live bindings
//! through [`kbd::menu_binding`]. `j`/`k`/`up`/`down` move the selection;
//! Enter or a row click dispatches that row's shell action. Escape or a
//! click outside closes the menu.
//!
//! [`AddFilterMenu`] owns pure state and [`step`] owns navigation. Rows are
//! prepared when the menu opens; [`render`] only paints them. The menu uses
//! the shell's `ActionId` registry and keymap for dispatch and binding labels.
//! It shares the component menu's popover surface, accent highlight, and key
//! lane styling.

use gpui::prelude::*;
use gpui::{
    Anchor, AnchoredPositionMode, App, IntoElement, MouseButton, SharedString, Window, anchored,
    deferred, px,
};
use gpui_component::{ActiveTheme as _, ThemeStyled as _, h_flex, v_flex};

use super::kbd;
use super::scale;
use crate::keymap::{Binding, Keystroke};

/// Row height on the design scale (PopupMenu's geometry).
const ROW_HEIGHT: f32 = 26.0;
/// Horizontal row inset on the design scale.
const ROW_INSET: f32 = 8.0;
/// The panel's minimum width on the design scale.
const MIN_WIDTH: f32 = 180.0;

/// One way of adding a filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Dimension,
    Expression,
}

impl Entry {
    /// The rows in menu order.
    pub const ALL: [Entry; 2] = [Entry::Dimension, Entry::Expression];

    pub fn title(self) -> &'static str {
        match self {
            Entry::Dimension => "Dimension…",
            Entry::Expression => "Expression…",
        }
    }

    /// The action a commit dispatches.
    pub fn action(self) -> &'static str {
        match self {
            Entry::Dimension => "frame::pick",
            Entry::Expression => "frame::add_expression",
        }
    }

    /// The domain name the row's element id and selector derive from.
    fn key(self) -> &'static str {
        match self {
            Entry::Dimension => "dimension",
            Entry::Expression => "expression",
        }
    }
}

/// A prepared row: the entry and its action's binding under the keymap
/// the menu opened with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub entry: Entry,
    pub keys: Option<Vec<Keystroke>>,
}

/// The open menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddFilterMenu {
    pub rows: Vec<Row>,
    /// The row `enter` commits; opens on the first.
    pub highlighted: usize,
    /// Whether the scope bar's text field held focus when the menu
    /// opened. Opening takes the shell root's focus, so this is the only
    /// record of where focus goes back to: the menu's own close returns it
    /// there, and a row commit hands it to the dialog the row opens.
    pub return_to_filter: bool,
}

impl AddFilterMenu {
    /// Prepare the rows against `bindings` (the live keymap).
    pub fn new(bindings: &[Binding], return_to_filter: bool) -> Self {
        Self {
            rows: Entry::ALL
                .iter()
                .map(|&entry| Row {
                    entry,
                    keys: crate::tips::chord_for(bindings, entry.action()),
                })
                .collect(),
            highlighted: 0,
            return_to_filter,
        }
    }

    /// The highlighted row's entry.
    pub fn selected(&self) -> Option<Entry> {
        self.rows.get(self.highlighted).map(|r| r.entry)
    }
}

/// Move by `delta` with wrap; an empty menu stays unchanged.
pub fn step(menu: &mut AddFilterMenu, delta: i64) {
    let len = menu.rows.len() as i64;
    if len == 0 {
        return;
    }
    menu.highlighted = (menu.highlighted as i64 + delta).rem_euclid(len) as usize;
}

/// The panel, deferred and anchored below its parent's origin: the
/// caller places this inside a box at the `+` glyph's bottom-left, so the
/// menu hangs under the glyph and snaps inside the window. Row presses
/// stop propagation so they never reach the click catcher beneath.
pub fn render(
    menu: &AddFilterMenu,
    on_row_click: impl Fn(Entry, &mut Window, &mut App) + Clone + 'static,
    on_row_hover: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let mut panel = v_flex()
        .min_w(scale::design(MIN_WIDTH))
        .mt_1()
        .p_1()
        .gap_y_0p5()
        .text_sm()
        .popover_style(cx)
        .debug_selector(|| "scope-add-menu".to_string())
        .occlude();
    for (i, row) in menu.rows.iter().enumerate() {
        let highlighted = i == menu.highlighted;
        let lane = if highlighted {
            theme.accent_foreground
        } else {
            theme.muted_foreground
        };
        let entry = row.entry;
        let on_click = on_row_click.clone();
        let on_hover = on_row_hover.clone();
        panel = panel.child(
            h_flex()
                .id(match entry {
                    Entry::Dimension => "scope-add-row-dimension",
                    Entry::Expression => "scope-add-row-expression",
                })
                .h(scale::design(ROW_HEIGHT))
                .px(scale::design(ROW_INSET))
                .rounded(theme.radius)
                .items_center()
                .justify_between()
                .gap_4()
                .map(|d| {
                    if highlighted {
                        d.bg(theme.accent).text_color(theme.accent_foreground)
                    } else {
                        d.text_color(theme.popover_foreground)
                    }
                })
                .debug_selector(move || format!("scope-add-menu-row-{}", entry.key()))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    on_click(entry, window, cx);
                })
                // Hovering a row is the mouse form of `j`/`k`: the
                // highlight follows the pointer, as in `PopupMenu`.
                .on_mouse_move(move |_, window, cx| on_hover(i, window, cx))
                .child(SharedString::new_static(entry.title()))
                .when_some(row.keys.as_ref(), |d, keys| {
                    d.child(
                        kbd::menu_binding(keys, lane)
                            .debug_selector(move || format!("scope-add-menu-key-{}", entry.key())),
                    )
                }),
        );
    }
    deferred(
        anchored()
            .anchor(Anchor::TopLeft)
            .position_mode(AnchoredPositionMode::Local)
            .snap_to_window_with_margin(px(8.))
            .child(panel),
    )
    .with_priority(1)
}

impl super::ShellView {
    /// Open the menu (the `+` verb's click). A no-op while a modal is
    /// open. Closes the palette, the command line and the stack list
    /// first, so exactly one surface owns the keyboard, and takes the
    /// shell root's focus for the same reason the stack list does: the
    /// scope bar's text field may hold it, and the menu's `j`/`k`/`enter`
    /// must not type into it.
    pub(super) fn open_add_filter_menu(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.close_palette(window, cx);
        self.leave_command_line(window, cx);
        self.close_stack_list(cx);
        self.matcher.cancel();
        // Recorded before the root takes focus, as the dialog door records
        // it before its field does.
        let return_to_filter = self.filter_field_focused(window, cx);
        self.add_filter_menu = Some(AddFilterMenu::new(
            self.services.keymap.bindings(),
            return_to_filter,
        ));
        if !window
            .focused(cx)
            .is_some_and(|focused| focused == self.focus_handle)
        {
            self.focus_handle.focus(window, cx);
        }
        self.note_keyboard_focus_move(window, cx);
        cx.notify();
    }

    /// Close the menu, if open, leaving focus to the caller (the palette
    /// opening over it).
    pub(super) fn close_add_filter_menu(&mut self, cx: &mut gpui::Context<Self>) {
        if self.add_filter_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The menu closes itself (`escape`, a press outside it): cancel any
    /// chord prefix typed while it was open, so a half-typed sequence
    /// cannot complete after it, and hand focus back to the text field if
    /// the field held it when the menu opened.
    pub(super) fn dismiss_add_filter_menu(
        &mut self,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let Some(menu) = self.add_filter_menu.take() else {
            return;
        };
        self.matcher.cancel();
        self.overlay_return_to_filter = menu.return_to_filter;
        self.return_focus_from_overlay(window, cx);
        cx.notify();
    }

    /// Commit `entry`: dispatch its action, which also closes the menu —
    /// the same door the palette and a user binding reach. The dialog the
    /// action opens recorded the shell root as its focus origin (the menu
    /// holds it), so the menu's own record replaces that one: closing the
    /// dialog returns to the text field when the menu was opened from it.
    pub(super) fn commit_add_filter(
        &mut self,
        entry: Entry,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let return_to_filter = self
            .add_filter_menu
            .take()
            .is_some_and(|m| m.return_to_filter);
        self.dispatch(
            &crate::actions::ActionId(entry.action().to_string()),
            None,
            window,
            cx,
        );
        if return_to_filter {
            if self.modal_open() {
                self.overlay_return_to_filter = true;
            } else {
                // Nothing opened (the action refused): back to the field now.
                self.overlay_return_to_filter = true;
                self.return_focus_from_overlay(window, cx);
            }
        }
        cx.notify();
    }

    /// The pointer moved onto row `i`: highlight it.
    pub(super) fn hover_add_filter(&mut self, i: usize, cx: &mut gpui::Context<Self>) {
        if let Some(menu) = self.add_filter_menu.as_mut()
            && menu.highlighted != i
            && i < menu.rows.len()
        {
            menu.highlighted = i;
            cx.notify();
        }
    }

    /// The menu's keys while it is open: `j`/`down` and `k`/`up` move,
    /// `enter` commits, `escape` closes; any other bare key is consumed.
    /// Returns false for a chord, which passes to the matcher (and whose
    /// dispatch closes the menu).
    pub(super) fn handle_add_filter_key(
        &mut self,
        key: &str,
        is_chord: bool,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(menu) = self.add_filter_menu.as_mut() else {
            return false;
        };
        if is_chord {
            return false;
        }
        match key {
            "escape" => self.dismiss_add_filter_menu(window, cx),
            "j" | "down" => step(menu, 1),
            "k" | "up" => step(menu, -1),
            "enter" => {
                if let Some(entry) = menu.selected() {
                    self.commit_add_filter(entry, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ActionId;
    use crate::keymap::{Modifiers, parse_binding};

    fn binding(keys: &str, action: &str) -> Binding {
        Binding {
            keystrokes: parse_binding(keys, Modifiers::NONE).unwrap(),
            predicate: None,
            action: ActionId(action.to_string()),
            layer: geode_core::config::Layer::Builtin,
            index: 0,
            context_source: None,
            key_source: keys.to_string(),
        }
    }

    #[test]
    fn rows_carry_each_actions_live_binding() {
        let menu = AddFilterMenu::new(&[binding("ctrl+p", "frame::pick")], false);
        assert_eq!(
            menu.rows.iter().map(|r| r.entry).collect::<Vec<_>>(),
            Entry::ALL
        );
        assert_eq!(
            menu.rows[0].keys,
            Some(parse_binding("ctrl+p", Modifiers::NONE).unwrap())
        );
        assert_eq!(menu.rows[1].keys, None, "an unbound action shows no key");
        let menu = AddFilterMenu::new(&[binding("ctrl+e", "frame::add_expression")], false);
        assert_eq!(menu.rows[0].keys, None);
        assert!(
            menu.rows[1].keys.is_some(),
            "a bound add_expression shows it"
        );
    }

    #[test]
    fn step_wraps_and_selected_follows() {
        let mut m = AddFilterMenu::new(&[], false);
        assert_eq!(m.selected(), Some(Entry::Dimension));
        step(&mut m, 1);
        assert_eq!(m.selected(), Some(Entry::Expression));
        step(&mut m, 1);
        assert_eq!(m.selected(), Some(Entry::Dimension), "wraps forward");
        step(&mut m, -1);
        assert_eq!(m.selected(), Some(Entry::Expression), "wraps backward");
    }
}
