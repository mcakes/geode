//! Shared pure core for Geode's filtered list surfaces — the settings
//! and keybinding dialogs and the command palette
//! (`docs/superpowers/specs/2026-09-01-dialog-filter-input-design.md` §4,
//! §6): fuzzy ranking of row text, and the keystroke vocabulary those
//! surfaces still own once a focused text input has claimed every
//! printable key.
//!
//! [`rank`] serves the two dialogs; the palette does its own filtering
//! inside `PaletteState` over a richer item type. [`nav_command`] serves
//! all three.
//!
//! No `gpui` here, in the mould of [`crate::vimnav`] and
//! [`crate::vimfind`] — feed it plain strings and shell-native
//! [`Keystroke`]s, unit-test it without a window.
//!
//! ## Why the vocabulary is what it is
//!
//! With a single-line gpui-component `Input` focused, the dialogs can
//! only claim keys that input does not consume first. Verified against
//! the pinned rev (spec §2): `home`/`end` are swallowed unconditionally;
//! `left`/`right` are swallowed except when a single empty selection
//! sits at the very start or end of the text, where the pinned release
//! propagates them (`gpui-base-0.6.2/src/input/base/movement.rs`,
//! `left`/`right`; they were swallowed unconditionally at the old git
//! rev) — `nav_command` claims neither, pinned by
//! `nav_command_claims_nothing_else`, so the vocabulary is unaffected;
//! `up`/`down`/`pageup`/`pagedown` and `tab`/`shift+tab` attach their
//! listeners only for multi-line inputs, so they fall
//! through; `ctrl+d`/`u`/`b`/`n`/`p` are unbound in the `"Input"` context
//! on both platforms. `ctrl+f` is bound to the editor's Search on
//! non-macOS and is reclaimed for us by a `NoAction` binding in
//! `geode-app`'s init (spec §7).
//!
//! `pageup`/`pagedown` are deliberate aliases of `ctrl+b`/`ctrl+f`, not a
//! third step size: they are free, and they are what a hand reaching for
//! "a screenful" finds first on a keyboard that has them.

use crate::keymap::{Keystroke, Modifiers};
use crate::palette::fuzzy_match;
use crate::vimnav::NavCommand;

/// One row that survived the filter: its index into the *unfiltered* row
/// list, plus the char offsets of the query's matched characters within
/// that row's searchable text (what the dialogs paint as highlights).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub row: usize,
    pub indices: Vec<usize>,
}

/// Rank `texts` against `query`.
///
/// An empty (or whitespace-only) query keeps every row, in its natural
/// order, highlighting nothing — the dialogs' resting state. Otherwise
/// only rows whose text fuzzy-matches survive, ordered by
/// [`fuzzy_match`]'s score descending; ties keep natural order, which is
/// what the stable sort below buys and what the dialogs' own
/// `(category, title)` ordering depends on to stay legible.
pub fn rank(texts: &[String], query: &str) -> Vec<Ranked> {
    if query.trim().is_empty() {
        return texts
            .iter()
            .enumerate()
            .map(|(row, _)| Ranked {
                row,
                indices: Vec::new(),
            })
            .collect();
    }

    let mut scored: Vec<(u32, Ranked)> = texts
        .iter()
        .enumerate()
        .filter_map(|(row, text)| {
            fuzzy_match(query, text).map(|(score, indices)| (score, Ranked { row, indices }))
        })
        .collect();
    // `sort_by` is stable, so equal scores keep the natural order the
    // rows arrived in.
    scored.sort_by(|(a, _), (b, _)| b.cmp(a));
    scored.into_iter().map(|(_, ranked)| ranked).collect()
}

