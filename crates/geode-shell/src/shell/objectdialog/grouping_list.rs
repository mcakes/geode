//! The Groupings dialog's list. Besides the nine slots it leads with two rows
//! that are not configuration objects: the view default and the lane's ad
//! hoc chain. This module names those rows, says what applying a row does to
//! the frame, and owns the list's own keys; the shared browse and edit
//! handlers call in through `Domain::applies_from_browse`.

use std::rc::Rc;

use geode_core::config::Layer;
use geode_core::groupings::GroupingSlots;
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Window, div};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};

use super::super::dialog::{ConfirmAnswer, ConfirmHandler, confirm_row, sync_dialog_text};
use super::super::kbd;
use super::{ObjectRow, render};
use crate::footer::{Hint, HintRow};
use crate::frame::{FrameView, GroupingChoice};
use crate::keymap::{Keystroke, Modifiers};
use crate::shell::ShellView;

/// The view-default row's name. `0` is also its key, as `ctrl+0` is its chord.
pub const VIEW_DEFAULT: &str = "0";
/// The ad hoc row's name, and the marker the toolbar readout uses for it.
pub const AD_HOC: &str = "*";
pub const VIEW_DEFAULT_TEXT: &str = "view default";
pub const NO_AD_HOC_TEXT: &str = "no ad hoc chain";
/// The view-default row's right-hand note: what choosing it means.
pub const VIEW_DEFAULT_NOTE: &str = "each view's own grouping";
pub const NOTHING_TO_EDIT: &str = "view default has nothing to edit";
pub const NO_ROW: &str = "no row is selected";
pub const NO_AD_HOC_CHAIN: &str = "no ad hoc chain yet";
pub const AD_HOC_NO_REVERT: &str = "the ad hoc chain has nothing to revert to";
pub const NOTHING_TO_CLEAR: &str = "view default has nothing to clear";
pub const NOTHING_TO_REVERT: &str = "view default has nothing to revert";

/// Which row of the list a name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    ViewDefault,
    AdHoc,
    Slot(u8),
}

pub fn kind_of(name: &str) -> Option<RowKind> {
    match name {
        VIEW_DEFAULT => Some(RowKind::ViewDefault),
        AD_HOC => Some(RowKind::AdHoc),
        _ => name
            .parse::<u8>()
            .ok()
            .filter(|n| (1..=9).contains(n))
            .map(RowKind::Slot),
    }
}

/// The kind of the row under the list cursor.
fn cursor_kind(shell: &ShellView) -> Option<RowKind> {
    let state = shell.object_dialog.as_ref()?;
    kind_of(&state.rows.at(state.selected)?.name)
}

/// The list's own keys in normal mode. `None` hands the key to the shared
/// browse handler: motion, `/`, and `d`/`r` on a slot row. The caller's
/// key path syncs the dialog text on return, so a chain field opened here
/// takes the input.
pub(super) fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> Option<bool> {
    if ks.mods != Modifiers::NONE {
        return None;
    }
    let kind = cursor_kind(shell);
    match ks.key.as_str() {
        "enter" => match kind {
            Some(kind) => apply(shell, kind, window, cx),
            None => render::set_notice(shell, NO_ROW.to_string()),
        },
        "0" => apply(shell, RowKind::ViewDefault, window, cx),
        "a" => apply(shell, RowKind::AdHoc, window, cx),
        key if key.len() == 1 && (b'1'..=b'9').contains(&key.as_bytes()[0]) => {
            apply(shell, RowKind::Slot(key.as_bytes()[0] - b'0'), window, cx)
        }
        "e" => edit(shell, kind, cx),
        "i" => {
            let seed = kind.and_then(|kind| chain_of(shell, kind, cx));
            open_ad_hoc_chain(shell, seed, cx);
        }
        "s" => arm_save(shell, cx),
        "d" if kind == Some(RowKind::AdHoc) => forget(shell, cx),
        "d" if kind == Some(RowKind::ViewDefault) => {
            render::set_notice(shell, NOTHING_TO_CLEAR.to_string())
        }
        "r" if kind == Some(RowKind::AdHoc) => {
            render::set_notice(shell, AD_HOC_NO_REVERT.to_string())
        }
        "r" if kind == Some(RowKind::ViewDefault) => {
            render::set_notice(shell, NOTHING_TO_REVERT.to_string())
        }
        _ => return None,
    }
    cx.notify();
    Some(true)
}

