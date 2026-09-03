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
//! The module also owns [`FindStyle`], the `[ui] find_style` user setting
//! choosing between this vim jump model and an fzf-style *filter* — the
//! session/query mechanics below are shared by both; only what the caller
//! does with the query differs (see [`FindStyle`]'s doc comment).
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
//! for matching displayed titles and categories — are unaffected.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, value};

use geode_core::config::Config;

use crate::keymap::{Keystroke, Modifiers};

/// Which behavior `/` gets in list dialogs (`[ui] find_style`): vim's
/// jump model above (the default), or an fzf-style filter. The [`VimFind`]
/// state machine serves both — `/` starts the session and query editing is
/// byte-for-byte identical either way; what differs is what the *caller*
/// does with the query (jump the selection vs. narrow the rendered rows —
/// [`press_while_finding`] vs. [`press_while_finding_fzf`]). Neither driver
/// has a caller today — the filter-first dialog rewrite (spec
/// `2026-09-01-dialog-filter-input-design.md` §8-9) retired the settings
/// dialog's `/` session, their last one — but both stay in the crate,
/// intact and tested, for Phase 3's blotter, which is expected to want
/// this whole vim-jump-vs-filter choice again. Mirrors
/// [`crate::fontsize::FontSize`]'s shape exactly: `config_value`/`label`/
/// `from_value`/`from_config` plus a [`persist_to_user_config`] sibling, so
/// the settings control, startup resolution, and hot reload all ride the
/// same paths font size does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FindStyle {
    #[default]
    Vim,
    Fzf,
}

impl FindStyle {
    /// Display order for the settings control.
    pub const ALL: [FindStyle; 2] = [FindStyle::Vim, FindStyle::Fzf];

    /// Label for the settings control's button.
    pub fn label(self) -> &'static str {
        match self {
            FindStyle::Vim => "Vim",
            FindStyle::Fzf => "Fzf",
        }
    }

    /// The value written to / read from `[ui] find_style`.
    pub fn config_value(self) -> &'static str {
        match self {
            FindStyle::Vim => "vim",
            FindStyle::Fzf => "fzf",
        }
    }

    /// Parse a config value. `None` for anything that isn't exactly one of
    /// the two known values — the caller decides the fallback
    /// ([`FindStyle::from_config`] falls back to `Vim`).
    pub fn from_value(s: &str) -> Option<FindStyle> {
        FindStyle::ALL.into_iter().find(|f| f.config_value() == s)
    }

    /// Resolve the effective find style from the layered config: doc
    /// `app`, key `ui.find_style`. A missing key, or any unknown value, is
    /// `Vim` — same lenient shape as `FontSize::from_config`.
    pub fn from_config(config: &Config) -> FindStyle {
        config
            .get("app", "ui.find_style")
            .and_then(|v| v.as_str())
            .and_then(FindStyle::from_value)
            .unwrap_or_default()
    }
}

