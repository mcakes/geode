//! Entry-bar completion: which slot of the shorthand the caret is in,
//! what a write there replaces, the ranked suggestions and the hint.
//! Pure; the tile owns the input, the focus and the provider.
//!
//! Slots follow `shorthand::parse`'s order: an optional leading signed
//! integer, then underlying, expiry, strikes, type, and, after a single
//! `C` or `P` leg, a barrier kind and level. Expiries and strikes take
//! `/`-separated parts, so a write there replaces only the part at the
//! caret.

use std::ops::Range;

use chrono::{Datelike, NaiveDate};
use geode_shell::listfilter;
use gpui::SharedString;

use crate::core::shorthand::{IMM_MONTHS, MONTH_NAMES, third_friday};
use crate::core::template::TemplateSet;

pub use geode_shell::exprcomplete::Write;

/// Rows the list paints at once; cycling reaches every candidate.
pub const MAX_ROWS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Qty,
    Underlying,
    Expiry,
    Strikes,
    Type,
    BarrierKind,
    BarrierLevel,
    /// After the last slot the line can hold.
    Past,
}

pub struct Inputs<'a> {
    pub templates: &'a TemplateSet,
    pub underlyings: &'a [SharedString],
    pub today: NaiveDate,
}

/// One row: what a write puts in, and a muted detail beside it.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub label: SharedString,
    pub detail: SharedString,
}

fn tokens(line: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(s..line.len());
    }
    out
}

