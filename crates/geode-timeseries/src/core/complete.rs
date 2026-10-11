//! Series-name completion for the expression field: which name the caret
//! is in, which loaded names rank against it, and what Tab, Shift+Tab, a
//! click and Enter write. Pure; the tile owns the input and the focus.
//!
//! An expression may reference only a loaded source series and call only
//! a grammar function, so the candidates are the loaded series names
//! ([`crate::core::Model::series_names`]) and the function names, each
//! with its `(`, ranked with the `:` line's matcher; series names rank
//! first on ties. Word boundaries come from the expression
//! tokenizer's own character classes, so the completer never offers to
//! replace text the parser would read as an operator or a number.

use std::ops::Range;

use geode_core::series::expr::{
    Function, INDEX_HELP, is_ident_char, is_ident_start, is_source_char,
};
use geode_shell::commandline::{accept, rank_candidates};
use geode_shell::listfilter::Ranked;
use gpui::SharedString;

/// Rows the list paints at once. Cycling reaches every candidate; the
/// painted window follows the lit row.
pub const MAX_ROWS: usize = 8;

/// Every function as the list offers it: its name with the `(` a call
/// needs, so writing one lands the caret inside the call.
pub fn function_names() -> impl Iterator<Item = String> {
    Function::ALL.iter().map(|f| format!("{}(", f.name()))
}

