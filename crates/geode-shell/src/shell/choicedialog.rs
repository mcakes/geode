//! The choice dialog (2026-09-19): one filter-first modal in
//! [`asof_view`](super::asof_view)'s mould for "pick one of these and
//! act", with a [`Target`] saying what the rows are and what a pick
//! does. Three targets so far:
//!
//! - [`Target::Grouping`] — the frame's grouping slots: the mouse and
//!   typeahead form of `ctrl+1..9`/`ctrl+0`, opened by `frame::grouping`
//!   (`mod+g`, the palette's "Pick a grouping…") and by a click on the
//!   toolbar's grouping readout (the `"1 · book / lhu"` / `"view
//!   default"` text, `toolbar::toolbar`'s `on_grouping`).
//! - [`Target::TileKind`] — the roster's module kinds: the choosing form
//!   of the palette's `<Kind>: Split` rows, opened by `tile::add`
//!   (`mod+n`, "Add a tile…") and by a bare double-click on a placeholder
//!   tile (`ShellView::try_pick_tile_on_double_click`), where the pick
//!   fills that placeholder in place through `add_tile`.
//! - [`Target::LogLevel`] — `Set log level…` (command-line locality spec
//!   §4.2), two steps over the one dialog: step 1's rows are the seven
//!   `geode::` targets with their effective level, a pick replaces the
//!   rows in place with step 2's five levels, and a pick there lands on
//!   `Diagnostics::request_level` — the path `:level` used to take.
//!   `escape` on step 2 returns to step 1 rather than closing.
//!
//! ## Architecture
//!
//! [`ChoiceDialogState`] is pure (no `gpui`), stored on `ShellView` as
//! `choice_dialog: Option<ChoiceDialogState>`, exactly like
//! `picker`/`as_of_dialog`: a [`ChoiceList`] over the option rows plus
//! the target. Ranking, the highlight, `tab` completion and `enter` all
//! go through the one choice core (`choice::route` is the key table);
//! the grouping target adds the digit jump — `1`–`9` on an EMPTY field
//! activate that slot outright, `0` the view default, mirroring the
//! chords — and each target its own commit.
//!
//! Only FILLED slots are grouping rows, plus "view default" first:
//! `Frame::set_active_slot` ignores an empty slot, and a row that
//! visibly does nothing is the defect the scope bar's `save` chip was
//! withdrawn for (`ScopeBarModel::savable`). A trader who wants a slot
//! that is not configured edits groupings (`config::edit_groupings`),
//! deliberately not offered here (user ruling 2026-09-19).
//!
//! [`open`] is the only entry point and the only place a
//! `ChoiceDialogState` is constructed — nothing survives a close/reopen,
//! the same contract every other modal here keeps.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Focusable as _, Window, div};
use gpui_component::{ActiveTheme as _, v_flex};

use geode_core::groupings::GroupingSlots;
use geode_core::log::{Level, LogLevels, TARGETS};

use crate::choice::{self, ChoiceKey, ChoiceList};
use crate::defaults::{AddPlacement, capitalize};
use crate::keymap::{Keystroke, Modifiers};
use crate::module::placeholder::PLACEHOLDER_KIND;

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

/// What the rows stand for and what a pick does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The slot each DECLARED option (an index into `list.options()`)
    /// activates: `None` for the view default.
    Grouping { slots: Vec<Option<u8>> },
    /// The module kind each declared option adds (`add_tile`).
    TileKind { kinds: Vec<String> },
    /// `Set log level…` (command-line locality spec §4.2), two steps over
    /// one dialog: `chosen` is `None` while the rows are targets and
    /// `Some(target)` while they are levels.
    LogLevel {
        targets: Vec<String>,
        chosen: Option<String>,
    },
}

/// The level rows, in severity order, as `[log]` spells them.
pub const LEVEL_WORDS: [(&str, Level); 5] = [
    ("error", Level::ERROR),
    ("warn", Level::WARN),
    ("info", Level::INFO),
    ("debug", Level::DEBUG),
    ("trace", Level::TRACE),
];

fn level_word(level: Level) -> &'static str {
    LEVEL_WORDS
        .iter()
        .find(|(_, l)| *l == level)
        .map(|(w, _)| *w)
        .unwrap_or("info")
}

/// A target's effective level: its own entry, else the default.
fn effective_level(levels: &LogLevels, target: &str) -> Level {
    levels
        .targets
        .iter()
        .find(|(t, _)| t == target)
        .map(|(_, l)| *l)
        .unwrap_or(levels.default)
}