/// Write `[ui] find_style` into `<user_dir>/app.toml`, preserving every
/// other table, key, and comment — the same toml_edit + atomic-write
/// contract as [`crate::fontsize::persist_to_user_config`] (see
/// [`crate::theme::persist_to_user_config`]'s doc comment for the full
/// failure-mode reasoning: missing file created with `config_version = 1`,
/// unparseable file left untouched and reported as `Err`, reload-watcher
/// interplay identical).
pub fn persist_to_user_config(user_dir: &Path, style: FindStyle) -> Result<(), String> {
    let path = user_dir.join("app.toml");
    let existed = path.exists();

    let mut doc = if existed {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        text.parse::<DocumentMut>().map_err(|e| {
            format!(
                "failed to parse {}: {e} (file left untouched)",
                path.display()
            )
        })?
    } else {
        DocumentMut::new()
    };

    if !existed {
        doc["config_version"] = value(1_i64);
    }

    if !doc.get("ui").is_some_and(Item::is_table_like) {
        doc["ui"] = Item::Table(Table::new());
    }
    let ui_table = doc["ui"]
        .as_table_mut()
        .expect("just ensured [ui] is a table");
    ui_table["find_style"] = value(style.config_value());

    crate::theme::write_atomic(user_dir, &path, &doc.to_string())
}

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

    /// The query whose matches a caller's rows should highlight: the live
    /// one while a session is editing (updating with every keystroke), else
    /// the last committed one — vim `hlsearch`: matches stay lit for
    /// `n`/`N` until the dialog closes (dialog state is fresh per open;
    /// there is no `:noh`). `None` when neither exists or the live query is
    /// still empty. Promoted from `keybindings_view::highlight_query`
    /// (settings-dialog rewrite) — it only ever read this struct's own two
    /// fields, so it was always this type's method in disguise.
    pub fn highlight_query(&self) -> Option<&str> {
        self.query().or(self.last_query()).filter(|q| !q.is_empty())
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

// ---------------------------------------------------------------------
// Shared session drivers (extracted from `keybindings_view` by the
// settings-dialog rewrite): the press-by-press semantics of an ACTIVE
// find session, over a caller's `(selection, anchor)` pair. When both
// dialogs still had a `/` session, keybindings and settings each held a
// `VimFind` + `selected: usize` + `find_anchor: Option<usize>` and
// delegated every mid-session keystroke here; only what *picking* a row
// meant differed (keybindings started rebind listening, settings just
// kept the selection), which is why [`press_while_finding_fzf`] reports
// an [`FzfOutcome`] for the caller to interpret instead of taking a
// callback. Neither dialog holds that state any more (the filter-first
// rewrite retired both `/` sessions — see the module doc), but the
// contract is unchanged for whichever caller picks these drivers back up
// (Phase 3's blotter — §9 of the design doc): three `&mut` parameters
// rather than a trait or a shared struct, so a caller can keep its own
// state type (with dialog-specific extras alongside) and hand over a
// borrow of exactly the three fields involved.
// ---------------------------------------------------------------------

/// Feed one keystroke to an ACTIVE find session in **vim** style and move
/// `selected` per the outcome: an incremental jump from the anchor on
/// every query edit (first match at-or-after the anchor, wrapping; back
/// to the anchor when the query is empty or matches nothing — vim
/// `incsearch`), anchor restore on cancel, anchor cleared (selection
/// stays) on commit. The caller guarantees `find.is_active()` and
/// swallows the keystroke regardless of outcome.
pub fn press_while_finding(
    find: &mut VimFind,
    selected: &mut usize,
    find_anchor: &mut Option<usize>,
    texts: &[String],
    ks: &Keystroke,
) {
    match find.press(ks) {
        FindResult::Updated => {
            let anchor = find_anchor.unwrap_or(*selected);
            *selected = find
                .query()
                .filter(|q| !q.is_empty())
                .and_then(|q| find_match(texts, anchor, FindDirection::Forward, q))
                .unwrap_or(anchor);
        }
        FindResult::Commit => {
            *find_anchor = None;
        }
        FindResult::Cancel => {
            if let Some(anchor) = find_anchor.take() {
                *selected = anchor;
            }
        }
        FindResult::Ignored => {}
    }
}

/// Repeat the last committed find in `dir` (`n`/`N`), excluding the
/// current row so every press advances (wrapping). Returns false — the
/// keystroke was not a find repeat — when nothing was ever committed;
/// with a committed query but no match anywhere, the selection just stays
/// (still handled: `n` after a stale query must not fall through to
/// anything else).
pub fn repeat_find(
    find: &VimFind,
    selected: &mut usize,
    texts: &[String],
    dir: FindDirection,
) -> bool {
    let Some(query) = find.last_query() else {
        return false;
    };
    if texts.is_empty() {
        return true;
    }
    let start = match dir {
        FindDirection::Forward => (*selected + 1) % texts.len(),
        FindDirection::Backward => (*selected + texts.len() - 1) % texts.len(),
    };
    if let Some(ix) = find_match(texts, start, dir, query) {
        *selected = ix;
    }
    true
}

/// The row indices whose searchable text contains `query` — the fzf
/// counterpart of [`find_match`]'s single jump target: the same
/// case-insensitive substring semantics, but *all* matches, in row order,
/// for a dialog's render pass to paint as the narrowed list while an fzf
/// session is active. An empty query filters nothing (every index), so a
/// just-started session shows the full list until the first character
/// lands.
pub fn filter_matches(texts: &[String], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..texts.len()).collect();
    }
    let needle = query.to_lowercase();
    (0..texts.len())
        .filter(|&ix| texts[ix].to_lowercase().contains(&needle))
        .collect()
}

