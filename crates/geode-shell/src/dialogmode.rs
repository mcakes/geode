//! Shared normal/filter modes for modal dialogs, independent of GPUI.
//!
//! Normal mode routes bare letters to dialog commands. Entering filter mode
//! snapshots the current query and routes typing to the shared Input. Escape
//! restores that snapshot; bare Enter keeps the edited query. Either exit returns
//! to normal mode without acting on the selected row.
//!
//! Filter-only surfaces, including the palette, dimension picker, and as-of
//! selector, own their input state separately and do not use this mode enum.

use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav::NavCommand;

/// Shift without Control, Alt, or Command.
const SHIFT: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: true,
    cmd: false,
};

/// Mode of a dialog with both commands and a text filter.
/// Filter-only surfaces manage their own state and escape behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Normal,
    Filter,
}

/// First applicable escape transition: leave filter, clear query, leave stage, close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeStep {
    /// Return to normal mode and restore the entry query through
    /// [`FilterExit::Revert`]. Bare Enter instead keeps the edited query.
    LeaveFilter,
    ClearQuery,
    PreviousStage,
    Close,
}

/// Choose the first applicable transition. The caller supplies whether a
/// previous stage exists; this helper neither changes state nor moves focus.
pub fn escape_step(mode: DialogMode, query_is_empty: bool, has_previous_stage: bool) -> EscapeStep {
    match mode {
        DialogMode::Filter => EscapeStep::LeaveFilter,
        DialogMode::Normal if !query_is_empty => EscapeStep::ClearQuery,
        DialogMode::Normal if has_previous_stage => EscapeStep::PreviousStage,
        DialogMode::Normal => EscapeStep::Close,
    }
}

/// Query policy when leaving filter mode. Both choices return to normal mode
/// without activating the selected row; committing that row is a separate command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterExit {
    /// Restore the query captured when this filter session began.
    Revert,
    /// Retain the edited query while returning bare letters to command handling.
    Keep,
}

/// Recognize Escape with any modifiers as revert, and bare Enter as keep.
/// Other keystrokes return `None` for the caller to route. This helper does not
/// check the current mode; callers use it only while filtering.
pub fn filter_exit(ks: &Keystroke) -> Option<FilterExit> {
    if ks.key == "escape" {
        return Some(FilterExit::Revert);
    }
    if ks.mods == Modifiers::NONE && ks.key == "enter" {
        return Some(FilterExit::Keep);
    }
    None
}

/// Snapshot the current query and enter filter mode. Each call replaces the
/// snapshot, so callers invoke this on an actual entry transition, including
/// mouse entry, rather than while already editing the filter.
pub fn enter_filter(mode: &mut DialogMode, entry: &mut String, query: &str) {
    entry.clear();
    entry.push_str(query);
    *mode = DialogMode::Filter;
}

/// Set normal mode, restoring the entry query for revert or retaining it for
/// keep. Return true only when revert changes the query text; a mode change alone
/// returns false. The caller synchronizes Input/focus and, when text changed,
/// resets selection and scrolling to match the restored result list.
pub fn exit_filter(
    mode: &mut DialogMode,
    entry: &str,
    query: &mut String,
    exit: FilterExit,
) -> bool {
    *mode = DialogMode::Normal;
    match exit {
        FilterExit::Keep => false,
        FilterExit::Revert if query == entry => false,
        FilterExit::Revert => {
            query.clear();
            query.push_str(entry);
            true
        }
    }
}

/// Keyboard owner for a dialog mode and key-capture state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    /// The shared filter `Input`: printable keys are text.
    Input,
    /// The shell root: printable keys reach the dialog's `on_key` as verbs
    /// (normal mode) or as raw captured keystrokes (listening).
    Shell,
}

/// Select the focus owner used by `dialog::sync_dialog_text`. Key capture
/// wins over filter mode so printable keystrokes reach the capture handler
/// instead of becoming Input text. This helper does not move focus itself.
pub fn focus_target(mode: DialogMode, listening: bool) -> FocusTarget {
    if listening {
        return FocusTarget::Shell;
    }
    match mode {
        DialogMode::Filter => FocusTarget::Input,
        DialogMode::Normal => FocusTarget::Shell,
    }
}