/// A row click is `enter` on that row. The click handler syncs the dialog
/// text afterwards, as the key path does.
///
/// A click that opened a stage instead of closing the dialog (an empty
/// slot's or an empty ad hoc row's chain field) marks it
/// `click_opened_stage`: the field's completion rows paint where the list
/// was, and a double-click's second half would otherwise complete
/// whichever dimension now sits under the pointer.
pub(super) fn click(
    shell: &mut ShellView,
    name: &str,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if let Some(kind) = kind_of(name) {
        apply(shell, kind, window, cx);
    }
    // Only this dialog's own stage: a click that applied and closed may
    // have revealed another domain's parked dialog, which this click never
    // touched.
    if let Some(state) = shell.object_dialog.as_mut()
        && state.domain.applies_from_browse()
        && matches!(&state.stage, super::Stage::Edit { object } if object == name)
    {
        state.click_opened_stage = true;
    }
}

/// Apply a row to the lane the dialog targets and close. A slot the frame
/// holds no chain for has nothing to apply, so its chain field opens
/// instead. Emptiness is the frame's answer, not the row's layer: a slot
/// the configuration defines but the reader dropped is one
/// `set_active_slot` would refuse, and its field is where it gets fixed.
fn apply(shell: &mut ShellView, kind: RowKind, window: &mut Window, cx: &mut Context<ShellView>) {
    // Resolved before the modal closes: the target lane is the one the
    // modal stack was opened from.
    let frame = shell.target_frame();
    match kind {
        RowKind::ViewDefault => {
            frame.update(cx, |f, cx| {
                if f.set_active_slot(None) {
                    cx.notify();
                }
            });
        }
        RowKind::Slot(n) => {
            if frame.read(cx).slots().get(n).is_none() {
                open_slot_chain(shell, n, cx);
                return;
            }
            frame.update(cx, |f, cx| {
                if f.set_active_slot(Some(n)) {
                    cx.notify();
                }
            });
        }
        RowKind::AdHoc => {
            if frame.read(cx).ad_hoc().is_none() {
                open_ad_hoc_chain(shell, None, cx);
                return;
            }
            frame.update(cx, |f, cx| {
                if f.activate_ad_hoc() {
                    cx.notify();
                }
            });
        }
    }
    shell.close_modal(window, cx);
}

/// `e`: the row's tick-list editor, for an empty slot too, so ticking stays
/// a way to define one.
fn edit(shell: &mut ShellView, kind: Option<RowKind>, cx: &mut Context<ShellView>) {
    match kind {
        Some(RowKind::Slot(n)) => render::enter_edit_stage(shell, &n.to_string(), None, cx),
        Some(RowKind::AdHoc) => enter_ad_hoc_stage(shell, None, cx),
        Some(RowKind::ViewDefault) => render::set_notice(shell, NOTHING_TO_EDIT.to_string()),
        None => render::set_notice(shell, NO_ROW.to_string()),
    }
}

/// Open slot `n`'s edit stage straight in its chain field, marked as opened
/// from the list.
fn open_slot_chain(shell: &mut ShellView, n: u8, cx: &mut Context<ShellView>) {
    render::enter_edit_stage(shell, &n.to_string(), None, cx);
    render::open_field(shell);
    mark_chain_from_list(shell);
}

/// Record that the field just opened came from the list. Only when a field
/// actually opened: the flag on a stage with no field would turn that
/// stage's next `i`/`enter` into a close.
fn mark_chain_from_list(shell: &mut ShellView) {
    if let Some(state) = shell.object_dialog.as_mut() {
        state.chain_from_list = state
            .draft
            .as_ref()
            .is_some_and(|draft| draft.text_entry.is_some());
    }
}