/// Persistent state for one open choice-dialog session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceDialogState {
    /// The ranked rows, spelled as the surface a pick lands on spells
    /// them: a grouping row is `"{n} · {label}"`, the toolbar readout's
    /// own text, so the row a trader picks reads exactly as the bar will
    /// afterwards; a tile row is the palette's `<Kind>` title.
    pub list: ChoiceList,
    pub target: Target,
}

impl ChoiceDialogState {
    /// The grouping rows for `slots`, the highlight placed on `active`
    /// (the frame's current slot — `None` lights the view-default row) so
    /// `enter` on an untouched picker changes nothing, like every other
    /// choice surface.
    pub fn grouping(slots: &GroupingSlots, active: Option<u8>) -> Self {
        let (options, targets) = grouping_rows(slots);
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        let current = targets.iter().position(|t| *t == active);
        let text = current.map(|ix| list.options()[ix].clone());
        list.place(text.as_deref());
        Self {
            list,
            target: Target::Grouping { slots: targets },
        }
    }

    /// The tile rows for the roster's `kinds`, in roster order, the
    /// placeholder left out (it is what a pick REPLACES, never something
    /// to add), the highlight on the first.
    pub fn tile_kinds<'a>(kinds: impl IntoIterator<Item = &'a str>) -> Self {
        let kinds: Vec<String> = kinds
            .into_iter()
            .filter(|k| *k != PLACEHOLDER_KIND)
            .map(str::to_string)
            .collect();
        let options = kinds.iter().map(|k| capitalize(k)).collect();
        Self {
            list: ChoiceList::new(options, choice::DEFAULT_CAP),
            target: Target::TileKind { kinds },
        }
    }

    /// Step 1 of `Set log level…`: one row per `geode::` target suffix,
    /// `"{target} · {level}"`, the highlight on the first.
    pub fn log_targets(levels: &LogLevels) -> Self {
        let targets: Vec<String> = TARGETS
            .iter()
            .map(|t| t.strip_prefix("geode::").unwrap_or(t).to_string())
            .collect();
        let options = targets
            .iter()
            .map(|t| format!("{t} · {}", level_word(effective_level(levels, t))))
            .collect();
        Self {
            list: ChoiceList::new(options, choice::DEFAULT_CAP),
            target: Target::LogLevel {
                targets,
                chosen: None,
            },
        }
    }

    /// Step 2: the five levels, the highlight placed on `current` so a
    /// bare `enter` changes nothing.
    pub fn log_levels(target: String, current: Level) -> Self {
        let options: Vec<String> = LEVEL_WORDS.iter().map(|(w, _)| (*w).to_string()).collect();
        let mut list = ChoiceList::new(options, choice::DEFAULT_CAP);
        list.place(Some(level_word(current)));
        Self {
            list,
            target: Target::LogLevel {
                targets: Vec::new(),
                chosen: Some(target),
            },
        }
    }

    /// The pick the highlighted row stands for, or `None` with nothing
    /// highlighted (every row filtered out).
    pub fn highlighted_pick(&self) -> Option<Pick> {
        self.list.pick().map(|ix| self.pick_at(ix))
    }

    /// The pick a RANKED row (a click's index, `dialog::choice_rows`'s
    /// own positions) stands for.
    pub fn pick_at_ranked(&self, ranked: usize) -> Option<Pick> {
        self.list.ranked().get(ranked).map(|r| self.pick_at(r.row))
    }

    fn pick_at(&self, declared: usize) -> Pick {
        match &self.target {
            Target::Grouping { slots } => Pick::Slot(slots[declared]),
            Target::TileKind { kinds } => Pick::Kind(kinds[declared].clone()),
            Target::LogLevel { targets, chosen } => match chosen {
                None => Pick::LogTarget(targets[declared].clone()),
                Some(target) => Pick::LogLevel(target.clone(), LEVEL_WORDS[declared].1),
            },
        }
    }

    /// A digit typed into an EMPTY field on the grouping target — `1`–`9`
    /// the slot of that number if it is filled, `0` the view default —
    /// mirroring `ctrl+N`. `None` for an unfilled slot (the chord ignores
    /// it too), a non-digit, or any other target.
    pub fn jump(&self, key: &str) -> Option<Option<u8>> {
        let Target::Grouping { slots } = &self.target else {
            return None;
        };
        let digit = key.parse::<u8>().ok().filter(|d| *d <= 9)?;
        if digit == 0 {
            return Some(None);
        }
        slots.iter().find(|s| **s == Some(digit)).copied()
    }

    /// The grouping-target convenience the tests read.
    pub fn highlighted_slot(&self) -> Option<Option<u8>> {
        match self.highlighted_pick()? {
            Pick::Slot(slot) => Some(slot),
            Pick::Kind(_) | Pick::LogTarget(_) | Pick::LogLevel(..) => None,
        }
    }
}

