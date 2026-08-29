//! Reusable vim-style `/` find for list dialogs.
//!
//! A pure state machine in the exact mold of [`crate::vimnav`]: no gpui,
//! feed it [`Keystroke`]s, get back a [`FindResult`] describing what the
//! query session did, plus a pure wrapping search function
//! ([`find_match`]) the caller applies to its own rows' searchable text.
//! Built for the keybinding dialog first, but deliberately generic — any
//! list dialog (settings-style or otherwise) can hold a [`VimFind`] next
//! to its `VimListNav` and get the same `/` interaction.
//!
//! ## Semantics (vim's jump model, not a filter)
//!
//! `/` starts a find session; typed characters build a query shown in the
//! caller's status line; the list itself never filters or reorders — the
//! caller moves its *selection* to matches instead (vim `incsearch`
//! style, live on every edit, from the anchor where `/` was pressed):
//!
//! - printable keys append to the query (`shift`+letter appends the
//!   uppercase letter; matching is case-insensitive either way);
//! - `backspace` removes the last character — on an already-empty query it
//!   exits the session (vim: backspacing past the start of the pattern
//!   abandons the search);
//! - `enter` commits: the session ends, the query is remembered for
//!   `n`/`N` repeats ([`VimFind::last_query`]), selection stays where the
//!   incremental jump put it. Enter on an empty query cancels instead;
//! - `escape` cancels: the session ends, the caller restores the anchor
//!   selection, and the previous committed query (if any) survives for
//!   `n`/`N`;
//! - anything else (modified keys, non-printable) is swallowed
//!   ([`FindResult::Ignored`]) so stray chords can't fall through to list
//!   navigation mid-session.
//!
//! `n`/`N` themselves are the *caller's* keys (pressed outside a session,
//! they're plain keystrokes this module never sees) — the caller checks
//! [`VimFind::last_query`] and applies [`find_match`] with the direction.
//!
//! ## Known keystroke-vs-character limitation
//!
//! This crate's [`Keystroke`] carries a key *name*, not the typed
//! character ([`crate::shell::keys::convert_keystroke`] deliberately drops
//! `key_char`), so a shifted symbol appends the key's base character (e.g.
//! typing `:` on a US layout arrives as `shift+;` and appends `;`).
//! Letters, digits, and unshifted symbols — the realistic query alphabet
//! for matching titles, categories, and action ids — are unaffected.

use crate::keymap::{Keystroke, Modifiers};

/// Direction for [`find_match`] and the caller's `n`/`N` repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindDirection {
    Forward,
    Backward,
}

/// The outcome of feeding one [`Keystroke`] to [`VimFind::press`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindResult {
    /// The query changed (character appended or removed) — re-run the
    /// incremental jump from the anchor and re-render.
    Updated,
    /// `enter` committed a non-empty query; the session is over and the
    /// query is saved for `n`/`N` ([`VimFind::last_query`]).
    Commit,
    /// The session ended with nothing committed (`escape`, `enter` on an
    /// empty query, or `backspace` past the start) — restore the anchor.
    Cancel,
    /// Swallowed without changing the query (a modified/non-printable
    /// keystroke mid-session, or a press while no session is active).
    Ignored,
}

/// Find-mode state: an in-progress query while a session is active, and
/// the last committed query for `n`/`N`. Holds no gpui types (the caller's
/// scroll handle stays outside, same split as `VimListNav`).
#[derive(Debug, Default)]
pub struct VimFind {
    editing: Option<String>,
    last: Option<String>,
}

impl VimFind {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a find session (the caller saw a bare `/`). The caller is
    /// responsible for remembering its own selection anchor to restore on
    /// [`FindResult::Cancel`].
    pub fn start(&mut self) {
        self.editing = Some(String::new());
    }

    /// True while a session is active (keystrokes belong to this module).
    pub fn is_active(&self) -> bool {
        self.editing.is_some()
    }

    /// The in-progress query, while a session is active.
    pub fn query(&self) -> Option<&str> {
        self.editing.as_deref()
    }

    /// The last committed query — what `n`/`N` repeat.
    pub fn last_query(&self) -> Option<&str> {
        self.last.as_deref()
    }

    /// Abandon any active session without touching `last_query` — for
    /// external interruptions (a row click, the dialog closing).
    pub fn cancel(&mut self) {
        self.editing = None;
    }

    /// Status-line display for an active session: `/` followed by the
    /// query so far — the analogue of `VimListNav::pending_display`.
    pub fn pending_display(&self) -> Option<String> {
        self.editing.as_ref().map(|q| format!("/{q}"))
    }