/// What one keystroke did to an active **fzf**-style session — the pick
/// seam a future caller of [`press_while_finding_fzf`] interprets for
/// itself (see the section comment above): the driver itself has no idea
/// what a dialog does with a picked row, only that one was picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FzfOutcome {
    /// The session continues: a query edit (selection re-anchored to the
    /// first match), an arrow step within the matches, a swallowed stray
    /// chord — or a bare `enter` on ZERO matches, which is deliberately
    /// inert (there is no row to pick, so nothing may end the session).
    Continue,
    /// A bare `enter` picked the currently selected row: the session is
    /// over, the anchor is dropped, and `selected` stays on the picked
    /// row. The caller decides what picking *means* beyond that.
    Picked,
    /// The session was cancelled (`escape`, or backspace past the start of
    /// the query); the anchor selection has already been restored.
    Cancelled,
}

/// Feed one keystroke to an ACTIVE find session in **fzf** style
/// (`[ui] find_style = "fzf"`) — the sibling of [`press_while_finding`],
/// kept separate so the vim path stays byte-for-byte untouched. Query
/// editing is still [`VimFind::press`] exactly as in vim mode
/// (backspace-past-start cancels, stray chords are swallowed, ...); what
/// differs is everything around it:
///
/// - every query edit resets the selection to the FIRST match (fzf's
///   idiom — the filtered list re-anchors at its top), parking on the
///   anchor when nothing matches (moot while no rows render, but
///   deterministic);
/// - bare `up`/`down` step the selection through the matches, clamped at
///   the ends (no wrap) — intercepted here, *before* [`VimFind::press`]
///   would swallow them as non-printable (vim mode leaves them to it);
/// - bare `enter` picks: with at least one match the session ends
///   ([`VimFind::press`] sees the enter, so an empty query's Cancel and a
///   non-empty one's Commit both close it), the anchor is dropped,
///   selection stays on the picked row, and [`FzfOutcome::Picked`] tells
///   the caller to do whatever picking means in its dialog. With ZERO
///   matches the enter never reaches [`VimFind::press`] at all: nothing
///   happens and the session stays active;
/// - `escape`/backspace-past-start cancel and restore the anchor, exactly
///   as vim mode does.
///
/// The caller guarantees `find.is_active()` and swallows the keystroke
/// regardless of outcome, same contract as [`press_while_finding`].
pub fn press_while_finding_fzf(
    find: &mut VimFind,
    selected: &mut usize,
    find_anchor: &mut Option<usize>,
    texts: &[String],
    ks: &Keystroke,
) -> FzfOutcome {
    let bare = ks.mods == Modifiers::NONE;

    if bare && (ks.key == "up" || ks.key == "down") {
        let query = find.query().unwrap_or("");
        let matches = filter_matches(texts, query);
        let Some(pos) = matches.iter().position(|&ix| ix == *selected) else {
            // Selection off the match list only happens with zero matches
            // (edits and arrows both keep it on one otherwise) — nothing
            // to step through.
            return FzfOutcome::Continue;
        };
        let new_pos = match ks.key.as_str() {
            "up" => pos.saturating_sub(1),
            _ => (pos + 1).min(matches.len() - 1),
        };
        *selected = matches[new_pos];
        return FzfOutcome::Continue;
    }

    if bare && ks.key == "enter" {
        let query = find.query().unwrap_or("");
        if filter_matches(texts, query).is_empty() {
            return FzfOutcome::Continue;
        }
        // Commit (non-empty query) or Cancel (empty query) — either way
        // the session is over; the distinction only matters to vim mode's
        // `n`/`N`, which fzf mode never consults.
        find.press(ks);
        *find_anchor = None;
        return FzfOutcome::Picked;
    }

    match find.press(ks) {
        FindResult::Updated => {
            let anchor = find_anchor.unwrap_or(*selected);
            let query = find.query().unwrap_or("");
            *selected = filter_matches(texts, query)
                .first()
                .copied()
                .unwrap_or(anchor);
            FzfOutcome::Continue
        }
        FindResult::Cancel => {
            if let Some(anchor) = find_anchor.take() {
                *selected = anchor;
            }
            FzfOutcome::Cancelled
        }
        // Commit is unreachable (bare enter is intercepted above);
        // Ignored (a stray chord) changes nothing.
        FindResult::Commit | FindResult::Ignored => FzfOutcome::Continue,
    }
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

    // -- the shared session drivers (extracted from keybindings_view) ------
    //
    // These tests moved here with the code they pin (the settings-dialog
    // rewrite promoted the session-press semantics out of
    // `keybindings_view` so both dialogs could share one driver). They
    // are now the only tests of these drivers: both dialogs have since
    // moved to an always-focused fuzzy filter (the settings dialog was
    // the drivers' last caller), so `press_while_finding` and
    // `press_while_finding_fzf` have no caller outside this module today
    // — retained whole, and tested here, for Phase 3's blotter (see the
    // module doc and `FindStyle`'s).

    /// A session mid-flight: `find` started, anchor saved at `selected` —
    /// the exact state a caller is in right after handling `/`.
    fn session(selected: usize) -> (VimFind, usize, Option<usize>) {
        let mut find = VimFind::new();
        find.start();
        (find, selected, Some(selected))
    }

    fn drive_texts() -> Vec<String> {
        texts(&[
            "Close tile Workspace",
            "Focus left Workspace",
            "Toggle palette Palette",
            "Focus right Workspace",
        ])
    }

    #[test]
    fn vim_driver_jumps_from_the_anchor_and_restores_on_no_match() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(0);
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        assert_eq!(sel, 1, "first 'f...' match at/after the anchor");
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("z"));
        assert_eq!(sel, 0, "'fz' matches nothing — back to the anchor");
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("backspace"));
        assert_eq!(sel, 1, "back to 'f', back to the match");
    }

    #[test]
    fn vim_driver_commit_keeps_the_match_and_escape_restores_the_anchor() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(0);
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("enter"));
        assert_eq!(sel, 1);
        assert_eq!(anchor, None, "commit drops the anchor");
        assert!(!find.is_active());

        let (mut find, mut sel, mut anchor) = session(0);
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        assert_eq!(sel, 1);
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("escape"));
        assert_eq!(sel, 0, "escape restores the anchor selection");
        assert_eq!(anchor, None);
    }

    #[test]
    fn shared_repeat_find_advances_with_wrap_in_both_directions() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(1);
        for k in ["f", "o", "c", "u", "s"] {
            press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key(k));
        }
        press_while_finding(&mut find, &mut sel, &mut anchor, &t, &key("enter"));
        assert_eq!(sel, 1);

        assert!(repeat_find(&find, &mut sel, &t, FindDirection::Forward));
        assert_eq!(sel, 3);
        assert!(repeat_find(&find, &mut sel, &t, FindDirection::Forward));
        assert_eq!(sel, 1, "forward repeat wraps past the end");
        assert!(repeat_find(&find, &mut sel, &t, FindDirection::Backward));
        assert_eq!(sel, 3, "backward repeat wraps past the start");
    }

    #[test]
    fn shared_repeat_find_without_a_committed_query_is_not_handled() {
        let t = drive_texts();
        let find = VimFind::new();
        let mut sel = 0;
        assert!(
            !repeat_find(&find, &mut sel, &t, FindDirection::Forward),
            "a bare n with nothing committed must fall through to list nav"
        );
        assert_eq!(sel, 0);
    }

    #[test]
    fn filter_matches_is_case_insensitive_and_empty_query_returns_all() {
        let t = drive_texts();
        assert_eq!(
            filter_matches(&t, ""),
            vec![0, 1, 2, 3],
            "an empty query filters nothing — every row stays visible"
        );
        assert_eq!(filter_matches(&t, "FOCUS"), vec![1, 3]);
        assert_eq!(filter_matches(&t, "palette"), vec![2]);
        assert_eq!(filter_matches(&t, "zzz"), Vec::<usize>::new());
    }

    #[test]
    fn fzf_driver_query_edits_reset_selection_to_the_first_match() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(2);
        let outcome = press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        assert_eq!(outcome, FzfOutcome::Continue);
        assert_eq!(
            sel, 1,
            "the FIRST 'f...' match, not the nearest to the anchor — fzf \
             resets to the top of the filtered list on every edit"
        );
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("z"));
        assert_eq!(
            sel, 2,
            "'fz' matches nothing — selection parks on the anchor (moot \
             while nothing renders, but deterministic)"
        );
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("backspace"));
        assert_eq!(sel, 1, "back to 'f', back to the first match");
    }

    #[test]
    fn fzf_driver_arrows_step_within_the_matches_and_clamp_at_the_ends() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(0);
        for k in ["f", "o", "c", "u", "s"] {
            press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key(k));
        }
        assert_eq!(sel, 1, "'focus' matches rows 1 and 3");
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("down"));
        assert_eq!(sel, 3);
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("down"));
        assert_eq!(sel, 3, "clamped at the last match, no wrap");
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("up"));
        assert_eq!(sel, 1);
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("up"));
        assert_eq!(sel, 1, "clamped at the first match, no wrap");
        assert!(find.is_active(), "arrows never end the session");
    }

    #[test]
    fn fzf_driver_enter_picks_and_ends_the_session() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(0);
        for k in ["f", "o", "c", "u", "s"] {
            press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key(k));
        }
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("down"));
        let outcome = press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("enter"));
        assert_eq!(outcome, FzfOutcome::Picked);
        assert!(!find.is_active(), "enter ends the session");
        assert_eq!(sel, 3, "selection lands on the picked row");
        assert_eq!(anchor, None);
    }

    #[test]
    fn fzf_driver_enter_with_zero_matches_is_inert_and_stays_active() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(1);
        for k in ["z", "z", "z"] {
            press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key(k));
        }
        let outcome = press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("enter"));
        assert_eq!(
            outcome,
            FzfOutcome::Continue,
            "enter on an empty match set must not report a pick"
        );
        assert!(
            find.is_active(),
            "enter on an empty match set must keep the session alive"
        );
        assert_eq!(find.query(), Some("zzz"), "the query survives too");
    }

    #[test]
    fn fzf_driver_enter_on_an_empty_query_picks_the_selected_row() {
        // `/` then enter with nothing typed: every row matches the empty
        // query, so enter picks whatever is selected (the anchor row).
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(2);
        let outcome = press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("enter"));
        assert_eq!(outcome, FzfOutcome::Picked);
        assert!(!find.is_active());
        assert_eq!(sel, 2);
        assert_eq!(anchor, None);
    }

    #[test]
    fn fzf_driver_escape_cancels_and_restores_the_anchor() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(2);
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        assert_eq!(sel, 1);
        let outcome = press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("escape"));
        assert_eq!(outcome, FzfOutcome::Cancelled);
        assert!(!find.is_active());
        assert_eq!(sel, 2, "escape restores the anchor selection");
    }

    #[test]
    fn fzf_driver_backspace_past_the_start_cancels_and_restores_the_anchor() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(2);
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("backspace"));
        assert!(find.is_active(), "one backspace only empties the query");
        let outcome =
            press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("backspace"));
        assert_eq!(outcome, FzfOutcome::Cancelled);
        assert!(
            !find.is_active(),
            "backspace past the start abandons the session, same as vim mode"
        );
        assert_eq!(sel, 2);
    }

    #[test]
    fn fzf_driver_stray_chords_are_swallowed_without_moving_the_selection() {
        let t = drive_texts();
        let (mut find, mut sel, mut anchor) = session(0);
        press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &key("f"));
        assert_eq!(sel, 1);
        let outcome = press_while_finding_fzf(&mut find, &mut sel, &mut anchor, &t, &ctrl("d"));
        assert_eq!(outcome, FzfOutcome::Continue);
        assert_eq!(sel, 1, "a modified keystroke changes nothing");
        assert!(find.is_active());
    }

    #[test]
    fn highlight_query_prefers_the_live_query_then_the_committed_one() {
        let mut find = VimFind::new();
        assert_eq!(find.highlight_query(), None);

        find.start();
        find.press(&key("a"));
        find.press(&key("enter"));
        assert_eq!(find.highlight_query(), Some("a"), "committed query");
        find.start();
        assert_eq!(
            find.highlight_query(),
            None,
            "an active-but-empty session must blank the highlight, not \
             show the stale committed query"
        );
        find.press(&key("b"));
        assert_eq!(find.highlight_query(), Some("b"), "live query wins");
        find.press(&key("escape"));
        assert_eq!(
            find.highlight_query(),
            Some("a"),
            "after cancel the committed query lights up again for n/N"
        );
    }

    // -- FindStyle (`[ui] find_style`) --------------------------------

    use geode_core::config::{ConfigSources, LayerDoc};

    #[test]
    fn find_style_values_roundtrip_and_vim_is_the_default() {
        assert_eq!(FindStyle::default(), FindStyle::Vim);
        for style in FindStyle::ALL {
            assert_eq!(FindStyle::from_value(style.config_value()), Some(style));
        }
        assert_eq!(FindStyle::Vim.config_value(), "vim");
        assert_eq!(FindStyle::Fzf.config_value(), "fzf");
        assert_eq!(FindStyle::Vim.label(), "Vim");
        assert_eq!(FindStyle::Fzf.label(), "Fzf");
        assert_eq!(FindStyle::from_value("emacs"), None);
        assert_eq!(FindStyle::from_value(""), None);
    }

    #[test]
    fn find_style_from_config_reads_ui_find_style_with_vim_fallback() {
        let with = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[ui]\nfind_style = \"fzf\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(FindStyle::from_config(&with), FindStyle::Fzf);

        let empty = Config::load(&ConfigSources::default());
        assert_eq!(FindStyle::from_config(&empty), FindStyle::Vim);

        let bogus = Config::load(&ConfigSources {
            builtin: vec![LayerDoc::builtin("app", "[ui]\nfind_style = \"emacs\"\n").unwrap()],
            desk: None,
            user: None,
        });
        assert_eq!(FindStyle::from_config(&bogus), FindStyle::Vim);
    }

    #[test]
    fn persist_creates_a_fresh_file_with_config_version() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), FindStyle::Fzf).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("config_version = 1"));
        assert!(text.contains("[ui]"));
        assert!(text.contains("find_style = \"fzf\""));
    }

    #[test]
    fn persist_preserves_comments_and_sibling_ui_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(
            &path,
            "# my config\nconfig_version = 1\n\n[ui]\nfont_size = \"large\" # keep me\n",
        )
        .unwrap();
        persist_to_user_config(dir.path(), FindStyle::Fzf).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my config"));
        assert!(text.contains("font_size = \"large\" # keep me"));
        assert!(text.contains("find_style = \"fzf\""));
    }

    #[test]
    fn persist_overwrites_a_previous_value_in_place() {
        let dir = tempfile::tempdir().unwrap();
        persist_to_user_config(dir.path(), FindStyle::Fzf).unwrap();
        persist_to_user_config(dir.path(), FindStyle::Vim).unwrap();
        let text = std::fs::read_to_string(dir.path().join("app.toml")).unwrap();
        assert!(text.contains("find_style = \"vim\""));
        assert!(!text.contains("find_style = \"fzf\""));
    }

    #[test]
    fn persist_refuses_to_touch_an_unparseable_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.toml");
        std::fs::write(&path, "not [valid toml").unwrap();
        assert!(persist_to_user_config(dir.path(), FindStyle::Fzf).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not [valid toml");
    }
}