/// What one row commits. Owned (a `String` kind), not borrowed from the
/// dialog state: a pick is a one-off event whose commit drops that state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// `Frame::set_active_slot`; `None` is the view default.
    Slot(Option<u8>),
    /// `ShellView::add_tile` of this kind.
    Kind(String),
    /// Step 1 of `Set log level…`: replace the rows with the levels.
    LogTarget(String),
    /// Step 2: `Diagnostics::request_level`.
    LogLevel(String, Level),
}

/// The grouping option texts and their slots, in row order: the view
/// default, then slots 1–9 that are filled.
pub fn grouping_rows(slots: &GroupingSlots) -> (Vec<String>, Vec<Option<u8>>) {
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

const GROUPING_HINTS: &[Hint] = &[
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

const TILE_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("add ·"),
    Hint::Key("escape"),
    Hint::Text("close"),
];

const LOG_HINTS: &[Hint] = &[
    Hint::Text("type to filter ·"),
    Hint::Key("up"),
    Hint::Key("down"),
    Hint::Text("move ·"),
    Hint::Key("enter"),
    Hint::Text("choose ·"),
    Hint::Key("escape"),
    Hint::Text("back / close"),
];

/// The per-target chrome: the modal's title, the selector prefix
/// (`{prefix}-choice-list`, `{prefix}-choice-{text}`, `{prefix}-hints`)
/// and the footer.
fn chrome(target: &Target) -> (&'static str, &'static str, &'static str, &'static [Hint]) {
    match target {
        Target::Grouping { .. } => ("Grouping", "grouping", "grouping-hints", GROUPING_HINTS),
        Target::TileKind { .. } => ("Add a tile", "tile", "tile-hints", TILE_HINTS),
        Target::LogLevel { .. } => ("Log level", "loglevel", "loglevel-hints", LOG_HINTS),
    }
}

/// Open on the frame's grouping slots — `frame::grouping` and the
/// toolbar readout's click.
pub fn open_grouping(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = {
        let frame = view.frame.read(cx);
        ChoiceDialogState::grouping(frame.slots(), frame.active_slot())
    };
    open(view, state, window, cx);
}

/// Open on the roster's kinds — `tile::add` and a placeholder's
/// double-click. The pick lands wherever `add_tile` puts a tile for the
/// focused tile at COMMIT time: a focused placeholder is filled in place,
/// a real tile is split in the `add` setting's direction.
pub fn open_tile_kinds(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = ChoiceDialogState::tile_kinds(view.services.roster.kinds());
    open(view, state, window, cx);
}

/// Open `Set log level…` on the target step (`log::level`, palette-only).
pub fn open_log_level(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    let state = ChoiceDialogState::log_targets(&view.diagnostics.read(cx).levels);
    open(view, state, window, cx);
}

fn open(
    view: &mut ShellView,
    state: ChoiceDialogState,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) {
    if view.modal.is_some() {
        return;
    }
    let (title, ..) = chrome(&state.target);
    view.choice_dialog_scroll
        .scroll_to_item(state.list.ranked_highlighted());
    view.choice_dialog = Some(state);
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        title,
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

/// Act on `pick` and close — the enter arm's, the digit jump's and a row
/// click's one commit path. A slot goes through `Frame::set_active_slot`,
/// the same door `frame::slot_N`/`frame::slot_clear` take; a kind through
/// `ShellView::add_tile` with the setting-resolved split, the palette's
/// `<Kind>: Split` row's own placement — which fills a focused
/// placeholder in place. The modal closes FIRST for a kind: `add_tile`
/// records the request for `ensure_occupants` on the next render, and
/// the close's own focus return must not land after that.
fn commit(shell: &mut ShellView, pick: Pick, window: &mut Window, cx: &mut Context<ShellView>) {
    match pick {
        Pick::Slot(slot) => {
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
        Pick::Kind(kind) => {
            shell.close_modal(window, cx);
            shell.add_tile(&kind, AddPlacement::Split(None), None, window, cx);
        }
        Pick::LogTarget(target) => {
            // Step 2 replaces the rows in place; the modal stays open and
            // the field is reset (`set_value` emits no `Change`, and the
            // new list starts with an empty query).
            let current = effective_level(&shell.diagnostics.read(cx).levels, &target);
            let state = ChoiceDialogState::log_levels(target, current);
            shell
                .choice_dialog_scroll
                .scroll_to_item(state.list.ranked_highlighted());
            shell.choice_dialog = Some(state);
            let input = shell.dialog_input.clone();
            input.update(cx, |i, cx| i.set_value("", window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
            cx.notify();
        }
        Pick::LogLevel(target, level) => {
            shell.diagnostics.update(cx, |d, cx| {
                d.request_level(&target, level);
                cx.notify();
            });
            shell.close_modal(window, cx);
        }
    }
}

/// The [`dialog::ModalKeyHandler`] for this modal: [`choice::route`]'s
/// table, plus the grouping target's digit jump on an empty field.
/// `escape` claims nothing (falls through to `handle_key_down`'s
/// modal-closes-on-escape branch), like every other dialog here.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    match choice::route(ks) {
        Some(ChoiceKey::Cancel) => {
            // The level step goes BACK to the target step; every other
            // dialog (and the target step itself) falls through to
            // `handle_key_down`'s modal-closes-on-escape branch.
            if matches!(
                shell.choice_dialog.as_ref().map(|s| &s.target),
                Some(Target::LogLevel {
                    chosen: Some(_),
                    ..
                })
            ) {
                let state = ChoiceDialogState::log_targets(&shell.diagnostics.read(cx).levels);
                shell
                    .choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
                shell.choice_dialog = Some(state);
                let input = shell.dialog_input.clone();
                input.update(cx, |i, cx| i.set_value("", window, cx));
                input.read(cx).focus_handle(cx).focus(window, cx);
                cx.notify();
                return true;
            }
            return false;
        }
        Some(ChoiceKey::Pick) => {
            // The field's live text may never have reached the list
            // through a `Change` event (`set_value` emits none): re-feed
            // it before trusting the highlight.
            let live = shell.dialog_input.read(cx).value().to_string();
            let pick = shell.choice_dialog.as_mut().and_then(|state| {
                state.list.set_query(&live);
                state.highlighted_pick()
            });
            // Nothing lit (every row filtered out): the picker stays open
            // and the empty list says it.
            if let Some(pick) = pick {
                commit(shell, pick, window, cx);
            }
            return true;
        }
        Some(ChoiceKey::Complete) => {
            // A filter-only dialog keeps its own field (spec §16.4 —
            // `sync_dialog_text` serves the mode-carrying dialogs), so
            // the completed text is written here, the way the as-of
            // dialog's calendar writes its date: `set_value` emits no
            // `Change`, and the list already holds the new query.
            let text = shell.choice_dialog.as_mut().and_then(|state| {
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
            if let Some(state) = shell.choice_dialog.as_ref() {
                shell
                    .choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
            }
            cx.notify();
            return true;
        }
        Some(ChoiceKey::Nav(cmd)) => {
            if let Some(state) = shell.choice_dialog.as_mut() {
                state.list.nav(cmd);
                shell
                    .choice_dialog_scroll
                    .scroll_to_item(state.list.ranked_highlighted());
            }
            cx.notify();
            return true;
        }
        None => {}
    }
    // The digit jump (grouping target only — `jump` answers `None` for
    // the rest, whose digits then type): only on an empty field, so a
    // digit typed as part of a filter (`"1 · book"`) still reaches the
    // input. `text()` is the borrowed rope (its `len` is bytes), not
    // `value()`'s fresh copy — this runs per keystroke. An unfilled
    // slot's digit is claimed and dropped, as its chord is ignored:
    // letting it type would put a lone `2` in the field that matches no
    // row, a worse answer than nothing.
    if ks.mods == Modifiers::NONE
        && is_digit(&ks.key)
        && shell.dialog_input.read(cx).text().len() == 0
        && matches!(
            shell.choice_dialog.as_ref().map(|s| &s.target),
            Some(Target::Grouping { .. })
        )
    {
        let slot = shell
            .choice_dialog
            .as_ref()
            .and_then(|state| state.jump(&ks.key));
        if let Some(slot) = slot {
            commit(shell, Pick::Slot(slot), window, cx);
        }
        return true;
    }
    false
}

fn is_digit(key: &str) -> bool {
    key.len() == 1 && key.as_bytes()[0].is_ascii_digit()
}

/// The body: the shared filter field, the ranked rows through
/// `dialog::choice_rows` (a row click commits — a pick, as in the
/// market-data underlying picker, not the dialogs' `tab`), and the hint
/// line.
fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.choice_dialog.as_ref() else {
        return div().into_any_element();
    };
    let (_, prefix, hints_selector, hints) = chrome(&state.target);
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let click_entity = entity.clone();
    let rows = dialog::choice_rows(
        &state.list,
        prefix,
        &shell.choice_dialog_scroll,
        theme,
        move |ranked, window, cx| {
            click_entity.update(cx, |shell, cx| {
                let pick = shell
                    .choice_dialog
                    .as_ref()
                    .and_then(|state| state.pick_at_ranked(ranked));
                if let Some(pick) = pick {
                    commit(shell, pick, window, cx);
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
            hints,
            hints_selector,
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
        let (options, targets) = grouping_rows(&slots());
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
        let state = ChoiceDialogState::grouping(&slots(), Some(3));
        assert_eq!(state.highlighted_slot(), Some(Some(3)));
        let state = ChoiceDialogState::grouping(&slots(), None);
        assert_eq!(state.highlighted_slot(), Some(None));
    }

    /// Typing narrows the rows and `enter` picks the highlighted one;
    /// a query matching nothing leaves nothing to pick.
    #[test]
    fn typing_narrows_and_the_highlight_names_a_slot() {
        let mut state = ChoiceDialogState::grouping(&slots(), None);
        state.list.set_query("under");
        assert_eq!(state.highlighted_slot(), Some(Some(3)));
        state.list.set_query("zzz");
        assert_eq!(state.highlighted_slot(), None);
    }

    /// A digit jumps to that slot when filled, `0` to the view default,
    /// and an unfilled slot's digit does nothing — the chords' own rule.
    #[test]
    fn a_digit_jumps_to_a_filled_slot_or_the_view_default() {
        let state = ChoiceDialogState::grouping(&slots(), None);
        assert_eq!(state.jump("3"), Some(Some(3)));
        assert_eq!(state.jump("0"), Some(None));
        assert_eq!(state.jump("2"), None, "an empty slot is not a target");
        assert_eq!(state.jump("j"), None);
    }

    /// The ranked index a click hands back resolves through the RANKED
    /// list, not the declared one — after a filter the two differ.
    #[test]
    fn a_click_resolves_through_the_ranked_order() {
        let mut state = ChoiceDialogState::grouping(&slots(), None);
        state.list.set_query("under");
        assert_eq!(state.pick_at_ranked(0), Some(Pick::Slot(Some(3))));
        assert_eq!(state.pick_at_ranked(1), None);
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

    /// Tile rows are the roster's kinds in roster order, titled as the
    /// palette titles them, with the placeholder left out; a digit on
    /// this target is not a jump.
    #[test]
    fn tile_rows_are_the_roster_kinds_titled_minus_the_placeholder() {
        let state =
            ChoiceDialogState::tile_kinds(["blotter", PLACEHOLDER_KIND, "cvi", "diagnostics"]);
        assert_eq!(state.list.options(), ["Blotter", "Cvi", "Diagnostics"]);
        assert_eq!(state.highlighted_pick(), Some(Pick::Kind("blotter".into())));
        assert_eq!(state.jump("1"), None, "digits type on the tile target");
        let mut state = state;
        state.list.set_query("diag");
        assert_eq!(
            state.pick_at_ranked(0),
            Some(Pick::Kind("diagnostics".into()))
        );
    }

    /// Step 1 rows are the seven `geode::` suffixes with each one's
    /// effective level; step 2 rows are the five levels with the current
    /// one lit.
    #[test]
    fn log_level_rows_name_targets_then_levels() {
        let levels = geode_core::log::LogLevels {
            default: geode_core::log::Level::INFO,
            targets: vec![("ingest".into(), geode_core::log::Level::DEBUG)],
        };
        let state = ChoiceDialogState::log_targets(&levels);
        assert_eq!(state.list.options()[0], "ingest · debug");
        assert_eq!(state.list.options()[1], "query · info");
        assert_eq!(state.list.options().len(), geode_core::log::TARGETS.len());
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::LogTarget("ingest".into()))
        );
        assert_eq!(state.jump("1"), None, "digits type on this target");

        let mut state =
            ChoiceDialogState::log_levels("ingest".into(), geode_core::log::Level::DEBUG);
        assert_eq!(
            state.list.options(),
            ["error", "warn", "info", "debug", "trace"]
        );
        assert_eq!(
            state.highlighted_pick(),
            Some(Pick::LogLevel(
                "ingest".into(),
                geode_core::log::Level::DEBUG
            )),
            "opens on the current level"
        );
        state.list.set_query("tr");
        assert_eq!(
            state.pick_at_ranked(0),
            Some(Pick::LogLevel(
                "ingest".into(),
                geode_core::log::Level::TRACE
            ))
        );
    }
}