/// What a keystroke means in normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalCommand {
    /// Navigation shared with filter mode.
    Nav(NavCommand),
    EnterFilter,
    Commit,
    Toggle,
    /// Step the selected value backward with `h`, Shift-Space, or Shift-Tab.
    ToggleBack,
    /// Move the selected item down/up with Shift-J/Shift-K without moving only selection.
    MoveItem(i32),
    EditText,
    /// A bare digit 1–9 for numbered objects, such as grouping slots.
    /// Zero and modified digits are not numbered-object commands.
    Digit(u8),
    /// A bare letter the vocabulary does not claim — the surface's own
    /// verb (`s`, `d`, `r`, `n`) — or the one shifted verb, `shift+r`,
    /// spelled as the uppercase letter (`Verb('R')`).
    Verb(char),
}

/// Map one keystroke to its normal-mode meaning, or `None` when the
/// surface should handle it itself (`escape`, which belongs to the
/// ladder) or ignore it.
pub fn normal_command(ks: &Keystroke) -> Option<NormalCommand> {
    // Shared navigation first, so `ctrl+d` and the arrows mean the same
    // thing here as they do with the filter focused.
    if let Some(cmd) = listfilter::nav_command(ks) {
        return Some(NormalCommand::Nav(cmd));
    }
    if ks.mods == SHIFT {
        return match ks.key.as_str() {
            "j" => Some(NormalCommand::MoveItem(1)),
            "k" => Some(NormalCommand::MoveItem(-1)),
            "g" => Some(NormalCommand::Nav(NavCommand::Bottom)),
            // Shift-Tab and Shift-Space share backward value stepping.
            "space" | "tab" => Some(NormalCommand::ToggleBack),
            // Represent Shift-R as the uppercase verb for reset-all.
            // Surfaces without that verb can refuse it like other unclaimed verbs.
            "r" => Some(NormalCommand::Verb('R')),
            _ => None,
        };
    }
    if ks.mods != Modifiers::NONE {
        return None;
    }
    match ks.key.as_str() {
        "j" => Some(NormalCommand::Nav(NavCommand::Move(1))),
        "k" => Some(NormalCommand::Nav(NavCommand::Move(-1))),
        "g" => Some(NormalCommand::Nav(NavCommand::Top)),
        "/" => Some(NormalCommand::EnterFilter),
        "enter" => Some(NormalCommand::Commit),
        // Share value-step aliases across dialogs. Bare h/l are commands,
        // so they do not fall through to surface-specific letter verbs.
        "space" | "l" | "tab" => Some(NormalCommand::Toggle),
        "h" => Some(NormalCommand::ToggleBack),
        "i" => Some(NormalCommand::EditText),
        // `escape` is the ladder's, never a verb — returning it here
        // would swallow the one key every dialog needs.
        "escape" => None,
        key => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => Some(NormalCommand::Verb(c)),
                (Some(c @ '1'..='9'), None) => Some(NormalCommand::Digit(c as u8 - b'0')),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Keystroke, Modifiers};
    use crate::vimnav::NavCommand;

    const SHIFT: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: true,
        cmd: false,
    };

    fn ks(key: &str, mods: Modifiers) -> Keystroke {
        Keystroke {
            mods,
            key: key.to_string(),
        }
    }
    fn bare(key: &str) -> Keystroke {
        ks(key, Modifiers::NONE)
    }

    /// Escape chooses one transition at a time, in the documented priority order.
    #[test]
    fn the_escape_ladder_takes_the_first_step_that_applies() {
        use EscapeStep::*;
        // Filter mode always leaves filter first, whatever else is true.
        assert_eq!(escape_step(DialogMode::Filter, true, true), LeaveFilter);
        assert_eq!(escape_step(DialogMode::Filter, false, false), LeaveFilter);
        // Then a non-empty query clears, before any stage is left.
        assert_eq!(escape_step(DialogMode::Normal, false, true), ClearQuery);
        // Then a nested stage.
        assert_eq!(escape_step(DialogMode::Normal, true, true), PreviousStage);
        // Then, and only then, the modal closes.
        assert_eq!(escape_step(DialogMode::Normal, true, false), Close);
    }

    /// Escape reverts regardless of modifiers; only unmodified Enter keeps the query.
    #[test]
    fn escape_reverts_the_filter_and_enter_keeps_it() {
        assert_eq!(filter_exit(&bare("escape")), Some(FilterExit::Revert));
        assert_eq!(filter_exit(&ks("escape", SHIFT)), Some(FilterExit::Revert));
        assert_eq!(filter_exit(&bare("enter")), Some(FilterExit::Keep));
        assert_eq!(filter_exit(&ks("enter", Modifiers::CTRL)), None);
        // Everything else is the filter `Input`'s, not this table's.
        assert_eq!(filter_exit(&bare("j")), None);
        assert_eq!(filter_exit(&bare("/")), None);
    }

    /// Reverting changed text reports true so callers can reset selection and scroll.
    #[test]
    fn leaving_by_escape_restores_the_query_entry_recorded() {
        let mut mode = DialogMode::Normal;
        let mut entry = String::new();
        let mut query = "vol".to_string();
        enter_filter(&mut mode, &mut entry, &query);
        assert_eq!(mode, DialogMode::Filter);
        query.push_str("atility");
        assert!(exit_filter(
            &mut mode,
            &entry,
            &mut query,
            FilterExit::Revert
        ));
        assert_eq!(mode, DialogMode::Normal);
        assert_eq!(query, "vol");
    }

    /// Keeping a filter returns to normal mode without changing the query.
    #[test]
    fn leaving_by_enter_keeps_what_filter_mode_typed() {
        let mut mode = DialogMode::Normal;
        let mut entry = String::new();
        let mut query = String::new();
        enter_filter(&mut mode, &mut entry, &query);
        query.push_str("delta");
        assert!(!exit_filter(
            &mut mode,
            &entry,
            &mut query,
            FilterExit::Keep
        ));
        assert_eq!(mode, DialogMode::Normal);
        assert_eq!(query, "delta");
    }

    /// Reverting identical text reports no change so callers can retain the cursor.
    #[test]
    fn an_escape_with_nothing_typed_reports_no_change() {
        let mut mode = DialogMode::Normal;
        let mut entry = String::new();
        let mut query = "gamma".to_string();
        enter_filter(&mut mode, &mut entry, &query);
        assert!(!exit_filter(
            &mut mode,
            &entry,
            &mut query,
            FilterExit::Revert
        ));
        assert_eq!(query, "gamma");
    }

    /// Each filter entry snapshots the query retained by the previous session.
    #[test]
    fn each_entry_into_filter_mode_takes_its_own_snapshot() {
        let mut mode = DialogMode::Normal;
        let mut entry = String::new();
        let mut query = String::new();
        enter_filter(&mut mode, &mut entry, &query);
        query.push_str("vega");
        exit_filter(&mut mode, &entry, &mut query, FilterExit::Keep);
        enter_filter(&mut mode, &mut entry, &query);
        query.push_str("-hedge");
        exit_filter(&mut mode, &entry, &mut query, FilterExit::Revert);
        assert_eq!(query, "vega");
    }

    #[test]
    fn normal_mode_maps_the_shared_vocabulary() {
        use NormalCommand::*;
        assert_eq!(normal_command(&bare("j")), Some(Nav(NavCommand::Move(1))));
        assert_eq!(normal_command(&bare("k")), Some(Nav(NavCommand::Move(-1))));
        assert_eq!(normal_command(&bare("g")), Some(Nav(NavCommand::Top)));
        assert_eq!(
            normal_command(&ks("g", SHIFT)),
            Some(Nav(NavCommand::Bottom))
        );
        assert_eq!(normal_command(&bare("/")), Some(EnterFilter));
        assert_eq!(normal_command(&bare("enter")), Some(Commit));
        assert_eq!(normal_command(&bare("space")), Some(Toggle));
        assert_eq!(normal_command(&bare("i")), Some(EditText));
        assert_eq!(normal_command(&ks("j", SHIFT)), Some(MoveItem(1)));
        assert_eq!(normal_command(&ks("k", SHIFT)), Some(MoveItem(-1)));
    }

    /// Shift-Space steps values backward while Shift-J/Shift-K move items.
    #[test]
    fn shift_space_steps_a_value_backward() {
        assert_eq!(
            normal_command(&ks("space", SHIFT)),
            Some(NormalCommand::ToggleBack)
        );
        // Forward stepping and item movement remain distinct commands.
        assert_eq!(normal_command(&bare("space")), Some(NormalCommand::Toggle));
        assert_eq!(
            normal_command(&ks("j", SHIFT)),
            Some(NormalCommand::MoveItem(1))
        );
    }

    /// All value-step aliases share one table. Modified h/l return no command
    /// so they cannot become accidental value steps or surface verbs.
    #[test]
    fn tab_and_h_and_l_step_a_value_beside_space() {
        use NormalCommand::*;
        assert_eq!(normal_command(&bare("l")), Some(Toggle));
        assert_eq!(normal_command(&bare("tab")), Some(Toggle));
        assert_eq!(normal_command(&bare("h")), Some(ToggleBack));
        assert_eq!(normal_command(&ks("tab", SHIFT)), Some(ToggleBack));
        // Space and Shift-Space use the same forward/backward commands.
        assert_eq!(normal_command(&bare("space")), Some(Toggle));
        assert_eq!(normal_command(&ks("space", SHIFT)), Some(ToggleBack));
        // Modified h/l are neither value steps nor surface verbs.
        for mods in [Modifiers::CTRL, Modifiers::ALT, Modifiers::CMD, SHIFT] {
            assert_eq!(normal_command(&ks("h", mods)), None, "{mods:?}");
            assert_eq!(normal_command(&ks("l", mods)), None, "{mods:?}");
        }
    }

    /// Normal mode shares arrows and Control navigation with the filter.
    #[test]
    fn normal_mode_still_honours_the_filter_modes_navigation() {
        use NormalCommand::*;
        assert_eq!(
            normal_command(&bare("down")),
            Some(Nav(NavCommand::Move(1)))
        );
        assert_eq!(
            normal_command(&ks("d", Modifiers::CTRL)),
            Some(Nav(NavCommand::Move(5)))
        );
        assert_eq!(
            normal_command(&ks("u", Modifiers::CTRL)),
            Some(Nav(NavCommand::Move(-5)))
        );
    }

    /// A bare letter with no fixed meaning is the surface's own verb —
    /// the whole point of normal mode. `escape` is NOT a verb: it is the
    /// ladder's, and returning `Verb('escape')` would swallow it.
    #[test]
    fn unclaimed_letters_become_surface_verbs_but_escape_does_not() {
        assert_eq!(normal_command(&bare("s")), Some(NormalCommand::Verb('s')));
        assert_eq!(normal_command(&bare("d")), Some(NormalCommand::Verb('d')));
        assert_eq!(normal_command(&bare("r")), Some(NormalCommand::Verb('r')));
        assert_eq!(
            normal_command(&ks("r", SHIFT)),
            Some(NormalCommand::Verb('R')),
            "shift+r is the reset-all verb, spelled uppercase"
        );
        assert_eq!(normal_command(&bare("escape")), None);
        assert_eq!(normal_command(&ks("s", Modifiers::CTRL)), None);
    }

    /// A bare `1`–`9` is a command of its own — the Groupings dialog
    /// jumps to that slot — where `0` is not: `ctrl+0` is
    /// `frame::slot_clear`, there is no slot `0` to open, and a key that
    /// maps to nothing must come back `None` so the surface can drop it
    /// rather than dispatch on a slot that does not exist. Modified digits
    /// are not digits either: `ctrl+3` is the frame's regroup chord and
    /// must never be read as a jump through a dialog.
    #[test]
    fn bare_digits_one_to_nine_are_a_command_and_zero_is_not() {
        assert_eq!(normal_command(&bare("1")), Some(NormalCommand::Digit(1)));
        assert_eq!(normal_command(&bare("9")), Some(NormalCommand::Digit(9)));
        assert_eq!(normal_command(&bare("0")), None);
        assert_eq!(normal_command(&ks("3", Modifiers::CTRL)), None);
        assert_eq!(normal_command(&ks("3", SHIFT)), None);
    }

    #[test]
    fn focus_follows_the_mode_unless_a_capture_is_listening() {
        assert_eq!(focus_target(DialogMode::Filter, false), FocusTarget::Input);
        assert_eq!(focus_target(DialogMode::Normal, false), FocusTarget::Shell);
        // A capture reads raw keystrokes off the shell root whatever the mode says.
        assert_eq!(focus_target(DialogMode::Filter, true), FocusTarget::Shell);
        assert_eq!(focus_target(DialogMode::Normal, true), FocusTarget::Shell);
    }
}