/// The byte range of the series name at `caret`, or `None` when the
/// caret is in a number, where no name can go.
///
/// A name is what the tokenizer reads as one reference: an identity
/// (`is_ident_start` then `is_ident_char`s) with an optional `@` and
/// `is_source_char`s, so `SPX.close/VI|` gives `VI` and
/// `(VIX@demo_re|` gives `VIX@demo_re`. A caret anywhere in a name, from
/// just before its first character to just after its last, gets the whole
/// name, so a Tab at `SPX.close/|VIX` completes `VIX` rather than gluing a
/// second name in front of it. Anywhere else (an empty field, after an
/// operator, a paren or a space) the range is empty at the caret. A caret
/// touching a number likewise takes no name.
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
            if start <= caret && caret <= i {
                return Some(start..i);
            }
        } else if c.is_ascii_digit() {
            i = run(i, |c| c.is_ascii_digit());
            if i < bytes.len() && bytes[i] == b'.' {
                i = run(i + 1, |c| c.is_ascii_digit());
            }
            if start <= caret && caret <= i {
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
/// Rebuilt by [`Self::refresh`] on the input's Change event, on open and
/// after Enter's expansion, never in render. A Tab, Shift+Tab or click
/// moves the cached range over the name it wrote and records the caret
/// after it, so a repeated Tab keeps cycling the same list; the echo of
/// that write is not a refresh (the tile skips it), and a caret that has
/// since moved is (see [`Self::stale_at`]).
#[derive(Debug, Default)]
pub struct Completion {
    names: Vec<String>,
    /// How many of `names` are series names; the rest are functions.
    series: usize,
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
    /// The caret just after that write.
    caret: Option<usize>,
}

/// One completion write: `name` over the byte `range` of the line it was
/// computed against. The tile applies it as a single range replace, so
/// it is one step of the input's undo history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Write {
    pub range: Range<usize>,
    pub name: String,
}

impl Write {
    /// The caret just after the written name.
    pub fn caret(&self) -> usize {
        self.range.start + self.name.len()
    }

    /// The line with the write applied, and the caret after it.
    pub fn apply(&self, line: &str) -> (String, usize) {
        accept(line, self.range.clone(), &self.name)
    }
}

/// Whether `range` can be sliced out of `line`: in bounds, ordered, and on
/// character boundaries at both ends. This prevents stale ranges from
/// panicking a listener; it does not verify that the text is unchanged.
fn fits(line: &str, range: &Range<usize>) -> bool {
    range.start <= range.end
        && range.end <= line.len()
        && line.is_char_boundary(range.start)
        && line.is_char_boundary(range.end)
}

impl Completion {
    /// Re-rank the series `names` and the function names against the
    /// name at `caret`, lighting the first candidate.
    pub fn refresh(&mut self, line: &str, caret: usize, names: Vec<String>) {
        let series = names.len();
        let mut names = names;
        names.extend(function_names());
        self.token = name_at(line, caret);
        self.candidates = match &self.token {
            Some(token) => rank_candidates(&names, &line[token.clone()]),
            None => Vec::new(),
        };
        self.labels = names.iter().map(|n| n.clone().into()).collect();
        self.names = names;
        self.series = series;
        self.highlighted = 0;
        self.written = None;
        self.caret = None;
    }

    /// Whether a Tab at `caret` must re-rank first: nothing has been
    /// written since the last refresh, or the caret has moved away from
    /// the end of the name last written. Otherwise the Tab continues the
    /// cycle over the cached list and range.
    pub fn stale_at(&self, caret: usize) -> bool {
        self.written.is_none() || self.caret != Some(caret)
    }

    /// Whether the list was ranked over `token`, the name at the live
    /// caret: a caret moved by arrows or a click does not re-rank, so a
    /// list ranked elsewhere says nothing about the name the caret now
    /// touches.
    pub fn ranked_at(&self, token: &Range<usize>) -> bool {
        self.token.as_ref() == Some(token)
    }

    /// No unambiguous source names were supplied. This can also happen when
    /// loaded source pairs are duplicated and cannot be named uniquely.
    pub fn nothing_loaded(&self) -> bool {
        self.series == 0
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

    /// Tab (`forward`) or Shift+Tab: the next or previous candidate over
    /// the cached range. The first Tab writes the lit (first) candidate,
    /// the first Shift+Tab the last; either wraps. `None` with no
    /// candidates, or when the cached range does not fit `line`.
    pub fn cycle(&mut self, line: &str, forward: bool) -> Option<Write> {
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
    pub fn pick(&mut self, line: &str, i: usize) -> Option<Write> {
        (i < self.candidates.len()).then_some(())?;
        self.write(line, i)
    }

    fn write(&mut self, line: &str, i: usize) -> Option<Write> {
        let token = self.token.clone()?;
        if !fits(line, &token) {
            return None;
        }
        let mut name = self.names[self.candidates[i].row].clone();
        // A function completed before its own `(` keeps that one: the
        // caret lands before it rather than between two.
        if name.ends_with('(') && line.as_bytes().get(token.end) == Some(&b'(') {
            name.pop();
        }
        let write = Write { range: token, name };
        self.token = Some(write.range.start..write.caret());
        self.caret = Some(write.caret());
        self.written = Some(i);
        self.highlighted = i;
        Some(write)
    }
}

/// Enter's expansion, the `:` line's rule: when the name at `caret` is
/// typed, is not exactly a loaded name, and exactly one loaded name
/// matches it, the write that puts that name in. `None` means commit the
/// text as typed. Computed from the live text and caret rather than the
/// cached list, which a caret move does not update.
pub fn expand_unique(line: &str, caret: usize, names: &[String]) -> Option<Write> {
    let token = name_at(line, caret)?;
    let typed = &line[token.clone()];
    if typed.is_empty() || names.iter().any(|n| n == typed) {
        return None;
    }
    match rank_candidates(names, typed).as_slice() {
        [one] => Some(Write {
            range: token,
            name: names[one.row].clone(),
        }),
        _ => None,
    }
}

/// The innermost function call the caret is inside, and which argument
/// the caret is in (0-based), or the index brackets it is inside.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enclosing {
    Call { function: Function, argument: usize },
    Index,
}

/// One open bracket between the line's start and the caret.
#[derive(Clone, Copy)]
enum Frame {
    Call {
        function: Function,
        argument: usize,
    },
    /// A bare `(`, or a call of a word that is not a function.
    Plain,
    Index,
}

/// Scan `line` to `caret` with the tokenizer's character classes: a word
/// immediately followed by `(` opens a call frame, a bare `(` (or one
/// after a word that is not a function) a plain frame, `[` an index
/// frame; `,` at the top of a call frame advances its argument; `)`/`]`
/// close. The innermost open call or index frame at the caret answers: a
/// plain frame nests for `)` matching but is transparent, so
/// `sma((A + B|), 3)` is still `sma`'s first argument. A comma inside a
/// plain frame advances nothing (the grammar never puts one there).
/// Nothing when the caret is at top level or the frames are all closed. A
/// caret past the end or inside a multi-byte character clamps back like
/// [`name_at`].
pub fn enclosing_at(line: &str, caret: usize) -> Option<Enclosing> {
    let mut caret = caret.min(line.len());
    while !line.is_char_boundary(caret) {
        caret -= 1;
    }
    let line = &line[..caret];
    let bytes = line.as_bytes();
    let mut frames: Vec<Frame> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if is_ident_start(c) {
            let start = i;
            while i < bytes.len() && is_ident_char(bytes[i] as char) {
                i += 1;
            }
            if bytes.get(i) == Some(&b'(') {
                frames.push(match Function::parse(&line[start..i]) {
                    Some(function) => Frame::Call {
                        function,
                        argument: 0,
                    },
                    None => Frame::Plain,
                });
                i += 1;
            }
            continue;
        }
        match c {
            '(' => frames.push(Frame::Plain),
            '[' => frames.push(Frame::Index),
            ')' | ']' => {
                frames.pop();
            }
            ',' => {
                if let Some(Frame::Call { argument, .. }) = frames.last_mut() {
                    *argument += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let innermost = frames.iter().rev().find(|f| !matches!(f, Frame::Plain));
    match innermost? {
        Frame::Call { function, argument } => Some(Enclosing::Call {
            function: *function,
            argument: *argument,
        }),
        Frame::Index => Some(Enclosing::Index),
        Frame::Plain => unreachable!("plain frames are filtered above"),
    }
}

/// What the help line shows: the parts of a signature around the active
/// argument, the result shape and the meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Help {
    /// Signature text before the active argument.
    pub before: String,
    /// The active argument's text (painted in the accent color), empty when none.
    pub active: String,
    /// Signature text after the active argument.
    pub after: String,
    /// `result · describe`, or the series note for a series row.
    pub tail: String,
}

impl Help {
    /// `f`'s signature with argument `i` marked, or with nothing marked
    /// for `None`. The argument names are the comma-separated pieces
    /// between the parens; a variadic signature (`min(series, …)`) marks
    /// its `…` for every argument past the named ones, and a fixed one
    /// marks nothing past its arity.
    fn call(f: Function, i: Option<usize>) -> Help {
        let sig = f.signature();
        let tail = format!("{} · {}", f.result(), f.describe());
        let open = sig.find('(').map(|p| p + 1).unwrap_or(sig.len());
        let close = sig.rfind(')').unwrap_or(sig.len());
        let args: Vec<&str> = sig[open..close].split(", ").collect();
        let variadic = args.last() == Some(&"…");
        let i = match i {
            Some(i) if i < args.len() => i,
            Some(_) if variadic => args.len() - 1,
            _ => {
                return Help {
                    before: sig.to_string(),
                    active: String::new(),
                    after: String::new(),
                    tail,
                };
            }
        };
        let before = format!("{}{}", &sig[..open], args[..i].join(", "));
        let before = if i > 0 { before + ", " } else { before };
        let after = if i + 1 < args.len() {
            format!(", {}{}", args[i + 1..].join(", "), &sig[close..])
        } else {
            sig[close..].to_string()
        };
        Help {
            before,
            active: args[i].to_string(),
            after,
            tail,
        }
    }
}

/// The help for the current field state, in priority: the lit completion
/// candidate while a name is being typed at the caret and the list was
/// ranked over it (`sma(` → sma's help with no active argument; a series
/// name → `before: name`, `tail: "series"`), else the enclosing call with
/// its active argument, else the index note, else `None`. With nothing
/// typed at the caret the list offers every name, so its first row says
/// nothing about the place the caret is in; the enclosing call does. The
/// same holds for a list ranked where the caret was before an arrow key
/// moved it ([`Completion::ranked_at`]).
pub fn help_for(completion: &Completion, line: &str, caret: usize) -> Option<Help> {
    let typing = name_at(line, caret).is_some_and(|r| !r.is_empty() && completion.ranked_at(&r));
    let lit = completion
        .candidates()
        .nth(completion.highlighted())
        .filter(|_| typing);
    if let Some(name) = lit {
        return Some(match name.strip_suffix('(').and_then(Function::parse) {
            Some(f) => Help::call(f, None),
            None => Help {
                before: name.to_string(),
                active: String::new(),
                after: String::new(),
                tail: "series".to_string(),
            },
        });
    }
    match enclosing_at(line, caret)? {
        Enclosing::Call { function, argument } => Some(Help::call(function, Some(argument))),
        Enclosing::Index => {
            let (before, tail) = INDEX_HELP.split_once(" · ")?;
            Some(Help {
                before: before.to_string(),
                active: String::new(),
                after: String::new(),
                tail: tail.to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geode_core::series::expr::Function;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    /// A Tab or Shift+Tab applied to `line`, as the tile applies it.
    fn tab(c: &mut Completion, line: &str, forward: bool) -> Option<(String, usize)> {
        c.cycle(line, forward).map(|w| w.apply(line))
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
            Some(0..3),
            "at a name's start: that name"
        );
        assert_eq!(
            name_at("SPX.close/VIX", 10),
            Some(10..13),
            "at a name's start after an operator: that name, not the one before"
        );
        assert_eq!(name_at("VIX * 2", 7), None, "a number takes no name");
        assert_eq!(name_at("VIX * 2", 6), None, "nor at its start");
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
        let every = all.len() + Function::ALL.len();
        c.refresh("", 0, all.clone());
        assert_eq!(
            c.candidates().take(all.len()).collect::<Vec<_>>(),
            all,
            "empty field: every series name first"
        );
        assert_eq!(c.candidate_count(), every, "then every function");
        c.refresh("SPX.close / ", 12, all.clone());
        assert_eq!(c.candidate_count(), every, "after an operator: every name");
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
            line = tab(&mut c, &line, true).unwrap().0;
        }
        assert_eq!(c.highlighted(), 9);
        assert_eq!(firsts(&c), (2..10).collect::<Vec<_>>());
    }

    #[test]
    fn tab_writes_and_cycles_the_cached_list_and_shift_tab_goes_back() {
        let all = names(&["SPX.close", "VIX", "VIX@demo_rest"]);
        let mut c = Completion::default();
        c.refresh("SPX.close / V", 13, all);
        let (line, caret) = tab(&mut c, "SPX.close / V", true).unwrap();
        assert_eq!((line.as_str(), caret), ("SPX.close / VIX", 15));
        let (line, caret) = tab(&mut c, &line, true).unwrap();
        assert_eq!(
            (line.as_str(), caret),
            ("SPX.close / VIX@demo_rest", 25),
            "the next Tab replaces the written name, not the typed V"
        );
        let (line, _) = tab(&mut c, &line, true).unwrap();
        assert_eq!(line, "SPX.close / VIX", "wraps");
        let (line, _) = tab(&mut c, &line, false).unwrap();
        assert_eq!(line, "SPX.close / VIX@demo_rest", "Shift+Tab steps back");
        assert_eq!(c.highlighted(), 1, "the lit row is the written one");
        let mut three = Completion::default();
        three.refresh("V", 1, names(&["VIX", "V2X", "VXN"]));
        let (line, _) = tab(&mut three, "V", true).unwrap();
        let (line, _) = tab(&mut three, &line, true).unwrap();
        assert_eq!(line, "V2X");
        let (line, _) = tab(&mut three, &line, false).unwrap();
        assert_eq!(line, "VIX", "Shift+Tab steps back one of three");
        let mut fresh = Completion::default();
        fresh.refresh("(V", 2, names(&["VIX", "V2X"]));
        assert_eq!(
            tab(&mut fresh, "(V", false).unwrap(),
            ("(V2X".to_string(), 4),
            "a first Shift+Tab writes the last"
        );
        let mut mid = Completion::default();
        mid.refresh("V / SPX", 1, names(&["VIX"]));
        assert_eq!(
            tab(&mut mid, "V / SPX", true).unwrap(),
            ("VIX / SPX".to_string(), 3),
            "mid-line: the caret lands after the name"
        );
        let mut none = Completion::default();
        none.refresh("VIX * 2", 7, names(&["VIX"]));
        assert_eq!(tab(&mut none, "VIX * 2", true), None);
    }

    /// After a write the cycle continues only while the caret stays where
    /// that write left it.
    #[test]
    fn a_moved_caret_makes_the_cycle_stale() {
        let mut c = Completion::default();
        c.refresh("S / V", 5, names(&["SPX.close", "VIX"]));
        assert!(c.stale_at(5), "nothing written yet");
        let w = c.cycle("S / V", true).unwrap();
        assert_eq!(w.caret(), 7);
        assert!(!c.stale_at(7), "the caret the write left");
        assert!(c.stale_at(1), "moved away");
    }

    #[test]
    fn a_pick_writes_like_a_tab() {
        let mut c = Completion::default();
        c.refresh("SPX.close / ", 12, names(&["SPX.close", "VIX"]));
        assert_eq!(
            c.pick("SPX.close / ", 1).unwrap().apply("SPX.close / "),
            ("SPX.close / VIX".to_string(), 15)
        );
        assert_eq!(c.highlighted(), 1);
        assert_eq!(
            c.pick("SPX.close / VIX", 2 + Function::ALL.len()),
            None,
            "out of range"
        );
    }

    /// A range cached against other text is refused, never sliced: its
    /// end inside a multi-byte character would panic `accept`.
    #[test]
    fn a_range_that_does_not_fit_the_line_writes_nothing() {
        let mut c = Completion::default();
        c.refresh("xx A", 4, names(&["ABC"]));
        assert_eq!(c.pick("xx é", 0), None, "the end falls inside é");
        let mut c = Completion::default();
        c.refresh("xx AB", 5, names(&["ABC"]));
        assert_eq!(c.pick("xx", 0), None, "past the end");
    }

    #[test]
    fn functions_complete_beside_series_names_with_their_paren() {
        let all = names(&["SPX.close", "VIX"]);
        let mut c = Completion::default();
        c.refresh("", 0, all.clone());
        let cands: Vec<&str> = c.candidates().collect();
        assert_eq!(&cands[..2], &["SPX.close", "VIX"], "series first");
        assert_eq!(cands.len(), 2 + Function::ALL.len());
        assert!(cands.contains(&"sma("));
        c.refresh("VIX / sm", 8, all.clone());
        assert_eq!(c.candidates().next(), Some("sma("));
        assert!(
            c.candidates().all(|n| n.ends_with('(')),
            "no series fits `sm`"
        );
        let w = c.cycle("VIX / sm", true).unwrap();
        assert_eq!(
            w.apply("VIX / sm"),
            ("VIX / sma(".to_string(), 10),
            "the caret lands inside the call"
        );
        assert!(!c.nothing_loaded());
        c.refresh("", 0, vec![]);
        assert!(
            c.nothing_loaded(),
            "functions alone are nothing to reference"
        );
        assert_eq!(
            expand_unique("VIX / sm", 8, &all),
            None,
            "Enter's expansion is over series names only"
        );
    }

    /// Completing a function name whose `(` is already typed writes the
    /// name alone, so the caret lands before the existing paren rather
    /// than doubling it.
    #[test]
    fn a_function_before_its_own_paren_completes_without_a_second_one() {
        let mut c = Completion::default();
        c.refresh("sma(A, 3)", 2, names(&["A"]));
        let w = c.cycle("sma(A, 3)", true).unwrap();
        assert_eq!(w.range, 0..3);
        assert_eq!(w.name, "sma");
        assert_eq!(w.apply("sma(A, 3)"), ("sma(A, 3)".to_string(), 3));
        let mut c = Completion::default();
        c.refresh("VIX / sm", 8, names(&["VIX"]));
        let w = c.cycle("VIX / sm", true).unwrap();
        assert_eq!(w.name, "sma(", "no paren follows: written with its own");
    }

    #[test]
    fn enter_expands_a_unique_inexact_name_only() {
        let all = names(&["SPX.close", "VIX", "VIX@demo_rest"]);
        assert_eq!(
            expand_unique("SPX.cl / VIX", 6, &all).map(|w| w.apply("SPX.cl / VIX")),
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

    /// `line` with a `|` marking the caret, as the cases below spell it.
    fn at(marked: &str) -> Option<Enclosing> {
        let caret = marked.find('|').expect("a caret mark");
        let line = marked.replacen('|', "", 1);
        enclosing_at(&line, caret)
    }

    fn call(function: Function, argument: usize) -> Option<Enclosing> {
        Some(Enclosing::Call { function, argument })
    }

    #[test]
    fn the_enclosing_call_is_the_innermost_open_frame_at_the_caret() {
        assert_eq!(at("sma(VI|"), call(Function::Sma, 0));
        assert_eq!(at("sma(VIX, |"), call(Function::Sma, 1));
        assert_eq!(at("sma(VIX, 20)|"), None, "closed");
        assert_eq!(at("sma(diff(A|), 3)"), call(Function::Diff, 0), "innermost");
        assert_eq!(
            at("sma(diff(A)|, 3)"),
            call(Function::Sma, 0),
            "the inner call closed: back in sma's first argument"
        );
        assert_eq!(at("(A + sma(B, |2))"), call(Function::Sma, 1));
        assert_eq!(at("foo(A|"), None, "an unknown word opens a plain frame");
        assert_eq!(
            at("sma((A + B|), 3)"),
            call(Function::Sma, 0),
            "a bare paren is transparent: still sma's first argument"
        );
        assert_eq!(
            at("sma((A, B|), 3)"),
            call(Function::Sma, 0),
            "a comma inside a plain frame does not advance the call's argument"
        );
        assert_eq!(
            at("sma((A + B), |3)"),
            call(Function::Sma, 1),
            "the plain frame closed; the comma at sma's top advances"
        );
        assert_eq!(at("foo(sma(A|))"), call(Function::Sma, 0));
        assert_eq!(
            at("sma(foo(A|), 3)"),
            call(Function::Sma, 0),
            "an unknown call is transparent too"
        );
        assert_eq!(at("A[|"), Some(Enclosing::Index));
        assert_eq!(at("A[-1]|"), None);
        assert_eq!(at("max(A, B, |"), call(Function::Max, 2));
        assert_eq!(at("sma |(A"), None, "a space before the paren: not a call");
        assert_eq!(at("sma|(A"), None, "before the paren: not inside the call");
        assert_eq!(at("|sma(A"), None, "top level");
        assert_eq!(enclosing_at("", 0), None);
        assert_eq!(
            enclosing_at("sma(é", 5),
            call(Function::Sma, 0),
            "a caret inside a multi-byte char clamps back"
        );
        assert_eq!(
            enclosing_at("sma(A", 99),
            call(Function::Sma, 0),
            "past the end"
        );
    }

    fn help(before: &str, active: &str, after: &str, tail: &str) -> Option<Help> {
        Some(Help {
            before: before.into(),
            active: active.into(),
            after: after.into(),
            tail: tail.into(),
        })
    }

    const SMA_TAIL: &str = "series · mean of the last n points; blank unless all n have a value";

    #[test]
    fn help_marks_the_active_argument_of_the_enclosing_call() {
        let none = Completion::default();
        assert_eq!(
            help_for(&none, "sma(VIX, 2", 10),
            help("sma(series, ", "n", ")", SMA_TAIL)
        );
        assert_eq!(
            help_for(&none, "sma(VIX", 7),
            help("sma(", "series", ", n)", SMA_TAIL)
        );
        assert_eq!(
            help_for(&none, "min(A, B, 2", 11),
            help(
                "min(series, ",
                "…",
                ")",
                "number or series · the smallest: over the range alone, per point with more arguments"
            ),
            "past the named arguments of a variadic call: the ellipsis"
        );
        assert_eq!(
            help_for(&none, "sma(A, 2, 3", 11),
            help("sma(series, n)", "", "", SMA_TAIL),
            "past the arity: the signature with nothing active"
        );
        assert_eq!(
            help_for(&none, "A[2", 3),
            help(
                "A[k]",
                "",
                "",
                "number · the point at offset k, 0 the first, from the end when k is negative"
            )
        );
        assert_eq!(help_for(&none, "A + 2", 5), None, "top level, in a number");
    }

    /// The lit candidate wins while a name is being typed; with nothing
    /// typed at the caret (the list then offers every name) the enclosing
    /// call answers, so `sma(VIX, |` describes `n`, not the first series.
    #[test]
    fn help_describes_the_lit_candidate_while_a_name_is_typed() {
        let all = names(&["SPX.close", "VIX"]);
        let mut c = Completion::default();
        c.refresh("sma(VI", 6, all.clone());
        assert_eq!(c.candidates().next(), Some("VIX"));
        assert_eq!(
            help_for(&c, "sma(VI", 6),
            help("VIX", "", "", "series"),
            "a lit series row"
        );
        c.refresh("VIX / sm", 8, all.clone());
        assert_eq!(c.candidates().next(), Some("sma("));
        assert_eq!(
            help_for(&c, "VIX / sm", 8),
            help("sma(series, n)", "", "", SMA_TAIL),
            "a lit function row: its signature with no active argument"
        );
        c.refresh("sma(VIX, ", 9, all.clone());
        assert!(c.candidate_count() > 0, "an empty name offers every name");
        assert_eq!(
            help_for(&c, "sma(VIX, ", 9),
            help("sma(series, ", "n", ")", SMA_TAIL),
            "nothing typed at the caret: the enclosing call, not the first row"
        );
        assert_eq!(
            help_for(&c, "sma(VIX, ", 7),
            help("sma(", "series", ", n)", SMA_TAIL),
            "the caret moved back onto VIX without a re-rank: the list is stale, the call answers"
        );
        c.refresh("QQQ", 3, all);
        assert_eq!(c.candidate_count(), 0);
        assert_eq!(help_for(&c, "QQQ", 3), None, "no candidate, no call");
    }
}
