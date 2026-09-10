//! The two-mode vocabulary Geode's *modal* dialogs share
//! (`docs/superpowers/specs/2026-09-08-geode-dialog-interaction-model-design.md`).
//!
//! A modal surface opens in [`DialogMode::Normal`], where no `Input` is
//! focused and bare letters are verbs; `/` enters [`DialogMode::Filter`],
//! which is exactly the always-focused filter that ships today. A surface
//! that has no verbs to reach outside its filter is *filter-only* and
//! never uses this module at all — the palette, settings, the dimension
//! picker and the as-of selector are unchanged (spec §3).
//!
//! No `gpui` here, in the mould of [`crate::vimnav`] and
//! [`crate::listfilter`]: feed it shell-native [`Keystroke`]s and
//! unit-test every transition without a window.

use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;
use crate::vimnav::NavCommand;

/// `Modifiers` has no `SHIFT` constant (only `NONE`/`CTRL`/`ALT`/`CMD`,
/// see `keymap::keystroke`) — this mirrors `vimnav`'s own local `SHIFT`.
const SHIFT: Modifiers = Modifiers {
    ctrl: false,
    alt: false,
    shift: true,
    cmd: false,
};

/// Which mode a modal dialog is in. A filter-only surface has no value of
/// this type at all, rather than being permanently `Filter` — the
/// distinction matters because such a surface's `escape` closes the modal
/// instead of walking the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Normal,
    Filter,
}

/// One rung of the `escape` ladder (spec §5). Each rung changes something
/// the user can see, so `escape` is never a keystroke that appears inert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeStep {
    /// Filter → normal, **keeping the query applied**: leaving a search
    /// leaves you on the match, it does not undo the search.
    LeaveFilter,
    ClearQuery,
    PreviousStage,
    Close,
}

/// The first rung that applies. `has_previous_stage` is the surface's own
/// question (4c's `Edit` has one, `Browse` does not); the keybinding
/// dialog always passes `false`.
pub fn escape_step(mode: DialogMode, query_is_empty: bool, has_previous_stage: bool) -> EscapeStep {
    match mode {
        DialogMode::Filter => EscapeStep::LeaveFilter,
        DialogMode::Normal if !query_is_empty => EscapeStep::ClearQuery,
        DialogMode::Normal if has_previous_stage => EscapeStep::PreviousStage,
        DialogMode::Normal => EscapeStep::Close,
    }
}

/// What a keystroke means in normal mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalCommand {
    /// Movement, shared with filter mode so one hand learns one set.
    Nav(NavCommand),
    EnterFilter,
    Commit,
    Toggle,
    /// `shift+space`: step the selected row's value backward. The
    /// vocabulary had a forward step (`Toggle`) and no way back — added
    /// so a `Number` can be lowered and a `Choice` can reach the option
    /// just behind it without wrapping all the way around.
    ToggleBack,
    /// Move the selected *item* rather than the selection: `shift+j` /
    /// `shift+k`. This is what replaces the pick-up sub-mode an earlier
    /// draft of Phase 4c needed when no key was free.
    MoveItem(i32),
    EditText,
    /// A bare letter the vocabulary does not claim — the surface's own
    /// verb (`s`, `d`, `r`, `n`).
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
            "space" => Some(NormalCommand::ToggleBack),
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
        "space" => Some(NormalCommand::Toggle),
        "i" => Some(NormalCommand::EditText),
        // `escape` is the ladder's, never a verb — returning it here
        // would swallow the one key every dialog needs.
        "escape" => None,
        key => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => Some(NormalCommand::Verb(c)),
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

    /// The ladder of spec §5: every rung changes something visible, so a
    /// dialog never eats an `escape` that appears to do nothing.
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

    /// `shift+space` steps a value backward; the forward key and the
    /// `shift+j`/`shift+k` item movers are unchanged by adding it.
    #[test]
    fn shift_space_steps_a_value_backward() {
        assert_eq!(
            normal_command(&ks("space", SHIFT)),
            Some(NormalCommand::ToggleBack)
        );
        // The forward key is unchanged, and shift+j/k still move items.
        assert_eq!(normal_command(&bare("space")), Some(NormalCommand::Toggle));
        assert_eq!(
            normal_command(&ks("j", SHIFT)),
            Some(NormalCommand::MoveItem(1))
        );
    }

    /// Arrows and the ctrl-steps keep working in normal mode: the two
    /// modes share one navigation vocabulary, so a hand that learned
    /// `ctrl+d` in the palette is not retrained at the dialog.
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
        assert_eq!(normal_command(&bare("escape")), None);
        assert_eq!(normal_command(&ks("s", Modifiers::CTRL)), None);
    }
}
