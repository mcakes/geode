//! The grouping picker (2026-09-19): a filter-first modal in
//! [`asof_view`](super::asof_view)'s mould that lists the frame's
//! grouping slots and activates one — the mouse and typeahead form of
//! `ctrl+1..9`/`ctrl+0`, opened by `frame::grouping` (`mod+g`, the
//! palette's "Pick a grouping…") and by a click on the toolbar's
//! grouping readout (the `"1 · book / lhu"` / `"view default"` text,
//! `toolbar::toolbar`'s `on_grouping`).
//!
//! ## Architecture
//!
//! [`GroupingPickerState`] is pure (no `gpui`), stored on `ShellView` as
//! `grouping_picker: Option<GroupingPickerState>`, exactly like
//! `picker`/`as_of_dialog`: a [`ChoiceList`] over the option rows plus
//! the slot each row stands for. Ranking, the highlight, `tab`
//! completion and `enter` all go through the one choice core
//! (`choice::route` is the key table); this module adds only the digit
//! jump — `1`–`9` on an EMPTY field activate that slot outright, `0`
//! the view default, mirroring the chords — and the commit.
//!
//! Only FILLED slots are listed, plus "view default" first:
//! `Frame::set_active_slot` ignores an empty slot, and a row that
//! visibly does nothing is the defect the scope bar's `save` chip was
//! withdrawn for (`ScopeBarModel::savable`). A trader who wants a slot
//! that is not configured edits groupings (`config::edit_groupings`),
//! deliberately not offered here (user ruling 2026-09-19).
//!
//! [`open`] is the only entry point and the only place a
//! `GroupingPickerState` is constructed — nothing survives a
//! close/reopen, the same contract every other modal here keeps.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::groupings::GroupingSlots;

use crate::choice::{self, ChoiceKey, ChoiceList};
use crate::keymap::{Keystroke, Modifiers};

use super::ShellView;
use super::dialog;
use super::picker::{Hint, hint_row};
use super::scale;

// ---------------------------------------------------------------------
// Pure core — no gpui.
// ---------------------------------------------------------------------

/// The "return to the views' own grouping" row's text — the same words
/// the toolbar readout shows when no slot is active
/// (`scopebar::build_model`'s `slot_label`).
pub const VIEW_DEFAULT: &str = "view default";

/// Persistent state for one open grouping-picker session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupingPickerState {
    /// The ranked rows: [`VIEW_DEFAULT`] then every filled slot as
    /// `"{n} · {label}"` — the toolbar readout's own spelling, so the row
    /// a trader picks reads exactly as the bar will afterwards.
    pub list: ChoiceList,
    /// The slot each DECLARED option (an index into `list.options()`)
    /// activates: `None` for the view default.
    pub slots: Vec<Option<u8>>,
}

impl GroupingPickerState {
    /// The rows for `slots`, the highlight placed on `active` (the frame's
    /// current slot — `None` lights the view-default row) so `enter` on an
    /// untouched picker changes nothing, like every other choice surface.
    pub fn new(slots: &GroupingSlots, active: Option<u8>) -> Self {
        let (options, targets) = rows(slots);
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        let current = targets.iter().position(|t| *t == active);
        let text = current.map(|ix| list.options()[ix].clone());
        list.place(text.as_deref());
        Self {
            list,
            slots: targets,
        }
    }

    /// The slot the highlighted row activates, or `None` with nothing
    /// highlighted (every row filtered out). `Some(None)` is the view
    /// default.
    pub fn highlighted_slot(&self) -> Option<Option<u8>> {
        self.list.pick().map(|ix| self.slots[ix])
    }

    /// The slot a RANKED row (a click's index, `dialog::choice_rows`'s own
    /// positions) activates.
    pub fn slot_at_ranked(&self, ranked: usize) -> Option<Option<u8>> {
        self.list.ranked().get(ranked).map(|r| self.slots[r.row])
    }

    /// A digit typed into an EMPTY field — `1`–`9` the slot of that number
    /// if it is filled, `0` the view default — mirroring `ctrl+N`. `None`
    /// for an unfilled slot (the chord ignores it too) or a non-digit.
    pub fn jump(&self, key: &str) -> Option<Option<u8>> {
        let digit = key.parse::<u8>().ok().filter(|d| *d <= 9)?;
        if digit == 0 {
            return Some(None);
        }
        self.slots.iter().find(|s| **s == Some(digit)).copied()
    }
}