/// `enter` in a chain field opened from the list, after the draft took the
/// typed chain: carry it out and close. The slot is written through the
/// pending batch, staged in the frame so it can be activated on this
/// keystroke rather than after the debounced write, and activated. A
/// refused write leaves the stage open with the refusal as its notice and
/// the frame untouched: a chain staged without a queued write would be in
/// force while persisted nowhere.
pub(super) fn finish_list_chain(
    shell: &mut ShellView,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let Some((name, chain, dirty)) = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .map(|draft| {
            (
                draft.name.clone(),
                super::groupings::ticked(draft),
                draft.is_dirty(),
            )
        })
    else {
        return;
    };
    if let Some(state) = shell.object_dialog.as_mut() {
        state.chain_from_list = false;
    }
    // Both targets pass the same check: an untouched seed (a slot's chain as
    // the frame holds it) was never checked against this field's vocabulary.
    render::revalidate(shell);
    if let Some(refusal) = super::apply::blocking_diagnostic(shell) {
        // Refused in place, as a typed unknown name is: the field reopens on
        // the same chain, so the fix is an edit, not a retype from the list.
        render::open_field(shell);
        mark_chain_from_list(shell);
        render::set_notice(shell, refusal);
        return;
    }
    let n = match kind_of(&name) {
        Some(RowKind::Slot(n)) => n,
        Some(RowKind::AdHoc) => {
            // Through `set_ad_hoc` even when the typed chain equals the seed:
            // an untouched seed is still a request to apply that chain. The
            // field refused an empty chain in place, so `set_ad_hoc` refuses
            // only a chain already in force, where closing is the answer.
            let frame = shell.target_frame();
            frame.update(cx, |f, cx| {
                if f.set_ad_hoc(chain) {
                    cx.notify();
                }
            });
            shell.close_modal(window, cx);
            return;
        }
        _ => return,
    };
    if dirty && let Some(refusal) = super::apply::commit_edit(shell, cx) {
        render::set_notice(shell, refusal);
        return;
    }
    let frame = shell.target_frame();
    frame.update(cx, |f, cx| {
        f.stage_slot(n, chain);
        f.set_active_slot(Some(n));
        cx.notify();
    });
    shell.close_modal(window, cx);
}

/// Enter the ad hoc chain's edit stage. `seed` replaces the stored chain as
/// the starting point (`i` on another row); the frame is not changed until
/// an edit or `enter` commits.
fn enter_ad_hoc_stage(
    shell: &mut ShellView,
    seed: Option<Vec<String>>,
    cx: &mut Context<ShellView>,
) {
    let chain = seed
        .or_else(|| {
            shell
                .target_frame()
                .read(cx)
                .ad_hoc()
                .map(<[String]>::to_vec)
        })
        .unwrap_or_default();
    let config =
        super::apply::config_with_pending(shell).unwrap_or_else(|| shell.services.config.clone());
    let draft = super::groupings::ad_hoc_draft(&config, &chain);
    render::enter_edit_stage(shell, AD_HOC, Some(draft), cx);
}

/// Open the ad hoc chain's field from the list, seeded with `seed` or the
/// stored chain.
pub(super) fn open_ad_hoc_chain(
    shell: &mut ShellView,
    seed: Option<Vec<String>>,
    cx: &mut Context<ShellView>,
) {
    enter_ad_hoc_stage(shell, seed, cx);
    render::open_field(shell);
    mark_chain_from_list(shell);
}

/// The chain a row holds, for `i`'s seed: a slot's as the frame holds it,
/// the stored ad hoc chain, nothing for the view default or an empty slot
/// (the stored ad hoc chain then seeds instead).
fn chain_of(shell: &ShellView, kind: RowKind, cx: &Context<ShellView>) -> Option<Vec<String>> {
    let frame = shell.target_frame();
    let view = frame.read(cx);
    match kind {
        RowKind::Slot(n) => view.slots().get(n).map(<[String]>::to_vec),
        RowKind::AdHoc => view.ad_hoc().map(<[String]>::to_vec),
        RowKind::ViewDefault => None,
    }
}