    /// Feed one keystroke to the active session (see the module doc for
    /// the full key table). While no session is active every keystroke is
    /// [`FindResult::Ignored`] — callers gate on [`Self::is_active`].
    pub fn press(&mut self, ks: &Keystroke) -> FindResult {
        let Some(query) = self.editing.as_mut() else {
            return FindResult::Ignored;
        };
        let bare = ks.mods == Modifiers::NONE;
        let shift_only = ks.mods
            == Modifiers {
                shift: true,
                ..Modifiers::NONE
            };

        if bare && ks.key == "escape" {
            self.editing = None;
            return FindResult::Cancel;
        }
        if bare && ks.key == "enter" {
            let query = self.editing.take().expect("checked Some above");
            return if query.is_empty() {
                FindResult::Cancel
            } else {
                self.last = Some(query);
                FindResult::Commit
            };
        }
        if bare && ks.key == "backspace" {
            if query.pop().is_none() {
                self.editing = None;
                return FindResult::Cancel;
            }
            return FindResult::Updated;
        }
        if bare && ks.key == "space" {
            query.push(' ');
            return FindResult::Updated;
        }
        if (bare || shift_only) && ks.key.chars().count() == 1 {
            let ch = ks.key.chars().next().expect("count checked above");
            if shift_only {
                query.extend(ch.to_uppercase());
            } else {
                query.push(ch);
            }
            return FindResult::Updated;
        }
        FindResult::Ignored
    }
}

/// Wrapping, case-insensitive substring search over `texts`: the first
/// index whose text contains `query`, scanning `texts.len()` entries
/// beginning **at** `start` and stepping in `dir` with wraparound. `None`
/// for no match anywhere or an empty query.
///
/// Callers pick `start` to get both behaviors this module serves:
/// incremental jump searches from the anchor itself (`start = anchor`,
/// `Forward` — the anchor row matching means the selection stays put,
/// vim-style), while `n`/`N` repeats exclude the current row
/// (`start = selected ± 1`, wrapped by the caller or by this function's
/// own modular arithmetic).
pub fn find_match(
    texts: &[String],
    start: usize,
    dir: FindDirection,
    query: &str,
) -> Option<usize> {
    if texts.is_empty() || query.is_empty() {
        return None;
    }
    let needle = query.to_lowercase();
    let len = texts.len();
    let step = match dir {
        FindDirection::Forward => 1,
        // +len-1 ≡ -1 (mod len): stepping backward without underflow.
        FindDirection::Backward => len - 1,
    };
    let mut ix = start % len;
    for _ in 0..len {
        if texts[ix].to_lowercase().contains(&needle) {
            return Some(ix);
        }
        ix = (ix + step) % len;
    }
    None
}

/// The byte range of the first case-insensitive occurrence of `query` in
/// `text`, for span highlighting (`StyledText::with_highlights` takes byte
/// ranges) — the *display* counterpart of [`find_match`]'s yes/no. `None`
/// for an empty query or no occurrence.
///
/// Char-wise scan rather than `to_lowercase().find(..)` on the whole
/// string: lowercasing can change byte lengths (ß → ss), which would skew
/// a byte offset found in the lowered copy when mapped back onto `text`.
/// Comparing per-char keeps every returned offset a real boundary in
/// `text` itself.
pub fn match_range(text: &str, query: &str) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let query_lower: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    text.char_indices().find_map(|(start, _)| {
        prefix_match_len(&text[start..], &query_lower).map(|len| start..start + len)
    })
}