/// The option texts and their slots, in row order: the view default,
/// then slots 1–9 that are filled.
pub fn rows(slots: &GroupingSlots) -> (Vec<String>, Vec<Option<u8>>) {
    let mut options = vec![VIEW_DEFAULT.to_string()];
    let mut targets = vec![None];
    for n in 1..=9u8 {
        if let Some(label) = slots.label(n) {
            options.push(format!("{n} · {label}"));
            targets.push(Some(n));
        }
    }
    (options, targets)
}

// ---------------------------------------------------------------------
// gpui: the modal.
// ---------------------------------------------------------------------

/// Dialog width on the design scale — the dimension picker's.
const WIDTH: f32 = 480.0;

const HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("activate ·"),
    Hint::Key("1"),
    Hint::Text("–"),
    Hint::Key("9"),
    Hint::Text("slot ·"),
    Hint::Key("0"),
    Hint::Text("view default ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    let state = {
        let frame = view.frame.read(cx);
        GroupingPickerState::new(frame.slots(), frame.active_slot())
    };
    view.grouping_picker_scroll
        .scroll_to_item(state.list.ranked_highlighted());
    view.grouping_picker = Some(state);
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "Grouping",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

/// The status-bar notice when a picked slot was emptied under the open
/// picker (a `groupings.toml` reload — `Frame::replace_slots` — while
/// the rows still listed it): the chord ignores the same case silently,
/// but the chord never showed a list claiming the slot existed.
pub(super) const SLOT_GONE: &str = "that grouping slot is no longer configured";

/// Activate `slot` (`None` = the views' own grouping) and close — the
/// enter arm's, the digit jump's and a row click's one commit path, the
/// same door `frame::slot_N`/`frame::slot_clear` take.
fn commit(
    shell: &mut ShellView,
    slot: Option<u8>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let (changed, still_there) = shell.frame.update(cx, |f, cx| {
        let changed = f.set_active_slot(slot);
        if changed {
            cx.notify();
        }
        (changed, slot.is_none_or(|n| f.slots().get(n).is_some()))
    });
    if !changed && !still_there {
        shell.notice = Some(SLOT_GONE);
    }
    shell.close_modal(window, cx);
}

/// The [`dialog::ModalKeyHandler`] for this modal: [`choice::route`]'s
/// table, plus the digit jump on an empty field. `escape` claims nothing
/// (falls through to `handle_key_down`'s modal-closes-on-escape branch),
/// like every other dialog here.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    match choice::route(ks) {
        Some(ChoiceKey::Cancel) => return false,
        Some(ChoiceKey::Pick) => {
            // The field's live text may never have reached the list
            // through a `Change` event (`set_value` emits none): re-feed
            // it before trusting the highlight.
            let live = shell.dialog_input.read(cx).value().to_string();
            let slot = shell.grouping_picker.as_mut().and_then(|state| {
                state.list.set_query(&live);
                state.highlighted_slot()
            });
            // Nothing lit (every row filtered out): the picker stays open
            // and the empty list says it.
            if let Some(slot) = slot {
                commit(shell, slot, window, cx);
            }
            return true;
        }
        Some(ChoiceKey::Complete) => {
            // A filter-only dialog keeps its own field (spec §16.4 —
            // `sync_dialog_text` serves the mode-carrying dialogs), so
            // the completed text is written here, the way the as-of
            // dialog's calendar writes its date: `set_value` emits no
            // `Change`, and the list already holds the new query.
            let text = shell.grouping_picker.as_mut().and_then(|state| {
                state
                    .list
                    .complete()
                    .then(|| state.list.query().to_string())
            });
            if let Some(text) = text {
                let input = shell.dialog_input.clone();
                input.update(cx, |i, cx| i.set_value(text, window, cx));
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            if let Some(state) = shell.grouping_picker.as_ref() {
                shell
                    .grouping_picker_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
            }
            cx.notify();
            return true;
        }
        Some(ChoiceKey::Nav(cmd)) => {
            if let Some(state) = shell.grouping_picker.as_mut() {
                state.list.nav(cmd);
                shell
                    .grouping_picker_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
            }
            cx.notify();
            return true;
        }
        None => {}
    }
    // The digit jump: only on an empty field, so a digit typed as part
    // of a filter (`"1 · book"`) still reaches the input. `text()` is the
    // borrowed rope (its `len` is bytes), not `value()`'s fresh copy —
    // this runs per keystroke. An unfilled slot's digit is claimed and
    // dropped, as its chord is ignored: letting it type would put a
    // lone `2` in the field that matches no row, a worse answer than
    // nothing.
    if ks.mods == Modifiers::NONE
        && is_digit(&ks.key)
        && shell.dialog_input.read(cx).text().len() == 0
    {
        let slot = shell
            .grouping_picker
            .as_ref()
            .and_then(|state| state.jump(&ks.key));
        if let Some(slot) = slot {
            commit(shell, slot, window, cx);
        }
        return true;
    }
    false
}