/// Send the open ad hoc draft's chain to the frame. `true` when the open
/// draft is the ad hoc chain, whether or not anything changed: the config
/// writer must never see it, so the caller stops here either way.
pub(super) fn commit_ad_hoc(shell: &mut ShellView, cx: &mut Context<ShellView>) -> bool {
    let Some(chain) = shell
        .object_dialog
        .as_ref()
        .and_then(|state| {
            state
                .draft
                .as_ref()
                .filter(|draft| is_ad_hoc(state.domain, &draft.name))
        })
        .map(super::groupings::ticked)
    else {
        return false;
    };
    if let Some(refusal) = super::apply::blocking_diagnostic(shell) {
        render::set_notice(shell, refusal);
        return true;
    }
    let frame = shell.target_frame();
    frame.update(cx, |f, cx| {
        if f.set_ad_hoc(chain) {
            cx.notify();
        }
    });
    if let Some(draft) = shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
    {
        draft.mark_saved();
    }
    true
}

/// `d` on the ad hoc chain, from the list or its editor: forget it. No
/// question: the chain is a few names and lives nowhere a revert could
/// restore it from.
pub(super) fn forget(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let frame = shell.target_frame();
    let forgot = frame.update(cx, |f, cx| {
        let forgot = f.forget_ad_hoc();
        if forgot {
            cx.notify();
        }
        forgot
    });
    if !forgot {
        render::set_notice(shell, NO_AD_HOC_CHAIN.to_string());
    }
}

/// The list's normal-mode footer.
pub(super) fn list_hints(query_is_empty: bool) -> Vec<Hint> {
    vec![
        Hint::new(HintRow::Move, &["j", "k"], "row"),
        Hint::range(HintRow::Move, "1", "9", "slot"),
        Hint::new(HintRow::Move, &["0"], "view default"),
        Hint::new(HintRow::Move, &["a"], "ad hoc"),
        Hint::new(HintRow::Edit, &["i"], "type a chain"),
        Hint::new(HintRow::Edit, &["e"], "edit row"),
        Hint::new(HintRow::Edit, &["s"], "save row to slot…"),
        Hint::new(HintRow::Edit, &["d"], "clear"),
        Hint::new(HintRow::Edit, &["r"], "revert"),
        Hint::new(HintRow::Go, &["enter"], "apply").selector("objectdialog-hint-enter"),
        Hint::new(HintRow::Go, &["/"], "filter"),
        Hint::new(
            HintRow::Go,
            &["escape"],
            if query_is_empty {
                "close"
            } else {
                "clear the filter"
            },
        ),
    ]
}

/// Whether `name` in `domain` is one of the two rows the frame supplies
/// rather than the config. Only Groupings has them: another domain accepts
/// `0` and `*` as ordinary object names, and dropping those from a count or
/// carrying them over as leading rows would miscount the list and paint the
/// object twice.
pub fn is_lead(domain: super::Domain, name: &str) -> bool {
    domain == super::Domain::Groupings && (name == VIEW_DEFAULT || name == AD_HOC)
}

/// The two leading rows for `frame`'s lane. Neither has a layer: nothing in
/// any config document defines them, so `d` and `r` have nothing to act on.
pub fn lead_rows(frame: &FrameView<'_>) -> Vec<ObjectRow> {
    let ad_hoc = frame
        .ad_hoc()
        .map(GroupingSlots::label_of)
        .unwrap_or_else(|| NO_AD_HOC_TEXT.to_string());
    [
        (VIEW_DEFAULT, VIEW_DEFAULT_TEXT.to_string()),
        (AD_HOC, ad_hoc),
    ]
    .into_iter()
    .map(|(name, summary)| ObjectRow {
        name: name.to_string(),
        summary,
        layer: None,
        overridden: false,
        drifted: false,
        prefix: None,
    })
    .collect()
}

/// The name of the row standing for the lane's choice.
pub fn active_row(choice: GroupingChoice) -> String {
    match choice {
        GroupingChoice::ViewDefault => VIEW_DEFAULT.to_string(),
        GroupingChoice::AdHoc => AD_HOC.to_string(),
        GroupingChoice::Slot(n) => n.to_string(),
    }
}

/// Whether the row holds a chain to apply, edit or save: a configured slot,
/// or the ad hoc row once a chain is stored. The view default never does.
pub fn has_chain(row: &ObjectRow) -> bool {
    match kind_of(&row.name) {
        Some(RowKind::Slot(_)) => row.layer.is_some(),
        Some(RowKind::AdHoc) => row.summary != NO_AD_HOC_TEXT,
        Some(RowKind::ViewDefault) | None => false,
    }
}