/// Map one keystroke onto a list-navigation command, or `None` if no
/// list surface claims it (see the module doc for what they *can*
/// claim). The caller feeds the result to [`crate::vimnav::apply`],
/// which wraps a bare ±1 and clamps every larger or counted step (spec
/// §20.5) against the current — filtered — row count.
///
/// Every palette motion goes through this and `vimnav::apply` — the
/// palette has no motion arms of its own any more (`PaletteState::
/// move_selection` was deleted in spec §20's final fix wave), so a bare
/// ±1 (`up`/`down`/`ctrl+p`/`ctrl+n`) wraps and every larger step
/// (`ctrl+d`/`ctrl+u`, `ctrl+f`/`ctrl+b`/page up/down) clamps, by
/// `apply`'s one rule. The dialogs — keybindings, settings, the object
/// dialog — and the picker and as-of selector route the same way, so
/// nothing here has to inspect the returned delta to decide which rule
/// applies (spec §3, "The command palette"; §20.5).
pub fn nav_command(ks: &Keystroke) -> Option<NavCommand> {
    let delta = match (ks.mods, ks.key.as_str()) {
        (Modifiers::NONE, "up") | (Modifiers::CTRL, "p") => -1,
        (Modifiers::NONE, "down") | (Modifiers::CTRL, "n") => 1,
        (Modifiers::CTRL, "u") => -5,
        (Modifiers::CTRL, "d") => 5,
        (Modifiers::CTRL, "b") | (Modifiers::NONE, "pageup") => -10,
        (Modifiers::CTRL, "f") | (Modifiers::NONE, "pagedown") => 10,
        _ => return None,
    };
    Some(NavCommand::Move(delta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn rows(r: &[Ranked]) -> Vec<usize> {
        r.iter().map(|m| m.row).collect()
    }

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        }
    }

    fn ctrl(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::CTRL,
            key: k.to_string(),
        }
    }

    #[test]
    fn an_empty_query_keeps_every_row_in_natural_order() {
        let t = texts(&["Focus left Workspace", "Split right Workspace"]);
        let ranked = rank(&t, "");
        assert_eq!(rows(&ranked), vec![0, 1]);
        assert!(
            ranked.iter().all(|m| m.indices.is_empty()),
            "an empty query highlights nothing"
        );
    }

    #[test]
    fn a_whitespace_only_query_is_treated_as_empty() {
        let t = texts(&["Focus left Workspace", "Split right Workspace"]);
        assert_eq!(rows(&rank(&t, "   ")), vec![0, 1]);
    }

    #[test]
    fn a_query_drops_rows_that_do_not_match() {
        let t = texts(&["Focus left Workspace", "Toggle theme Appearance"]);
        assert_eq!(rows(&rank(&t, "theme")), vec![1]);
    }

    #[test]
    fn matching_is_case_insensitive_and_subsequence_based() {
        let t = texts(&["Focus left Workspace"]);
        assert_eq!(rows(&rank(&t, "FCSLFT")), vec![0]);
    }

    #[test]
    fn rows_are_ordered_by_score_not_by_position() {
        // "split" is a contiguous prefix-ish run in row 1 and a scattered
        // subsequence in row 0, so row 1 must outrank it despite coming
        // second in natural order.
        let t = texts(&["Set panel list toggle Misc", "Split right Workspace"]);
        assert_eq!(rows(&rank(&t, "split")), vec![1, 0]);
    }

    #[test]
    fn equal_scores_keep_natural_order() {
        let t = texts(&["Focus up Workspace", "Focus up Docks"]);
        let ranked = rank(&t, "focus up");
        assert_eq!(
            rows(&ranked),
            vec![0, 1],
            "identical match shapes must not reorder"
        );
    }

    #[test]
    fn indices_are_char_offsets_into_the_row_text() {
        let t = texts(&["Focus left"]);
        let ranked = rank(&t, "fl");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].indices, vec![0, 6]);
    }

    #[test]
    fn nav_command_maps_the_whole_dialog_vocabulary() {
        let cases: Vec<(Keystroke, NavCommand)> = vec![
            (key("up"), NavCommand::Move(-1)),
            (key("down"), NavCommand::Move(1)),
            (ctrl("p"), NavCommand::Move(-1)),
            (ctrl("n"), NavCommand::Move(1)),
            (ctrl("u"), NavCommand::Move(-5)),
            (ctrl("d"), NavCommand::Move(5)),
            (ctrl("b"), NavCommand::Move(-10)),
            (ctrl("f"), NavCommand::Move(10)),
            (key("pageup"), NavCommand::Move(-10)),
            (key("pagedown"), NavCommand::Move(10)),
        ];
        for (ks, expected) in cases {
            assert_eq!(
                nav_command(&ks),
                Some(expected),
                "wrong command for {:?}",
                ks.key
            );
        }
    }

    #[test]
    fn nav_command_claims_nothing_else() {
        // Keys the dialogs must leave alone: text the input owns, the
        // dialogs' own control keys, and the retired vim motions.
        for ks in [
            key("j"),
            key("k"),
            key("g"),
            key("enter"),
            key("escape"),
            key("tab"),
            key("left"),
            key("right"),
            key("home"),
            key("end"),
            key("/"),
        ] {
            assert_eq!(nav_command(&ks), None, "{} must not be nav", ks.key);
        }
    }

    #[test]
    fn nav_command_requires_exactly_the_named_modifiers() {
        let shift_up = Keystroke {
            mods: Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            key: "up".to_string(),
        };
        assert_eq!(nav_command(&shift_up), None, "shift+up is a selection key");

        let ctrl_shift_d = Keystroke {
            mods: Modifiers {
                ctrl: true,
                shift: true,
                ..Modifiers::NONE
            },
            key: "d".to_string(),
        };
        assert_eq!(nav_command(&ctrl_shift_d), None);
    }
}