fn is_digit(key: &str) -> bool {
    key.len() == 1 && key.as_bytes()[0].is_ascii_digit()
}

/// The body: the shared filter field, the ranked slot rows through
/// `dialog::choice_rows` (a row click activates — a pick, as in the
/// market-data underlying picker, not the dialogs' `tab`), and the hint
/// line.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.grouping_picker.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let click_entity = entity.clone();
    let rows = dialog::choice_rows(
        &state.list,
        "grouping",
        &shell.grouping_picker_scroll,
        theme,
        move |ranked, window, cx| {
            click_entity.update(cx, |shell, cx| {
                let slot = shell
                    .grouping_picker
                    .as_ref()
                    .and_then(|state| state.slot_at_ranked(ranked));
                if let Some(slot) = slot {
                    commit(shell, slot, window, cx);
                }
            });
        },
    );
    v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx))
        .child(rows)
        .child(hint_row(
            HINTS,
            "grouping-hints",
            WIDTH,
            muted,
            theme.muted,
            theme.border,
            theme.radius,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_keystroke;

    fn slots() -> GroupingSlots {
        let mut s = GroupingSlots::default();
        s.set(1, vec!["book".into(), "lhu".into()]);
        s.set(3, vec!["underlying_ref".into()]);
        s
    }

    /// Only filled slots are rows, the view default first, each spelled
    /// as the toolbar readout spells it.
    #[test]
    fn rows_are_the_view_default_then_every_filled_slot() {
        let (options, targets) = rows(&slots());
        assert_eq!(
            options,
            vec!["view default", "1 · book / lhu", "3 · underlying_ref"]
        );
        assert_eq!(targets, vec![None, Some(1), Some(3)]);
    }

    /// Opening on the frame's active slot lights that row, so `enter`
    /// on an untouched picker changes nothing.
    #[test]
    fn the_highlight_opens_on_the_active_slot() {
        let state = GroupingPickerState::new(&slots(), Some(3));
        assert_eq!(state.highlighted_slot(), Some(Some(3)));
        let state = GroupingPickerState::new(&slots(), None);
        assert_eq!(state.highlighted_slot(), Some(None));
    }

    /// Typing narrows the rows and `enter` picks the highlighted one;
    /// a query matching nothing leaves nothing to pick.
    #[test]
    fn typing_narrows_and_the_highlight_names_a_slot() {
        let mut state = GroupingPickerState::new(&slots(), None);
        state.list.set_query("under");
        assert_eq!(state.highlighted_slot(), Some(Some(3)));
        state.list.set_query("zzz");
        assert_eq!(state.highlighted_slot(), None);
    }

    /// A digit jumps to that slot when filled, `0` to the view default,
    /// and an unfilled slot's digit does nothing — the chords' own rule.
    #[test]
    fn a_digit_jumps_to_a_filled_slot_or_the_view_default() {
        let state = GroupingPickerState::new(&slots(), None);
        assert_eq!(state.jump("3"), Some(Some(3)));
        assert_eq!(state.jump("0"), Some(None));
        assert_eq!(state.jump("2"), None, "an empty slot is not a target");
        assert_eq!(state.jump("j"), None);
    }

    /// The ranked index a click hands back resolves through the RANKED
    /// list, not the declared one — after a filter the two differ.
    #[test]
    fn a_click_resolves_through_the_ranked_order() {
        let mut state = GroupingPickerState::new(&slots(), None);
        state.list.set_query("under");
        assert_eq!(state.slot_at_ranked(0), Some(Some(3)));
        assert_eq!(state.slot_at_ranked(1), None);
    }

    /// `choice::route` is the key table: a bare digit is none of its keys
    /// (it reaches the jump), `enter` is the pick.
    #[test]
    fn a_bare_digit_is_not_a_choice_key() {
        let one = parse_keystroke("1", Modifiers::NONE).unwrap();
        assert_eq!(choice::route(&one), None);
        let enter = parse_keystroke("enter", Modifiers::NONE).unwrap();
        assert_eq!(choice::route(&enter), Some(ChoiceKey::Pick));
    }
}
