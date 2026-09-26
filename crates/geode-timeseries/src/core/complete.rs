//! Series-name completion for the expression field: which name the caret
//! is in, which loaded names rank against it, and what Tab, Shift+Tab, a
//! click and Enter write. Pure; the tile owns the input and the focus.
//!
//! An expression may reference only a LOADED source series, so the
//! candidates are [`crate::core::Model::series_names`], ranked with the
//! `:` line's matcher. Word boundaries come from the expression
//! tokenizer's own character classes, so the completer never offers to
//! replace text the parser would read as an operator or a number.

use std::ops::Range;

use geode_core::series::expr::{is_ident_char, is_ident_start, is_source_char};
use geode_shell::commandline::{accept, rank_candidates};
use geode_shell::listfilter::Ranked;
use gpui::SharedString;

/// Rows the list paints at once. Cycling reaches every candidate; the
/// painted window follows the lit row.
pub const MAX_ROWS: usize = 8;

/// The byte range of the series name at `caret`, or `None` when the
/// caret is in a number, where no name can go.
///
/// A name is what the tokenizer reads as one reference: an identity
/// (`is_ident_start` then `is_ident_char`s) with an optional `@` and
/// `is_source_char`s, so `SPX.close/VI|` gives `VI` and
/// `(VIX@demo_re|` gives `VIX@demo_re`. A caret touching a name's end or
/// inside it gets the whole name; anywhere else (an empty field, after an
/// operator, a paren or a space, or at a name's very start) the range is
/// empty at the caret — nothing of a name typed yet.
pub fn name_at(line: &str, caret: usize) -> Option<Range<usize>> {
    let mut caret = caret.min(line.len());
    while !line.is_char_boundary(caret) {
        caret -= 1;
    }
    let bytes = line.as_bytes();
    let run = |mut i: usize, keep: fn(char) -> bool| {
        while i < bytes.len() && keep(bytes[i] as char) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    while i < line.len() {
        let c = line[i..].chars().next().unwrap_or_default();
        let start = i;
        if is_ident_start(c) {
            i = run(i, is_ident_char);
            if i < bytes.len() && bytes[i] == b'@' {
                i = run(i + 1, is_source_char);
            }
            if start < caret && caret <= i {
                return Some(start..i);
            }
        } else if c.is_ascii_digit() {
            i = run(i, |c| c.is_ascii_digit());
            if i < bytes.len() && bytes[i] == b'.' {
                i = run(i + 1, |c| c.is_ascii_digit());
            }
            if start < caret && caret <= i {
                return None;
            }
        } else {
            i += c.len_utf8();
        }
    }
    Some(caret..caret)
}

/// The completion list under the expression field: the loaded names,
/// the candidates ranked against the name at the caret, the lit row, and
/// the cached range a Tab replaces.
///
/// Rebuilt by [`Self::refresh`] on the input's Change event and on open,
/// never in render. A Tab, Shift+Tab or click writes through
/// `set_value`, which emits no Change, so the cached range is moved over
/// the written name here and a repeated Tab keeps cycling the same list.
#[derive(Debug, Default)]
pub struct Completion {
    names: Vec<String>,
    labels: Vec<SharedString>,
    candidates: Vec<Ranked>,
    /// The candidate lit in the list: the first until a Tab writes one,
    /// then the one written.
    highlighted: usize,
    /// The range of `line` a completion replaces; `None` in a number.
    token: Option<Range<usize>>,
    /// The candidate a Tab, Shift+Tab or click last wrote, which the next
    /// Tab or Shift+Tab steps from.
    written: Option<usize>,
}

impl Completion {
    /// Re-rank `names` against the name at `caret`, lighting the first
    /// candidate.
    pub fn refresh(&mut self, line: &str, caret: usize, names: Vec<String>) {
        self.token = name_at(line, caret);
        self.candidates = match &self.token {
            Some(token) => rank_candidates(&names, &line[token.clone()]),
            None => Vec::new(),
        };
        self.labels = names.iter().map(|n| n.clone().into()).collect();
        self.names = names;
        self.highlighted = 0;
        self.written = None;
    }

    /// Whether a Tab, Shift+Tab or click has written a candidate since the
    /// last refresh — the next Tab continues that cycle.
    pub fn cycling(&self) -> bool {
        self.written.is_some()
    }

    /// No series is loaded, so nothing can be referenced at all.
    pub fn nothing_loaded(&self) -> bool {
        self.names.is_empty()
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    /// The candidate names in rank order.
    pub fn candidates(&self) -> impl Iterator<Item = &str> {
        self.candidates.iter().map(|r| self.names[r.row].as_str())
    }

    /// The painted window: at most [`MAX_ROWS`] candidates, as
    /// `(candidate index, label)`, scrolled so the lit row is in it.
    pub fn painted(&self) -> impl Iterator<Item = (usize, &SharedString)> {
        let first = (self.highlighted + 1).saturating_sub(MAX_ROWS);
        self.candidates
            .iter()
            .enumerate()
            .skip(first)
            .take(MAX_ROWS)
            .map(|(i, r)| (i, &self.labels[r.row]))
    }

    /// Tab (`forward`) or Shift+Tab: write the next or previous candidate
    /// over the cached range and return the new line and caret. The first
    /// Tab writes the lit (first) candidate, the first Shift+Tab the
    /// last; either wraps. `None` with no candidates.
    pub fn cycle(&mut self, line: &str, forward: bool) -> Option<(String, usize)> {
        let n = self.candidates.len();
        if n == 0 {
            return None;
        }
        let i = match (self.written, forward) {
            (None, true) => self.highlighted,
            (None, false) => n - 1,
            (Some(w), true) => (w + 1) % n,
            (Some(w), false) => (w + n - 1) % n,
        };
        self.write(line, i)
    }

    /// A click on candidate `i`: the same write a Tab makes.
    pub fn pick(&mut self, line: &str, i: usize) -> Option<(String, usize)> {
        (i < self.candidates.len()).then_some(())?;
        self.write(line, i)
    }

    fn write(&mut self, line: &str, i: usize) -> Option<(String, usize)> {
        let token = self.token.clone()?;
        if token.end > line.len() || !line.is_char_boundary(token.start) {
            return None;
        }
        let name = &self.names[self.candidates[i].row];
        let (line, caret) = accept(line, token.clone(), name);
        self.token = Some(token.start..caret);
        self.written = Some(i);
        self.highlighted = i;
        Some((line, caret))
    }
}

/// Enter's expansion, the `:` line's rule: when the name at `caret` is
/// typed, is not exactly a loaded name, and exactly one loaded name
/// matches it, the line with that name written in and the new caret.
/// `None` means commit the text as typed. Computed from the live text and
/// caret rather than the cached list, which a caret move does not update.
pub fn expand_unique(line: &str, caret: usize, names: &[String]) -> Option<(String, usize)> {
    let token = name_at(line, caret)?;
    let typed = &line[token.clone()];
    if typed.is_empty() || names.iter().any(|n| n == typed) {
        return None;
    }
    match rank_candidates(names, typed).as_slice() {
        [one] => Some(accept(line, token, &names[one.row])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_name_at_the_caret_follows_the_tokenizer() {
        let line = "SPX.close/VI";
        assert_eq!(name_at(line, line.len()), Some(10..12), "`/` splits");
        let line = "(VIX@demo_re";
        assert_eq!(
            name_at(line, line.len()),
            Some(1..12),
            "`@source` is part of it"
        );
        let line = "VIX@demo-re";
        assert_eq!(
            name_at(line, line.len()),
            Some(0..11),
            "`-` is a source char after `@`"
        );
        let line = "VIX-SP";
        assert_eq!(
            name_at(line, line.len()),
            Some(4..6),
            "`-` before `@` is subtraction"
        );
        let line = "VIX@";
        assert_eq!(name_at(line, 4), Some(0..4), "an empty source so far");
        assert_eq!(name_at("", 0), Some(0..0), "empty field");
        assert_eq!(name_at("VIX * ", 6), Some(6..6), "after a space");
        assert_eq!(name_at("VIX*(", 5), Some(5..5), "after a paren");
        assert_eq!(name_at("VIX*", 4), Some(4..4), "after an operator");
        assert_eq!(
            name_at("SPX.close / VIX", 13),
            Some(12..15),
            "inside a name: all of it"
        );
        assert_eq!(
            name_at("VIX", 0),
            Some(0..0),
            "at a name's start: nothing typed"
        );
        assert_eq!(name_at("VIX * 2", 7), None, "a number takes no name");
        assert_eq!(name_at("VIX * 2.5", 9), None, "nor a decimal");
        assert_eq!(name_at("2X", 2), Some(1..2), "a digit run then a name");
        assert_eq!(name_at("é+V", 4), Some(3..4), "non-ASCII is skipped whole");
        assert_eq!(
            name_at("éV", 1),
            Some(0..0),
            "a caret inside a char clamps back"
        );
    }

    #[test]
    fn the_list_ranks_loaded_names_and_an_empty_name_offers_all() {
        let all = names(&["SPX.close", "VIX", "VIX@demo_rest", "V2X"]);
        let mut c = Completion::default();
        c.refresh("", 0, all.clone());
        assert_eq!(
            c.candidates().collect::<Vec<_>>(),
            all,
            "empty field: every name"
        );
        c.refresh("SPX.close / ", 12, all.clone());
        assert_eq!(c.candidate_count(), 4, "after an operator: every name");
        c.refresh("SPX.close / VI", 14, all.clone());
        assert_eq!(
            c.candidates().collect::<Vec<_>>(),
            vec!["VIX", "VIX@demo_rest"],
            "ranked by the `:` line's matcher"
        );
        assert_eq!(c.highlighted(), 0);
        c.refresh("VIX * 2", 7, all.clone());
        assert_eq!(c.candidate_count(), 0, "a number offers nothing");
        assert!(!c.nothing_loaded());
        c.refresh("", 0, Vec::new());
        assert!(c.nothing_loaded());
    }

    #[test]
    fn the_painted_window_holds_eight_and_follows_the_lit_row() {
        let many: Vec<String> = (0..12).map(|i| format!("S{i:02}")).collect();
        let mut c = Completion::default();
        c.refresh("", 0, many);
        let firsts = |c: &Completion| c.painted().map(|(i, _)| i).collect::<Vec<_>>();
        assert_eq!(firsts(&c), (0..8).collect::<Vec<_>>());
        let mut line = String::new();
        for _ in 0..10 {
            line = c.cycle(&line, true).unwrap().0;
        }
        assert_eq!(c.highlighted(), 9);
        assert_eq!(firsts(&c), (2..10).collect::<Vec<_>>());
    }

    #[test]
    fn tab_writes_and_cycles_the_cached_list_and_shift_tab_goes_back() {
        let all = names(&["SPX.close", "VIX", "VIX@demo_rest"]);
        let mut c = Completion::default();
        c.refresh("SPX.close / V", 13, all);
        let (line, caret) = c.cycle("SPX.close / V", true).unwrap();
        assert_eq!((line.as_str(), caret), ("SPX.close / VIX", 15));
        let (line, caret) = c.cycle(&line, true).unwrap();
        assert_eq!(
            (line.as_str(), caret),
            ("SPX.close / VIX@demo_rest", 25),
            "the next Tab replaces the written name, not the typed V"
        );
        let (line, _) = c.cycle(&line, true).unwrap();
        assert_eq!(line, "SPX.close / VIX", "wraps");
        let (line, _) = c.cycle(&line, false).unwrap();
        assert_eq!(line, "SPX.close / VIX@demo_rest", "Shift+Tab steps back");
        assert_eq!(c.highlighted(), 1, "the lit row is the written one");
        let mut three = Completion::default();
        three.refresh("V", 1, names(&["VIX", "V2X", "VXN"]));
        let (line, _) = three.cycle("V", true).unwrap();
        let (line, _) = three.cycle(&line, true).unwrap();
        assert_eq!(line, "V2X");
        let (line, _) = three.cycle(&line, false).unwrap();
        assert_eq!(line, "VIX", "Shift+Tab steps back one of three");
        let mut fresh = Completion::default();
        fresh.refresh("(V", 2, names(&["VIX", "V2X"]));
        assert_eq!(
            fresh.cycle("(V", false).unwrap(),
            ("(V2X".to_string(), 4),
            "a first Shift+Tab writes the last"
        );
        let mut mid = Completion::default();
        mid.refresh("V / SPX", 1, names(&["VIX"]));
        assert_eq!(
            mid.cycle("V / SPX", true).unwrap(),
            ("VIX / SPX".to_string(), 3),
            "mid-line: the caret lands after the name"
        );
        let mut none = Completion::default();
        none.refresh("VIX * 2", 7, names(&["VIX"]));
        assert_eq!(none.cycle("VIX * 2", true), None);
    }

    #[test]
    fn a_pick_writes_like_a_tab() {
        let mut c = Completion::default();
        c.refresh("SPX.close / ", 12, names(&["SPX.close", "VIX"]));
        assert_eq!(
            c.pick("SPX.close / ", 1).unwrap(),
            ("SPX.close / VIX".to_string(), 15)
        );
        assert_eq!(c.highlighted(), 1);
        assert_eq!(c.pick("SPX.close / VIX", 5), None, "out of range");
    }

    #[test]
    fn enter_expands_a_unique_inexact_name_only() {
        let all = names(&["SPX.close", "VIX", "VIX@demo_rest"]);
        assert_eq!(
            expand_unique("SPX.cl / VIX", 6, &all),
            Some(("SPX.close / VIX".to_string(), 9)),
            "one match: written in"
        );
        assert_eq!(
            expand_unique("SPX.close / VI", 14, &all),
            None,
            "two matches"
        );
        assert_eq!(expand_unique("SPX.close / VIX", 15, &all), None, "exact");
        assert_eq!(
            expand_unique("SPX.close / ", 12, &all),
            None,
            "nothing typed"
        );
        assert_eq!(
            expand_unique("VIX * 2", 7, &names(&["V2X"])),
            None,
            "a number"
        );
        assert_eq!(expand_unique("QQQ", 3, &all), None, "no match");
    }
}
