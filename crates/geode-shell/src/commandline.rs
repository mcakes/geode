//! The per-tile command line's pure state (Phase 3 §3.4): the word under
//! the cursor, ranked completions through the palette's fuzzy matcher,
//! accepting one, and the key vocabulary. `shell::commandline_view`
//! paints it; `ShellView` routes keys to it. No `gpui` here.

use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter::{Ranked, rank};
use crate::tiling::TileId;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prompt {
    Find,
    Command,
}

impl Prompt {
    pub fn glyph(self) -> &'static str {
        match self {
            Prompt::Find => "/",
            Prompt::Command => ":",
        }
    }
}

#[derive(Debug)]
pub struct CommandLine {
    pub prompt: Prompt,
    pub tile: TileId,
    pub error: Option<String>,
    /// The occupant's vocabulary for the word under the cursor, as last
    /// asked.
    pub words: Vec<String>,
    /// `words` ranked against that word; empty when there is no popup.
    pub candidates: Vec<Ranked>,
    pub highlighted: usize,
    pub word: Range<usize>,
}

impl CommandLine {
    pub fn new(prompt: Prompt, tile: TileId) -> CommandLine {
        CommandLine {
            prompt,
            tile,
            error: None,
            words: Vec::new(),
            candidates: Vec::new(),
            highlighted: 0,
            word: 0..0,
        }
    }

    /// Re-rank for a new line and vocabulary.
    pub fn refresh(&mut self, line: &str, cursor: usize, words: Vec<String>) {
        self.word = word_at(line, cursor);
        self.words = words;
        self.candidates = rank_candidates(&self.words, &line[self.word.clone()]);
        self.highlighted = 0;
        self.error = None;
    }

    pub fn highlighted_word(&self) -> Option<&str> {
        self.candidates
            .get(self.highlighted)
            .map(|r| self.words[r.row].as_str())
    }

    pub fn step(&mut self, delta: i64) {
        if self.candidates.is_empty() {
            return;
        }
        let len = self.candidates.len() as i64;
        self.highlighted = ((self.highlighted as i64 + delta).rem_euclid(len)) as usize;
    }
}

fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || c == ','
}

/// The byte range of the word under `cursor` (a byte offset, as
/// `InputState::cursor` reports). Words are delimited by whitespace and
/// commas, so `:group lhu,und` completes `und`. Sitting right at the start
/// of a word — the char immediately behind the cursor is a delimiter, or
/// the cursor is at the very start of the line — is nothing typed of that
/// word yet, so the range is empty there rather than claiming the whole
/// word ahead; anywhere else the full word touching the cursor is
/// returned, extending forward past it to the word's real end.
pub fn word_at(line: &str, cursor: usize) -> Range<usize> {
    let mut cursor = cursor.min(line.len());
    // M4: `InputState::cursor` should always be on a char boundary, but
    // this pure core must not depend on that — clamp down to the nearest
    // boundary at or before it rather than panicking on the slices below.
    while !line.is_char_boundary(cursor) {
        cursor -= 1;
    }
    if line[..cursor].chars().next_back().is_none_or(is_delimiter) {
        return cursor..cursor;
    }
    let start = line[..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| is_delimiter(*c))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let end = line[cursor..]
        .char_indices()
        .find(|(_, c)| is_delimiter(*c))
        .map(|(i, _)| cursor + i)
        .unwrap_or(line.len());
    start..end
}

pub fn rank_candidates(words: &[String], word: &str) -> Vec<Ranked> {
    rank(words, word)
}