/// Whether `name` in `domain` is the lane's ad hoc chain. Only Groupings
/// has one: another domain accepts `*` as an ordinary object name, and
/// treating that object as the ad hoc chain would drop its edits and turn
/// its `d` into forgetting the lane's chain.
pub fn is_ad_hoc(domain: super::Domain, name: &str) -> bool {
    domain == super::Domain::Groupings && name == AD_HOC
}

/// How an edit stage names its object on screen.
pub fn title_of(domain: super::Domain, name: &str) -> String {
    if is_ad_hoc(domain, name) {
        "ad hoc".to_string()
    } else {
        name.to_string()
    }
}

/// The pending save: the chain, the slot awaiting a y/n (with the label of
/// the chain a yes would lose), and whether `mod+s` in a list-opened chain
/// field armed it, so cancelling returns to that field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SaveToSlot {
    pub chain: Vec<String>,
    pub replace: Option<u8>,
    pub replaced: String,
    pub from_field: bool,
}

/// What saving a chain into a slot would do to what the slot holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveTarget {
    /// Nothing defines the slot.
    Empty,
    /// The slot already holds this chain: nothing to write.
    Equal,
    /// Only a lower layer defines it: a save forks it, and `r` restores it.
    Inherited,
    /// The user layer defines it with another chain, which a save would lose.
    Owned,
}

/// Classify slot `slot` as a save target for `chain`. `held` is the chain
/// the slot holds now, `rows` the list's rows (for the layer that defines it).
pub fn classify(
    rows: &[ObjectRow],
    held: Option<&[String]>,
    slot: u8,
    chain: &[String],
) -> SaveTarget {
    if held == Some(chain) {
        return SaveTarget::Equal;
    }
    let name = slot.to_string();
    match rows
        .iter()
        .find(|row| row.name == name)
        .and_then(|row| row.layer)
    {
        None => SaveTarget::Empty,
        Some(Layer::User) => SaveTarget::Owned,
        Some(_) => SaveTarget::Inherited,
    }
}

pub const NOTHING_TO_SAVE: &str = "this row has no chain to save";

/// `s`: ask which slot to save the chain to. From the list, the cursor
/// row's chain; from an edit stage or its chain field, the draft's.
pub(super) fn arm_save(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    arm(shell, false, cx);
}

/// `mod+s` in a chain field opened from the list: take the typed chain into
/// the draft, then ask which slot to save it to. Any refusal (a typed name
/// the field does not know, or a chain the draft's check refuses, such as an
/// untouched seed naming a column that no longer exists) keeps the field
/// open with the refusal, as `enter` does.
pub(super) fn save_from_field(shell: &mut ShellView, cx: &mut Context<ShellView>) {
    let step = shell
        .object_dialog
        .as_mut()
        .and_then(|state| state.draft.as_mut())
        .map(super::Draft::apply_chain);
    match step {
        Some(super::Step::Changed | super::Step::Inert) => {
            if let Some(state) = shell.object_dialog.as_mut() {
                state.mode = crate::dialogmode::DialogMode::Normal;
            }
            render::revalidate(shell);
            if let Some(refusal) = super::apply::blocking_diagnostic(shell) {
                reopen_list_field(shell);
                render::set_notice(shell, refusal);
                return;
            }
            if let Some(state) = shell.object_dialog.as_mut() {
                state.chain_from_list = false;
            }
            arm(shell, true, cx);
        }
        Some(super::Step::Refused(reason)) => render::set_notice(shell, reason),
        None => {}
    }
}

/// Reopen the open draft's chain field, as opened from the list: its text is
/// the draft's chain, and its `enter`/`escape` are the list-opened ones.
fn reopen_list_field(shell: &mut ShellView) {
    render::open_field(shell);
    mark_chain_from_list(shell);
}