/// A leading quantity as typed so far: a sign, digits, or both.
fn is_qty(t: &str) -> bool {
    let digits = t.strip_prefix(['-', '+']).unwrap_or(t);
    !t.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn is_single_leg(t: &str) -> bool {
    t.eq_ignore_ascii_case("C") || t.eq_ignore_ascii_case("P")
}

/// The part of `token` between the `/`s around `caret`.
fn slash_part(line: &str, token: Range<usize>, caret: usize) -> Range<usize> {
    let text = &line[token.clone()];
    let at = caret - token.start;
    let start = text[..at].rfind('/').map_or(0, |i| i + 1);
    let end = text[at..].find('/').map_or(text.len(), |i| at + i);
    token.start + start..token.start + end
}

/// The slot the caret is in and the byte range a write there replaces.
/// A caret anywhere in a token, including just after its last character,
/// is in that token; a caret in whitespace is in the next slot with an
/// empty range.
pub fn slot_at(line: &str, caret: usize) -> (Slot, Range<usize>) {
    let mut caret = caret.min(line.len());
    while !line.is_char_boundary(caret) {
        caret -= 1;
    }
    let toks = tokens(line);
    let (k, range) = match toks.iter().position(|t| t.start <= caret && caret <= t.end) {
        Some(k) => (k, toks[k].clone()),
        None => (toks.iter().filter(|t| t.end < caret).count(), caret..caret),
    };
    let text = |r: &Range<usize>| &line[r.clone()];
    if k == 0 && is_qty(text(&range)) {
        return (Slot::Qty, range);
    }
    let qty = toks.first().is_some_and(|t| k > 0 && is_qty(text(t)));
    let index = k - usize::from(qty);
    let type_tok = toks.get(3 + usize::from(qty));
    let slot = match index {
        0 => Slot::Underlying,
        1 => Slot::Expiry,
        2 => Slot::Strikes,
        3 => Slot::Type,
        4 if type_tok.is_some_and(|t| is_single_leg(text(t))) => Slot::BarrierKind,
        5 if type_tok.is_some_and(|t| is_single_leg(text(t))) => Slot::BarrierLevel,
        _ => Slot::Past,
    };
    let range = match slot {
        Slot::Expiry | Slot::Strikes if !range.is_empty() => slash_part(line, range, caret),
        _ => range,
    };
    (slot, range)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn suggestions(slot: Slot, inputs: &Inputs) -> Vec<Suggestion> {
    let s = |label: String, detail: String| Suggestion {
        label: label.into(),
        detail: detail.into(),
    };
    match slot {
        Slot::Underlying => inputs
            .underlyings
            .iter()
            .map(|u| Suggestion {
                label: u.clone(),
                detail: SharedString::default(),
            })
            .collect(),
        Slot::Expiry => {
            let mut out = Vec::with_capacity(12);
            let (mut y, mut m) = (inputs.today.year(), inputs.today.month());
            while out.len() < 8 {
                if let Some(d) = third_friday(y, m).filter(|d| *d >= inputs.today) {
                    let yy = y.rem_euclid(100);
                    out.push(s(
                        format!("{}{yy:02}", IMM_MONTHS[m as usize - 1]),
                        format!(
                            "{}{yy:02} · {}",
                            MONTH_NAMES[m as usize - 1],
                            d.format("%-d %b %Y")
                        ),
                    ));
                }
                (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            }
            out.extend(["1m", "3m", "6m", "1y"].map(|t| s(t.into(), "tenor".into())));
            out
        }
        Slot::Type => {
            let mut out = vec![s("C".into(), "call".into()), s("P".into(), "put".into())];
            out.extend(inputs.templates.iter().map(|d| {
                let mut detail = plural(d.strikes, "strike", "strikes");
                if d.expiries > 1 {
                    detail.push_str(&format!(" · {}", plural(d.expiries, "expiry", "expiries")));
                }
                s(d.name.clone(), detail)
            }));
            out
        }
        Slot::BarrierKind => ["UI", "UO", "DI", "DO"]
            .map(|k| s(k.into(), String::new()))
            .into_iter()
            .collect(),
        Slot::Qty | Slot::Strikes | Slot::BarrierLevel | Slot::Past => Vec::new(),
    }
}

fn hint(slot: Slot, line: &str, caret: usize, inputs: &Inputs) -> String {
    match slot {
        Slot::Qty | Slot::Underlying => "UNDERLYING  after an optional signed quantity".into(),
        Slot::Expiry => "EXPIRY  Z26 · DEC26 · 20DEC26 · 3m".into(),
        Slot::Strikes => {
            // In the strikes slot the next token after the caret is the type.
            let toks = tokens(line);
            let after = toks.iter().find(|t| t.start > caret);
            match after.and_then(|t| inputs.templates.resolve(&line[t.clone()])) {
                Some(d) if d.strikes == 1 => format!("STRIKES  K for {}", d.name),
                Some(d) => {
                    let ks: Vec<String> = (1..=d.strikes).map(|i| format!("K{i}")).collect();
                    format!("STRIKES  {} for {}", ks.join("/"), d.name)
                }
                None => "STRIKES  K or K1/K2/…".into(),
            }
        }
        Slot::Type => "TYPE  C · P · or a template".into(),
        Slot::BarrierKind => "BARRIER  UI · UO · DI · DO, optional".into(),
        Slot::BarrierLevel => "LEVEL  the barrier level".into(),
        Slot::Past => String::new(),
    }
}

#[derive(Debug, Default)]
pub struct Completion {
    slot: Option<Slot>,
    all: Vec<Suggestion>,
    /// Indices into `all`, in rank order.
    ranked: Vec<usize>,
    highlighted: usize,
    token: Option<Range<usize>>,
    written: Option<usize>,
    caret: Option<usize>,
    hint: SharedString,
    no_underlyings: bool,
}

fn fits(line: &str, r: &Range<usize>) -> bool {
    r.start <= r.end
        && r.end <= line.len()
        && line.is_char_boundary(r.start)
        && line.is_char_boundary(r.end)
}

impl Completion {
    /// Re-rank at `caret`, lighting the first candidate. Runs on every
    /// edit, history step, reload and stale Tab; never in render.
    pub fn refresh(&mut self, line: &str, caret: usize, inputs: &Inputs) {
        let (slot, range) = slot_at(line, caret);
        self.all = suggestions(slot, inputs);
        let texts: Vec<String> = self
            .all
            .iter()
            .map(|s| {
                if s.detail.is_empty() {
                    s.label.to_string()
                } else {
                    format!("{} {}", s.label, s.detail)
                }
            })
            .collect();
        self.ranked = listfilter::rank(&texts, &line[range.clone()])
            .into_iter()
            .map(|r| r.row)
            .collect();
        self.no_underlyings = slot == Slot::Underlying && inputs.underlyings.is_empty();
        self.hint = hint(slot, line, caret, inputs).into();
        self.slot = Some(slot);
        self.token = Some(range);
        self.highlighted = 0;
        self.written = None;
        self.caret = None;
    }

    pub fn stale_at(&self, caret: usize) -> bool {
        self.written.is_none() || self.caret != Some(caret)
    }

    pub fn slot(&self) -> Option<Slot> {
        self.slot
    }

    pub fn hint(&self) -> &SharedString {
        &self.hint
    }

    pub fn no_underlyings(&self) -> bool {
        self.no_underlyings
    }

    pub fn candidate_count(&self) -> usize {
        self.ranked.len()
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    pub fn painted(&self) -> impl Iterator<Item = (usize, &Suggestion)> {
        let first = (self.highlighted + 1).saturating_sub(MAX_ROWS);
        self.ranked
            .iter()
            .enumerate()
            .skip(first)
            .take(MAX_ROWS)
            .map(|(i, r)| (i, &self.all[*r]))
    }

    pub fn cycle(&mut self, line: &str, forward: bool) -> Option<Write> {
        let n = self.ranked.len();
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

    pub fn pick(&mut self, line: &str, i: usize) -> Option<Write> {
        (i < self.ranked.len()).then_some(())?;
        self.write(line, i)
    }

    fn write(&mut self, line: &str, i: usize) -> Option<Write> {
        let token = self.token.clone()?;
        if !fits(line, &token) {
            return None;
        }
        let text = self.all[self.ranked[i]].label.to_string();
        let end = token.start + text.len();
        let write = Write {
            range: token.clone(),
            text,
        };
        self.token = Some(token.start..end);
        self.caret = Some(end);
        self.written = Some(i);
        self.highlighted = i;
        Some(write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    fn unds(names: &[&str]) -> Vec<SharedString> {
        names
            .iter()
            .map(|n| SharedString::from(n.to_string()))
            .collect()
    }

    fn refreshed(line: &str, caret: usize, u: &[SharedString]) -> Completion {
        let set = TemplateSet::builtin();
        let mut c = Completion::default();
        c.refresh(
            line,
            caret,
            &Inputs {
                templates: &set,
                underlyings: u,
                today: today(),
            },
        );
        c
    }

    fn labels(c: &Completion) -> Vec<String> {
        c.painted().map(|(_, s)| s.label.to_string()).collect()
    }

    #[test]
    fn the_slot_follows_parse_order_with_an_optional_qty() {
        let cases: &[(&str, usize, Slot, std::ops::Range<usize>)] = &[
            ("", 0, Slot::Underlying, 0..0),
            ("SP", 2, Slot::Underlying, 0..2),
            ("-5", 2, Slot::Qty, 0..2),
            ("-", 1, Slot::Qty, 0..1),
            ("-5 ", 3, Slot::Underlying, 3..3),
            ("-5 SPX Z2", 9, Slot::Expiry, 7..9),
            ("SPX Z26/H2", 10, Slot::Expiry, 8..10),
            ("SPX Z26/H27 ", 12, Slot::Strikes, 12..12),
            ("SPX Z26 4800/52", 15, Slot::Strikes, 13..15),
            ("SPX Z26 4800/5200 C", 19, Slot::Type, 18..19),
            ("SPX Z26 5000 C ", 15, Slot::BarrierKind, 15..15),
            ("SPX Z26 5000 C UO ", 18, Slot::BarrierLevel, 18..18),
            ("SPX Z26 4800/5200 CS ", 21, Slot::Past, 21..21),
            ("SPX Z26 5000 C UO 5500 ", 23, Slot::Past, 23..23),
            ("SPX Z26 5000 C", 1, Slot::Underlying, 0..3),
            ("SPX  Z26", 4, Slot::Expiry, 4..4),
        ];
        for (line, caret, slot, range) in cases {
            assert_eq!(
                slot_at(line, *caret),
                (*slot, range.clone()),
                "{line:?} at {caret}"
            );
        }
    }

    #[test]
    fn a_caret_inside_a_character_clamps_back() {
        let line = "SPé";
        assert_eq!(slot_at(line, 3), (Slot::Underlying, 0..4));
    }

    #[test]
    fn underlyings_rank_in_provider_order_and_an_empty_provider_says_so() {
        let u = unds(&["SPX", "SX5E", "NDX"]);
        assert_eq!(labels(&refreshed("", 0, &u)), ["SPX", "SX5E", "NDX"]);
        assert_eq!(labels(&refreshed("SX", 2, &u))[0], "SX5E");
        let empty = refreshed("", 0, &[]);
        assert!(empty.no_underlyings());
        assert_eq!(empty.candidate_count(), 0);
    }

    #[test]
    fn expiries_are_the_next_eight_months_then_tenors() {
        let c = refreshed("SPX ", 4, &[]);
        let l = labels(&c);
        // 27 Sep 2026: September's third Friday (18th) has passed.
        assert_eq!(
            l,
            ["V26", "X26", "Z26", "F27", "G27", "H27", "J27", "K27"],
            "the painted window holds eight"
        );
        assert_eq!(c.candidate_count(), 12, "eight months and four tenors");
        let z = c
            .painted()
            .find(|(_, s)| s.label.as_ref() == "Z26")
            .unwrap()
            .1;
        assert_eq!(z.detail.as_ref(), "DEC26 · 18 Dec 2026");
        assert_eq!(
            labels(&refreshed("SPX dec", 7, &[]))[0],
            "Z26",
            "the month form ranks"
        );
    }

    #[test]
    fn a_month_whose_third_friday_is_today_still_counts() {
        let set = TemplateSet::builtin();
        let mut c = Completion::default();
        let friday = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        c.refresh(
            "SPX ",
            4,
            &Inputs {
                templates: &set,
                underlyings: &[],
                today: friday,
            },
        );
        assert_eq!(labels(&c)[0], "U26");
    }

    #[test]
    fn types_are_c_p_then_templates_with_their_signature() {
        let c = refreshed("SPX Z26 5000 ", 13, &[]);
        let rows: Vec<(String, String)> = c
            .painted()
            .map(|(_, s)| (s.label.to_string(), s.detail.to_string()))
            .collect();
        assert_eq!(rows[0], ("C".into(), "call".into()));
        assert_eq!(rows[1], ("P".into(), "put".into()));
        assert_eq!(rows[2], ("CS".into(), "2 strikes".into()));
        // Seven builtin templates put CAL ninth, past the painted window;
        // typing brings it into view.
        assert_eq!(c.candidate_count(), 9);
        let cal = refreshed("SPX Z26 5000 CAL", 16, &[]);
        assert!(cal.painted().any(
            |(_, s)| (s.label.as_ref(), s.detail.as_ref()) == ("CAL", "1 strike · 2 expiries")
        ));
    }

    #[test]
    fn barrier_kinds_follow_only_a_single_leg() {
        assert_eq!(
            labels(&refreshed("SPX Z26 5000 C ", 15, &[])),
            ["UI", "UO", "DI", "DO"]
        );
        assert_eq!(
            refreshed("SPX Z26 4800/5200 CS ", 21, &[]).candidate_count(),
            0
        );
    }

    #[test]
    fn the_hint_names_the_slot_and_the_strike_count_of_the_type() {
        let hint = |line: &str| refreshed(line, line.len(), &[]).hint().to_string();
        assert_eq!(hint("SPX Z26 "), "STRIKES  K or K1/K2/…");
        let set = TemplateSet::builtin();
        let mut c = Completion::default();
        let line = "SPX Z26  FLY";
        c.refresh(
            line,
            8,
            &Inputs {
                templates: &set,
                underlyings: &[],
                today: today(),
            },
        );
        assert_eq!(
            c.hint().as_ref(),
            "STRIKES  K1/K2/K3 for FLY",
            "the type after the caret"
        );
        assert_eq!(hint("SPX "), "EXPIRY  Z26 · DEC26 · 20DEC26 · 3m");
        assert_eq!(hint(""), "UNDERLYING  after an optional signed quantity");
        assert_eq!(hint("SPX Z26 5000 "), "TYPE  C · P · or a template");
        assert_eq!(hint("SPX Z26 4800/5200 CS "), "");
    }

    #[test]
    fn tab_writes_cycles_and_shift_tab_goes_back_over_the_slash_part() {
        let mut c = refreshed("SPX Z26/", 8, &[]);
        let line = "SPX Z26/";
        let w = c.cycle(line, true).unwrap();
        let (line, caret) = w.apply(line);
        assert_eq!((line.as_str(), caret), ("SPX Z26/V26", 11));
        let w = c.cycle(&line, true).unwrap();
        let (line, _) = w.apply(&line);
        assert_eq!(
            line, "SPX Z26/X26",
            "the next Tab replaces the written part"
        );
        let w = c.cycle(&line, false).unwrap();
        assert_eq!(w.apply(&line).0, "SPX Z26/V26");
        let mut fresh = refreshed("SPX ", 4, &[]);
        assert_eq!(
            fresh.cycle("SPX ", false).unwrap().apply("SPX ").0,
            "SPX 1y",
            "a first Shift+Tab writes the last"
        );
    }

    #[test]
    fn a_moved_caret_is_stale_and_a_pick_writes_like_a_tab() {
        let u = unds(&["SPX", "SX5E"]);
        let mut c = refreshed("S", 1, &u);
        assert!(c.stale_at(1));
        let second = c.painted().nth(1).unwrap().1.label.to_string();
        let w = c.pick("S", 1).unwrap();
        assert_eq!(
            w.apply("S"),
            (second.clone(), second.len()),
            "the row at index 1, whatever the ranking"
        );
        assert!(!c.stale_at(second.len()));
        assert!(c.stale_at(0));
        assert_eq!(c.pick("SX5E", 9), None, "out of range");
    }

    #[test]
    fn a_range_that_does_not_fit_the_line_writes_nothing() {
        let u = unds(&["SPX"]);
        // The underlying slot, so row 0 exists and only the fit refuses.
        let mut c = refreshed("S", 1, &u);
        assert_eq!(c.candidate_count(), 1);
        assert_eq!(c.pick("é", 0), None, "the end falls inside é");
        let mut c = refreshed("SP", 2, &u);
        assert_eq!(c.candidate_count(), 1);
        assert_eq!(c.pick("S", 0), None, "past the end");
    }

    #[test]
    fn the_painted_window_follows_the_lit_row() {
        let many: Vec<SharedString> = (0..12)
            .map(|i| SharedString::from(format!("U{i:02}")))
            .collect();
        let mut c = refreshed("", 0, &many);
        let mut line = String::new();
        for _ in 0..10 {
            line = c.cycle(&line, true).unwrap().apply(&line).0;
        }
        assert_eq!(c.highlighted(), 9);
        assert_eq!(
            c.painted().map(|(i, _)| i).collect::<Vec<_>>(),
            (2..10).collect::<Vec<_>>()
        );
    }
}