/// Replace `word` in `line` with `candidate`; the new cursor sits after it.
pub fn accept(line: &str, word: Range<usize>, candidate: &str) -> (String, usize) {
    let mut out = String::with_capacity(line.len() + candidate.len());
    out.push_str(&line[..word.start]);
    out.push_str(candidate);
    let cursor = out.len();
    out.push_str(&line[word.end..]);
    (out, cursor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKey {
    Next,
    Prev,
    Accept,
}

pub fn completion_key(ks: &Keystroke) -> Option<CompletionKey> {
    let plain = ks.mods == Modifiers::NONE;
    let ctrl = ks.mods == Modifiers::CTRL;
    match ks.key.as_str() {
        "tab" if plain => Some(CompletionKey::Accept),
        "down" if plain => Some(CompletionKey::Next),
        "up" if plain => Some(CompletionKey::Prev),
        "n" if ctrl => Some(CompletionKey::Next),
        "p" if ctrl => Some(CompletionKey::Prev),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submit {
    /// Run the line as typed.
    Run(String),
    /// One candidate matched a word that was not yet exact: the line
    /// with it accepted, and the new cursor. Run that.
    Accepted(String, usize),
    /// Several candidates and none exact: refuse, naming them.
    Ambiguous(Vec<String>),
}

/// Enter's decision (§3.4): an exact word or no candidates runs as typed;
/// exactly one candidate is accepted and run; more is refused. The line
/// never guesses.
pub fn resolve_submit(
    line: &str,
    cursor: usize,
    candidates: &[Ranked],
    words: &[String],
) -> Submit {
    let word = word_at(line, cursor);
    let typed = &line[word.clone()];
    if typed.is_empty() || candidates.is_empty() || words.iter().any(|w| w == typed) {
        return Submit::Run(line.to_string());
    }
    if candidates.len() == 1 {
        let (line, cursor) = accept(line, word, &words[candidates[0].row]);
        return Submit::Accepted(line, cursor);
    }
    Submit::Ambiguous(candidates.iter().map(|r| words[r.row].clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Modifiers;

    fn key(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::NONE,
            key: k.into(),
        }
    }
    fn ctrl(k: &str) -> Keystroke {
        Keystroke {
            mods: Modifiers::CTRL,
            key: k.into(),
        }
    }
    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_word_under_the_cursor_is_delimited_by_spaces_and_commas() {
        assert_eq!(word_at("sort del", 8), 5..8);
        assert_eq!(
            word_at("sort del", 5),
            5..5,
            "at the start of a word, the word is empty so far"
        );
        assert_eq!(
            word_at("sort delta01 desc", 9),
            5..12,
            "cursor inside a word"
        );
        assert_eq!(word_at("group lhu,und", 13), 10..13, "commas split");
        assert_eq!(word_at("", 0), 0..0);
        assert_eq!(word_at("sort ", 5), 5..5);
    }

    #[test]
    fn a_cursor_on_a_non_char_boundary_clamps_down_instead_of_panicking() {
        // M4: "café" — 'é' is a 2-byte UTF-8 char occupying bytes 8..10 of
        // this line, so byte 9 sits inside it. `InputState::cursor` should
        // always land on a boundary, but the pure core must not depend on
        // that: it clamps down to the nearest boundary at or before the
        // given cursor rather than panicking on the slice.
        let line = "sort café";
        assert_eq!(
            word_at(line, 9),
            5..line.len(),
            "clamped to the boundary before the multi-byte char, still inside the word"
        );
    }

    #[test]
    fn candidates_are_ranked_by_the_shared_fuzzy_matcher() {
        // "daily_delta_pnl" has a scattered `d...e...l` subsequence (via
        // its embedded "delta"), so it survives alongside the prefix
        // match; "gamma01" shares no `d` at all and is dropped.
        let ranked = rank_candidates(&words(&["delta01", "gamma01", "daily_delta_pnl"]), "del");
        assert_eq!(ranked[0].row, 0);
        assert_eq!(
            ranked.len(),
            2,
            "gamma01 has no d-e-l subsequence: {ranked:?}"
        );
        assert!(
            rank_candidates(&words(&["a"]), "").len() == 1,
            "an empty word keeps everything"
        );
    }

    #[test]
    fn accepting_replaces_the_word_and_puts_the_cursor_after_it() {
        assert_eq!(
            accept("sort del", 5..8, "delta01"),
            ("sort delta01".into(), 12)
        );
        assert_eq!(
            accept("sort d desc", 5..6, "delta01"),
            ("sort delta01 desc".into(), 12)
        );
        assert_eq!(accept("sort ", 5..5, "npv"), ("sort npv".into(), 8));
    }

    #[test]
    fn the_key_vocabulary() {
        assert_eq!(completion_key(&key("tab")), Some(CompletionKey::Accept));
        assert_eq!(completion_key(&ctrl("n")), Some(CompletionKey::Next));
        assert_eq!(completion_key(&key("down")), Some(CompletionKey::Next));
        assert_eq!(completion_key(&ctrl("p")), Some(CompletionKey::Prev));
        assert_eq!(completion_key(&key("up")), Some(CompletionKey::Prev));
        assert_eq!(completion_key(&key("a")), None);
    }

    #[test]
    fn submit_runs_accepts_or_refuses() {
        let w = words(&["delta01", "gamma01", "npv"]);
        let one = rank_candidates(&w, "np");
        assert_eq!(
            resolve_submit("sort np", 7, &one, &w),
            Submit::Accepted("sort npv".into(), 8)
        );
        let many = rank_candidates(&w, "a01");
        assert_eq!(
            resolve_submit("sort a01", 8, &many, &w),
            Submit::Ambiguous(words(&["delta01", "gamma01"]))
        );
        let exact = rank_candidates(&w, "npv");
        assert_eq!(
            resolve_submit("sort npv", 8, &exact, &w),
            Submit::Run("sort npv".into()),
            "an exact word runs"
        );
        assert_eq!(
            resolve_submit("sort npv desc", 13, &[], &w),
            Submit::Run("sort npv desc".into()),
            "no candidates: run as typed"
        );
        assert_eq!(
            resolve_submit("unpin", 5, &[], &[]),
            Submit::Run("unpin".into())
        );
    }
}