/// If `slice` begins with the (already-lowercased) query chars, the byte
/// length of that matching prefix in `slice`'s own encoding; else `None`.
fn prefix_match_len(slice: &str, query_lower: &[char]) -> Option<usize> {
    let mut qpos = 0;
    for (offset, ch) in slice.char_indices() {
        for lc in ch.to_lowercase() {
            if query_lower.get(qpos) != Some(&lc) {
                return None;
            }
            qpos += 1;
        }
        if qpos >= query_lower.len() {
            return Some(offset + ch.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.to_string(),
        }
    }

    fn shift(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
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
    fn typing_builds_the_query_and_commit_remembers_it() {
        let mut find = VimFind::new();
        assert!(!find.is_active());
        find.start();
        assert!(find.is_active());
        assert_eq!(find.press(&key("f")), FindResult::Updated);
        assert_eq!(find.press(&key("o")), FindResult::Updated);
        assert_eq!(find.press(&shift("c")), FindResult::Updated);
        assert_eq!(find.query(), Some("foC"));
        assert_eq!(find.pending_display().as_deref(), Some("/foC"));
        assert_eq!(find.press(&key("enter")), FindResult::Commit);
        assert!(!find.is_active());
        assert_eq!(find.last_query(), Some("foC"));
    }

    #[test]
    fn space_and_backspace_edit_the_query() {
        let mut find = VimFind::new();
        find.start();
        find.press(&key("a"));
        find.press(&key("space"));
        find.press(&key("b"));
        assert_eq!(find.query(), Some("a b"));
        assert_eq!(find.press(&key("backspace")), FindResult::Updated);
        assert_eq!(find.query(), Some("a "));
    }

    #[test]
    fn backspace_past_the_start_cancels_the_session() {
        let mut find = VimFind::new();
        find.start();
        assert_eq!(find.press(&key("backspace")), FindResult::Cancel);
        assert!(!find.is_active());
    }

    #[test]
    fn escape_cancels_but_keeps_the_previous_committed_query() {
        let mut find = VimFind::new();
        find.start();
        find.press(&key("x"));
        find.press(&key("enter"));
        assert_eq!(find.last_query(), Some("x"));
        find.start();
        find.press(&key("y"));
        assert_eq!(find.press(&key("escape")), FindResult::Cancel);
        assert_eq!(
            find.last_query(),
            Some("x"),
            "a cancelled session must not clobber the committed query n/N use"
        );
    }

    #[test]
    fn enter_on_an_empty_query_cancels_rather_than_committing() {
        let mut find = VimFind::new();
        find.start();
        assert_eq!(find.press(&key("enter")), FindResult::Cancel);
        assert_eq!(find.last_query(), None);
    }

    #[test]
    fn modified_keystrokes_are_swallowed_mid_session() {
        let mut find = VimFind::new();
        find.start();
        assert_eq!(find.press(&ctrl("d")), FindResult::Ignored);
        assert_eq!(find.query(), Some(""));
        assert!(find.is_active(), "a stray chord must not end the session");
    }

    #[test]
    fn presses_while_inactive_are_ignored() {
        let mut find = VimFind::new();
        assert_eq!(find.press(&key("a")), FindResult::Ignored);
        assert_eq!(find.query(), None);
    }

    fn texts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn find_match_is_case_insensitive_and_starts_at_start() {
        let t = texts(&["Alpha", "Beta", "Gamma"]);
        assert_eq!(find_match(&t, 0, FindDirection::Forward, "beta"), Some(1));
        assert_eq!(find_match(&t, 1, FindDirection::Forward, "BETA"), Some(1));
    }

    #[test]
    fn find_match_wraps_in_both_directions() {
        let t = texts(&["match", "other", "another"]);
        // Forward from 1 wraps past the end back to 0.
        assert_eq!(find_match(&t, 1, FindDirection::Forward, "match"), Some(0));
        // Backward from 1: 1, 0 — finds 0 without needing the wrap...
        assert_eq!(find_match(&t, 1, FindDirection::Backward, "match"), Some(0));
        // ...and from 2 backward with only a late match, wraps 2 → 1 → 0.
        let t2 = texts(&["a", "b", "target"]);
        assert_eq!(
            find_match(&t2, 1, FindDirection::Backward, "target"),
            Some(2),
            "backward from 1 must wrap around to reach index 2"
        );
    }

    #[test]
    fn find_match_none_for_no_match_or_empty_query() {
        let t = texts(&["a", "b"]);
        assert_eq!(find_match(&t, 0, FindDirection::Forward, "zzz"), None);
        assert_eq!(find_match(&t, 0, FindDirection::Forward, ""), None);
        assert_eq!(find_match(&[], 0, FindDirection::Forward, "a"), None);
    }

    #[test]
    fn match_range_finds_the_first_case_insensitive_occurrence() {
        assert_eq!(match_range("Focus left", "focus"), Some(0..5));
        assert_eq!(match_range("Focus left", "LEFT"), Some(6..10));
        assert_eq!(match_range("workspace::focus_left", "focus"), Some(11..16));
    }

    #[test]
    fn match_range_none_for_no_match_or_empty_query() {
        assert_eq!(match_range("Focus left", "zzz"), None);
        assert_eq!(match_range("Focus left", ""), None);
        assert_eq!(match_range("", "a"), None);
    }

    #[test]
    fn match_range_returns_byte_offsets_on_char_boundaries() {
        // Multibyte prefix: 'é' is 2 bytes — the range must be byte-
        // addressed (for StyledText::with_highlights) yet still start on
        // the real boundary of the matched span.
        let text = "éclair Focus";
        let range = match_range(text, "focus").expect("should match");
        assert_eq!(&text[range], "Focus");
    }

    #[test]
    fn match_range_matches_case_insensitively_across_multibyte_chars() {
        let text = "ÉCLAIR";
        let range = match_range(text, "éclair").expect("should match");
        assert_eq!(range, 0..text.len());
    }
}