/// Cancel the save prompt. One armed from a chain field returns to that
/// field, so the typed chain is never left applied to a tick list whose
/// next tick would commit it.
fn cancel_save(shell: &mut ShellView) {
    let from_field = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.save.as_ref())
        .is_some_and(|save| save.from_field);
    clear_save(shell);
    if from_field {
        reopen_list_field(shell);
    }
}

fn arm(shell: &mut ShellView, from_field: bool, cx: &mut Context<ShellView>) {
    let from_draft = shell
        .object_dialog
        .as_ref()
        .and_then(|state| state.draft.as_ref())
        .map(super::groupings::ticked);
    let chain = match from_draft {
        Some(chain) => {
            // A draft with an error is not a chain a slot may take: the
            // slot's reader would drop it, leaving an active slot that
            // groups by nothing.
            if let Some(refusal) = super::apply::blocking_diagnostic(shell) {
                render::set_notice(shell, refusal);
                return;
            }
            Some(chain)
        }
        None => cursor_kind(shell).and_then(|kind| chain_of(shell, kind, cx)),
    };
    match chain.filter(|chain| !chain.is_empty()) {
        Some(chain) => {
            if let Some(state) = shell.object_dialog.as_mut() {
                state.save = Some(SaveToSlot {
                    chain,
                    from_field,
                    ..SaveToSlot::default()
                });
            }
        }
        None => render::set_notice(shell, NOTHING_TO_SAVE.to_string()),
    }
}

/// Keys while the save prompt is up. It owns every key: a stray letter must
/// not act on the list behind the question.
pub(super) fn handle_save_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(save) = shell.object_dialog.as_ref().and_then(|s| s.save.clone()) else {
        return false;
    };
    if let Some(slot) = save.replace {
        match ConfirmAnswer::from_key(ks) {
            Some(ConfirmAnswer::Yes) => carry_out_save(shell, slot, save.chain, window, cx),
            Some(ConfirmAnswer::No) => cancel_save(shell),
            None => {}
        }
    } else if ks.key == "escape" {
        cancel_save(shell);
    } else if let Some(slot) = slot_digit(ks) {
        choose_slot(shell, slot, window, cx);
    }
    cx.notify();
    true
}

/// A bare `1`…`9`.
fn slot_digit(ks: &Keystroke) -> Option<u8> {
    let &[byte] = ks.key.as_bytes() else {
        return None;
    };
    (ks.mods == Modifiers::NONE && (b'1'..=b'9').contains(&byte)).then(|| byte - b'0')
}

/// A click on one of the prompt's digits. Only while the digits are what
/// the prompt shows: a click that arrives after the y/n question replaced
/// them must not pick another slot behind it.
pub(super) fn press_save_digit(
    shell: &mut ShellView,
    slot: u8,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if shell
        .object_dialog
        .as_ref()
        .is_some_and(|s| s.save.as_ref().is_some_and(|save| save.replace.is_none()))
    {
        choose_slot(shell, slot, window, cx);
        cx.notify();
    }
}

fn clear_save(shell: &mut ShellView) {
    if let Some(state) = shell.object_dialog.as_mut() {
        state.save = None;
    }
}

/// A digit answered the prompt: save now, or ask first when the slot's own
/// user-layer chain would be lost.
fn choose_slot(shell: &mut ShellView, slot: u8, window: &mut Window, cx: &mut Context<ShellView>) {
    let Some(save) = shell.object_dialog.as_ref().and_then(|s| s.save.clone()) else {
        return;
    };
    // Against the pending batch when one is queued: an edit a moment ago
    // may have forked this slot (so a save would lose the user's chain) or
    // changed its chain (so an "equal" one is not), and neither reaches the
    // list's rows or the frame until promotion.
    let (target, held) = match super::apply::config_with_pending(shell) {
        Some(config) => {
            let rows = super::Domain::Groupings.objects(&config);
            let slots = super::super::hot_reload::rebuild_slots(&config);
            let held = slots.get(slot).map(GroupingSlots::label_of);
            (classify(&rows, slots.get(slot), slot, &save.chain), held)
        }
        None => {
            let frame = shell.target_frame();
            let view = frame.read(cx);
            let held = view.slots().get(slot).map(GroupingSlots::label_of);
            let rows = shell
                .object_dialog
                .as_ref()
                .map(|s| s.rows.rows())
                .unwrap_or_default();
            (
                classify(rows, view.slots().get(slot), slot, &save.chain),
                held,
            )
        }
    };
    match target {
        SaveTarget::Owned => {
            if let Some(state) = shell.object_dialog.as_mut()
                && let Some(save) = state.save.as_mut()
            {
                save.replace = Some(slot);
                save.replaced = held.unwrap_or_default();
            }
        }
        SaveTarget::Equal => {
            // Staged even though nothing is written: the slot may be one the
            // pending batch defines and the frame does not hold yet, which
            // `set_active_slot` would refuse. On a slot the frame holds with
            // this chain, staging changes nothing.
            clear_save(shell);
            activate_and_close(shell, slot, Some(save.chain), window, cx);
        }
        SaveTarget::Empty | SaveTarget::Inherited => {
            carry_out_save(shell, slot, save.chain, window, cx)
        }
    }
}

/// Queue the write, stage the slot so it can be activated now, activate it
/// and close. A refused write changes nothing in the frame and says why,
/// with the dialog still open.
fn carry_out_save(
    shell: &mut ShellView,
    slot: u8,
    chain: Vec<String>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    clear_save(shell);
    match super::apply::queue_slot_chain(shell, slot, &chain, cx) {
        Err(refusal) => render::set_notice(shell, refusal),
        Ok(fork) => {
            activate_and_close(shell, slot, Some(chain), window, cx);
            // The dialog has closed, so the fork is announced on the status
            // bar, in the words every fork uses.
            if let Some(fork) = fork {
                shell.notice = Some(fork.into());
            }
        }
    }
}

fn activate_and_close(
    shell: &mut ShellView,
    slot: u8,
    staged: Option<Vec<String>>,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    let frame = shell.target_frame();
    frame.update(cx, |f, cx| {
        if let Some(chain) = staged {
            f.stage_slot(slot, chain);
        }
        f.set_active_slot(Some(slot));
        cx.notify();
    });
    shell.close_modal(window, cx);
}

/// The save prompt, painted where the action bar sits: the chain, nine
/// digit buttons (an empty slot's outlined, so the slots a save loses
/// nothing to stand out), and a line saying what each kind of slot does.
/// While a user-owned slot awaits its answer, the y/n question instead.
pub(super) fn save_prompt(
    state: &super::ObjectDialogState,
    save: &SaveToSlot,
    entity: &Entity<ShellView>,
    cx: &mut App,
) -> AnyElement {
    if let Some(slot) = save.replace {
        let held = save.replaced.clone();
        let on_yes: ConfirmHandler = Rc::new(move |shell, window, cx| {
            let Some(chain) = shell
                .object_dialog
                .as_ref()
                .and_then(|s| s.save.as_ref())
                .filter(|save| save.replace == Some(slot))
                .map(|save| save.chain.clone())
            else {
                return;
            };
            carry_out_save(shell, slot, chain, window, cx);
        });
        let on_no: ConfirmHandler = Rc::new(|shell, _window, _cx| cancel_save(shell));
        return v_flex()
            .debug_selector(|| "objectdialog-save".to_string())
            .child(
                div()
                    .debug_selector(|| "objectdialog-save-replace".to_string())
                    .child(confirm_row(
                        format!("Replace slot {slot} ({held})? Its chain is lost."),
                        "Replace",
                        "objectdialog-save",
                        entity,
                        on_yes,
                        on_no,
                        cx,
                    )),
            )
            .into_any_element();
    }
    let theme = cx.theme();
    let mut digits = h_flex().gap_1().items_center();
    for slot in 1..=9u8 {
        let entity = entity.clone();
        let ks = crate::keymap::parse_keystroke(&slot.to_string(), Modifiers::NONE)
            .expect("a digit is a valid key");
        let empty = state
            .rows
            .rows()
            .iter()
            .find(|row| row.name == slot.to_string())
            .is_none_or(|row| !has_chain(row));
        let button = Button::new(("objectdialog-save-digit", slot as usize))
            .small()
            .child(kbd::chip(&ks))
            .on_click(move |_event, window, cx| {
                entity.update(cx, |shell, cx| {
                    press_save_digit(shell, slot, window, cx);
                    shell.refresh_dialog_rows(cx);
                    sync_dialog_text(shell, window, cx);
                });
            });
        let button = if empty {
            button.outline()
        } else {
            button.ghost()
        };
        digits = digits.child(
            div()
                .debug_selector(move || format!("objectdialog-save-{slot}"))
                .child(button),
        );
    }
    v_flex()
        .gap_1()
        .debug_selector(|| "objectdialog-save".to_string())
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .text_sm()
                .child(format!(
                    "Save {} to slot",
                    GroupingSlots::label_of(&save.chain)
                ))
                .child(digits),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(SAVE_NOTE),
        )
        .into_any_element()
}

/// What each kind of target slot does with a save.
pub const SAVE_NOTE: &str = "an empty slot is filled · a desk or builtin slot is copied to your config · your own slot asks first";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;
    use geode_core::scopes::SavedScopes;

    fn frame() -> Frame {
        let mut slots = GroupingSlots::default();
        slots.set(3, vec!["lhu".into()]);
        Frame::new(slots, SavedScopes::new(), None)
    }

    #[test]
    fn names_resolve_to_their_row_kind() {
        assert_eq!(kind_of("0"), Some(RowKind::ViewDefault));
        assert_eq!(kind_of("*"), Some(RowKind::AdHoc));
        assert_eq!(kind_of("7"), Some(RowKind::Slot(7)));
        assert_eq!(kind_of("10"), None);
        assert_eq!(kind_of("book"), None);
        let g = super::super::Domain::Groupings;
        assert!(is_lead(g, "0") && is_lead(g, "*") && !is_lead(g, "1"));
        assert!(!is_lead(super::super::Domain::Views, "0"));
    }

    #[test]
    fn the_lead_rows_are_the_view_default_then_the_ad_hoc_chain() {
        let mut f = frame();
        let rows = lead_rows(&f.shared());
        assert_eq!(
            rows.iter()
                .map(|r| (r.name.as_str(), r.summary.as_str()))
                .collect::<Vec<_>>(),
            [("0", "view default"), ("*", "no ad hoc chain")]
        );
        assert!(rows.iter().all(|r| r.layer.is_none()));
        assert!(!has_chain(&rows[1]));

        f.shared_mut()
            .set_ad_hoc(vec!["underlying_ref".into(), "expiry".into()]);
        let rows = lead_rows(&f.shared());
        assert_eq!(rows[1].summary, "underlying_ref / expiry");
        assert!(has_chain(&rows[1]));
        assert!(!has_chain(&rows[0]), "the view default has no chain");
    }

    fn row(name: &str, layer: Option<Layer>) -> ObjectRow {
        ObjectRow {
            name: name.to_string(),
            summary: String::new(),
            layer,
            overridden: false,
            drifted: false,
            prefix: None,
        }
    }

    #[test]
    fn a_save_target_is_classified_by_what_saving_would_lose() {
        let rows = [
            row("1", Some(Layer::Builtin)),
            row("2", Some(Layer::User)),
            row("3", None),
        ];
        let lhu = vec!["lhu".to_string()];
        let book = vec!["book".to_string()];
        assert_eq!(classify(&rows, None, 3, &lhu), SaveTarget::Empty);
        assert_eq!(
            classify(&rows, Some(&book), 1, &lhu),
            SaveTarget::Inherited,
            "a fork is recoverable with r, so it never asks"
        );
        assert_eq!(
            classify(&rows, Some(&book), 2, &lhu),
            SaveTarget::Owned,
            "a user-owned chain would be lost"
        );
        assert_eq!(classify(&rows, Some(&lhu), 2, &lhu), SaveTarget::Equal);
        assert_eq!(classify(&rows, Some(&lhu), 1, &lhu), SaveTarget::Equal);
    }

    #[test]
    fn the_active_row_names_the_lanes_choice() {
        assert_eq!(active_row(GroupingChoice::ViewDefault), "0");
        assert_eq!(active_row(GroupingChoice::AdHoc), "*");
        assert_eq!(active_row(GroupingChoice::Slot(3)), "3");
    }
}
