# Pricer Entry-Bar Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The pricer's entry bar suggests the token under the caret
(underlying, expiry, type, barrier kind) and shows a one-line hint naming
what goes there. Tab and Shift+Tab cycle and write suggestions; a click
writes one.

**Architecture:**
- A pure `core::complete` decides the slot at the caret (by token
  position, the way `parse` reads the line), the byte range a write
  replaces, the ranked suggestions and the hint. It owns a small Tab-cycle
  state machine modelled on the timeseries expression field's.
- Underlyings come through an `UnderlyingSource` trait the app hands the
  factory. Today it is backed by `[pricing] underlyings` in `app.toml`;
  watchlists replace the backing later.
- The tile keeps the completion on its `Entry`, refreshes it on text,
  caret-affecting and reload events (never in render), and paints a hint
  line and a list under the bar.

**Tech Stack:** Rust, GPUI 0.2 + gpui-component 0.6.2 (`InputState`,
`InputEvent`, `deferred`/`anchored`), `geode_shell::listfilter::rank`,
`geode_shell::exprcomplete::Write`, `geode_shell::commandline::accept`.

**Spec:** `docs/superpowers/specs/2026-09-26-pricer-templates-and-completion-design.md` §3 (branch B). Branch A (§2) is merged.

## Global Constraints

- Slots, in `parse`'s order: an optional leading signed integer (`Qty`), then `Underlying`, `Expiry`, `Strikes`, `Type`, then `BarrierKind` and `BarrierLevel` only after a `C` or `P` type. Anything later is `Past`.
- `Expiry` and `Strikes` writes replace the `/`-separated part at the caret; every other write replaces the whole whitespace token.
- Suggestions: Underlying from the provider, in its order; Expiry the next 8 months' IMM codes (from today by the tile's `Clock`, a month counting while its third Friday is today or later) with month form and date as detail, then tenors `1m 3m 6m 1y`; Type `C`, `P`, then every template in set order with its signature as detail; BarrierKind `UI UO DI DO`. Qty, Strikes, BarrierLevel and Past have no suggestions.
- Ranking: `geode_shell::listfilter::rank` over each suggestion's `"label detail"` text; an empty typed token keeps natural order.
- The list paints at most 8 rows and scrolls with the lit row.
- Keys follow the timeseries expression field: Tab writes the lit suggestion and repeated Tab cycles; Shift+Tab cycles back (a first Shift+Tab writes the last); a row click writes and keeps focus in the field; each write is one range replace (one undo step). `enter` adds the line exactly as typed, with no expansion. `up`/`down` keep walking history.
- An empty provider shows `no underlyings configured ([pricing] underlyings)` in the list for the Underlying slot.
- No formatting or allocation in render: labels, details and the hint are `SharedString`s prepared in `refresh`.
- `[pricing] underlyings` in `app.toml`: an array of strings, upper-cased and de-duplicated in order. A non-array value, or a non-string element, warns at `app.pricing.underlyings` and is skipped. A reload applies it without a restart.
- `geode-pricer` never depends on a sibling module.
- Docs: `docs/current/features.md`, `docs/current/configuration.md` and `crates/geode-pricer/README.md` change with the behavior.
- Mutation harness: commit before targeted runs; `zsh scripts/mutation-check.sh --anchors-only` exits 0 at each task end.
- Commits end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Say "color", not "colour", in new text.

## Known duplication (ruled)

The Tab-cycle state (`cycle`, `pick`, `stale_at`, the painted window) now
exists in three places: timeseries `core::complete`, `geode_shell::exprcomplete`
(different keys and rows) and this plan's pricer `core::complete`. Merging
them into one `geode-shell` type is a separate refactor across two shipped
surfaces. This plan keeps the pricer's local and reuses the shared ranker,
`accept` and `exprcomplete::Write`. The consolidation is a recorded
follow-up.

## Review Focus

1. A caret moved by arrows or a click emits no `Change`, so a Tab at a moved caret must re-rank at the live caret before writing (the timeseries `stale_at` rule).
2. The echo of the tile's own write is a `Change` that must not reset the cycle; any other edit must.
3. `step_history` and the post-commit clear use `set_value` (no `Change`), so they must refresh the completion themselves.
4. A reload that changes templates or underlyings while the bar is open must refresh the list, including a revision bump with no text change on the next keystroke.
5. A multi-byte character in the field must never make a cached range slice inside a character (`fits` check before every write).

Tests for 1–3 and 5 are in Task 3, and for 4 in Tasks 2 and 3.

---

### Task 1: The pure completer (`core::complete`)

**Files:**
- Create: `crates/geode-pricer/src/core/complete.rs`
- Modify: `crates/geode-pricer/src/core/mod.rs` (`pub mod complete;` and re-exports)
- Modify: `crates/geode-pricer/src/core/shorthand.rs` (make `IMM_MONTHS`, `MONTH_NAMES`, `third_friday` reachable if not already `pub`)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces (`crate::core::complete`):
  - `pub const MAX_ROWS: usize = 8;`
  - `pub enum Slot { Qty, Underlying, Expiry, Strikes, Type, BarrierKind, BarrierLevel, Past }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `pub fn slot_at(line: &str, caret: usize) -> (Slot, Range<usize>)`
  - `pub struct Inputs<'a> { pub templates: &'a TemplateSet, pub underlyings: &'a [SharedString], pub today: NaiveDate }`
  - `pub struct Suggestion { pub label: SharedString, pub detail: SharedString }`
  - `#[derive(Default)] pub struct Completion` with `refresh(&mut self, line: &str, caret: usize, inputs: &Inputs)`, `stale_at(&self, caret: usize) -> bool`, `cycle(&mut self, line: &str, forward: bool) -> Option<Write>`, `pick(&mut self, line: &str, i: usize) -> Option<Write>`, `painted(&self) -> impl Iterator<Item = (usize, &Suggestion)>`, `highlighted(&self) -> usize`, `candidate_count(&self) -> usize`, `slot(&self) -> Option<Slot>`, `hint(&self) -> &SharedString`, `no_underlyings(&self) -> bool`
  - `pub use geode_shell::exprcomplete::Write;` (fields `range`, `text`; `apply(line) -> (String, usize)`)

- [ ] **Step 1: Failing tests** (bottom of `complete.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    fn unds(names: &[&str]) -> Vec<SharedString> {
        names.iter().map(|n| SharedString::from(n.to_string())).collect()
    }

    fn refreshed(line: &str, caret: usize, u: &[SharedString]) -> Completion {
        let set = TemplateSet::builtin();
        let mut c = Completion::default();
        c.refresh(line, caret, &Inputs { templates: &set, underlyings: u, today: today() });
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
            assert_eq!(slot_at(line, *caret), (*slot, range.clone()), "{line:?} at {caret}");
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
        let z = c.painted().find(|(_, s)| s.label.as_ref() == "Z26").unwrap().1;
        assert_eq!(z.detail.as_ref(), "DEC26 · 18 Dec 2026");
        assert_eq!(labels(&refreshed("SPX dec", 7, &[]))[0], "Z26", "the month form ranks");
    }

    #[test]
    fn a_month_whose_third_friday_is_today_still_counts() {
        let set = TemplateSet::builtin();
        let mut c = Completion::default();
        let friday = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        c.refresh("SPX ", 4, &Inputs { templates: &set, underlyings: &[], today: friday });
        assert_eq!(labels(&c)[0], "U26");
    }

    #[test]
    fn types_are_c_p_then_templates_with_their_signature() {
        let c = refreshed("SPX Z26 5000 ", 13, &[]);
        let rows: Vec<(String, String)> =
            c.painted().map(|(_, s)| (s.label.to_string(), s.detail.to_string())).collect();
        assert_eq!(rows[0], ("C".into(), "call".into()));
        assert_eq!(rows[1], ("P".into(), "put".into()));
        assert_eq!(rows[2], ("CS".into(), "2 strikes".into()));
        assert!(rows.iter().any(|r| r == &("CAL".into(), "1 strike · 2 expiries".into())));
    }

    #[test]
    fn barrier_kinds_follow_only_a_single_leg() {
        assert_eq!(labels(&refreshed("SPX Z26 5000 C ", 15, &[])), ["UI", "UO", "DI", "DO"]);
        assert_eq!(refreshed("SPX Z26 4800/5200 CS ", 21, &[]).candidate_count(), 0);
    }

    #[test]
    fn the_hint_names_the_slot_and_the_strike_count_of_the_type() {
        let hint = |line: &str| refreshed(line, line.len(), &[]).hint().to_string();
        assert_eq!(hint("SPX Z26 "), "STRIKES  K or K1/K2/…");
        let set = TemplateSet::builtin();
        let mut c = Completion::default();
        let line = "SPX Z26  FLY";
        c.refresh(line, 8, &Inputs { templates: &set, underlyings: &[], today: today() });
        assert_eq!(c.hint().as_ref(), "STRIKES  K1/K2/K3 for FLY", "the type after the caret");
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
        assert_eq!(line, "SPX Z26/X26", "the next Tab replaces the written part");
        let w = c.cycle(&line, false).unwrap();
        assert_eq!(w.apply(&line).0, "SPX Z26/V26");
        let mut fresh = refreshed("SPX ", 4, &[]);
        assert_eq!(fresh.cycle("SPX ", false).unwrap().apply("SPX ").0, "SPX 1y", "a first Shift+Tab writes the last");
    }

    #[test]
    fn a_moved_caret_is_stale_and_a_pick_writes_like_a_tab() {
        let u = unds(&["SPX", "SX5E"]);
        let mut c = refreshed("S", 1, &u);
        assert!(c.stale_at(1));
        let second = c.painted().nth(1).unwrap().1.label.to_string();
        let w = c.pick("S", 1).unwrap();
        assert_eq!(w.apply("S"), (second.clone(), second.len()), "the row at index 1, whatever the ranking");
        assert!(!c.stale_at(second.len()));
        assert!(c.stale_at(0));
        assert_eq!(c.pick("SX5E", 9), None, "out of range");
    }

    #[test]
    fn a_range_that_does_not_fit_the_line_writes_nothing() {
        let u = unds(&["SPX"]);
        let mut c = refreshed("xx S", 4, &u);
        assert_eq!(c.pick("xx é", 0), None, "the end falls inside é");
        let mut c = refreshed("xx SP", 5, &u);
        assert_eq!(c.pick("xx", 0), None, "past the end");
    }

    #[test]
    fn the_painted_window_follows_the_lit_row() {
        let many: Vec<SharedString> = (0..12).map(|i| SharedString::from(format!("U{i:02}"))).collect();
        let mut c = refreshed("", 0, &many);
        let mut line = String::new();
        for _ in 0..10 {
            line = c.cycle(&line, true).unwrap().apply(&line).0;
        }
        assert_eq!(c.highlighted(), 9);
        assert_eq!(c.painted().map(|(i, _)| i).collect::<Vec<_>>(), (2..10).collect::<Vec<_>>());
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer core::complete`
Expected: compile errors.

- [ ] **Step 3: Implement** (`complete.rs` above the tests)

```rust
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
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

fn suggestions(slot: Slot, inputs: &Inputs) -> Vec<Suggestion> {
    let s = |label: String, detail: String| Suggestion { label: label.into(), detail: detail.into() };
    match slot {
        Slot::Underlying => inputs
            .underlyings
            .iter()
            .map(|u| Suggestion { label: u.clone(), detail: SharedString::default() })
            .collect(),
        Slot::Expiry => {
            let mut out = Vec::with_capacity(12);
            let (mut y, mut m) = (inputs.today.year(), inputs.today.month());
            while out.len() < 8 {
                if let Some(d) = third_friday(y, m).filter(|d| *d >= inputs.today) {
                    let yy = y.rem_euclid(100);
                    out.push(s(
                        format!("{}{yy:02}", IMM_MONTHS[m as usize - 1]),
                        format!("{}{yy:02} · {}", MONTH_NAMES[m as usize - 1], d.format("%-d %b %Y")),
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
```

The strikes hint reads the first token after the caret, which in the strikes slot is the type.

The `Completion` state:

```rust
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
    r.start <= r.end && r.end <= line.len() && line.is_char_boundary(r.start) && line.is_char_boundary(r.end)
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
            .map(|s| if s.detail.is_empty() { s.label.to_string() } else { format!("{} {}", s.label, s.detail) })
            .collect();
        self.ranked = listfilter::rank(&texts, &line[range.clone()]).into_iter().map(|r| r.row).collect();
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
        self.ranked.iter().enumerate().skip(first).take(MAX_ROWS).map(|(i, r)| (i, &self.all[*r]))
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
        let write = Write { range: token.clone(), text };
        self.token = Some(token.start..end);
        self.caret = Some(end);
        self.written = Some(i);
        self.highlighted = i;
        Some(write)
    }
}
```

`listfilter::rank` takes `&[String]`. If it returns a different type or needs other arguments, adapt the call and keep "empty query keeps natural order".

In `shorthand.rs`, make `IMM_MONTHS`, `MONTH_NAMES` and `third_friday` `pub` or `pub(crate)` if they aren't already (they are `pub` today). In `core/mod.rs`, add `pub mod complete;`.

- [ ] **Step 4: Run to see them pass**

Run: `cargo test -p geode-pricer core::complete`
Expected: all pass. The one hint assertion corrected above is part of the test as committed.

- [ ] **Step 5: Mutation entries** (next to the pricer core entries)

```zsh
# Only a single C or P leg takes a barrier; a package's fifth token is
# past the end.
run_mutation "pricer complete: a package offers barrier kinds" \
  crates/geode-pricer/src/core/complete.rs \
  '        4 if type_tok.is_some_and(|t| is_single_leg(text(t))) => Slot::BarrierKind,' \
  '        4 => Slot::BarrierKind,' \
  geode-pricer barrier_kinds_follow_only_a_single_leg

# Expiries and strikes write only the `/` part at the caret.
run_mutation "pricer complete: a write replaces the whole slash token" \
  crates/geode-pricer/src/core/complete.rs \
  '        Slot::Expiry | Slot::Strikes if !range.is_empty() => slash_part(line, range, caret),' \
  '        Slot::Expiry | Slot::Strikes if false => slash_part(line, range, caret),' \
  geode-pricer tab_writes_cycles_and_shift_tab_goes_back_over_the_slash_part

# A leading quantity shifts every later slot by one.
run_mutation "pricer complete: a leading qty is not skipped" \
  crates/geode-pricer/src/core/complete.rs \
  '    let index = k - usize::from(qty);' \
  '    let index = k;' \
  geode-pricer the_slot_follows_parse_order_with_an_optional_qty

# A range cached against other text is refused, never sliced.
run_mutation "pricer complete: a stale range is sliced" \
  crates/geode-pricer/src/core/complete.rs \
  '        if !fits(line, &token) {' \
  '        if false {' \
  geode-pricer a_range_that_does_not_fit_the_line_writes_nothing
```

Commit, then run `zsh scripts/mutation-check.sh "pricer complete"`: all must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy -p geode-pricer --all-targets -- -D warnings
git add crates/geode-pricer/src/core scripts/mutation-check.sh
git commit -m "feat(pricer): pure entry-bar completion: slot, suggestions, hint, Tab cycle

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The underlying seam and `[pricing] underlyings`

**Files:**
- Modify: `crates/geode-pricer/src/content.rs` (trait, `UnderlyingList`, factory field and builder)
- Modify: `crates/geode-app/src/bridge.rs` (read config, build the list, reload it, key)
- Modify: `docs/current/configuration.md` (Pricing section)
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Produces (`geode_pricer::content`):

```rust
/// Where the entry bar's underlying suggestions come from. The app backs
/// it with `[pricing] underlyings` today and with the active watchlist
/// later; the pricer reads it, never writes it.
pub trait UnderlyingSource {
    /// In the provider's own order.
    fn underlyings(&self, cx: &App) -> Rc<[SharedString]>;
    /// Bumped whenever the list changes; a tile re-reads on a change.
    fn revision(&self, cx: &App) -> u64;
}

/// A list set from outside, e.g. from config.
#[derive(Default)]
pub struct UnderlyingList { list: RefCell<Rc<[SharedString]>>, revision: Cell<u64> }
impl UnderlyingList {
    /// Upper-cases, drops blanks and repeats (first wins), and bumps the
    /// revision only when the list actually changed.
    pub fn set(&self, names: &[String]);
}
impl UnderlyingSource for UnderlyingList { … }
```

  - `PricerFactory::with_underlyings(self, source: Rc<dyn UnderlyingSource>) -> Self`. The default source is an empty `UnderlyingList`, so every existing `PricerFactory::new` call keeps compiling.
  - `pub(crate) fn underlying_source(&self) -> Rc<dyn UnderlyingSource>` on `Shared` (or the factory), for the tile in Task 3.
- Produces (`geode_app::bridge`): `pub fn pricing_underlyings_from_config(config: &Config) -> (Vec<String>, Vec<Diagnostic>)`. `PricerConfigKey` gains `underlyings: Option<toml::Value>`.

- [ ] **Step 1: Failing tests**

In `content.rs` tests:

```rust
    #[test]
    fn an_underlying_list_normalises_and_bumps_only_on_change() {
        let l = UnderlyingList::default();
        let cx_free = |l: &UnderlyingList| (l.list.borrow().clone(), l.revision.get());
        l.set(&["spx".into(), "SX5E".into(), "SPX".into(), "  ".into()]);
        let (list, rev) = cx_free(&l);
        assert_eq!(list.iter().map(|s| s.as_ref()).collect::<Vec<_>>(), ["SPX", "SX5E"]);
        assert_eq!(rev, 1);
        l.set(&["SPX".into(), "sx5e".into()]);
        assert_eq!(cx_free(&l).1, 1, "the same list after normalising: no bump");
        l.set(&["NDX".into()]);
        assert_eq!(cx_free(&l).1, 2);
    }
```

In `bridge.rs` tests:
- `pricing_underlyings_reads_an_array_and_warns_on_bad_values`:
  - `underlyings = ["spx", 3, "SX5E"]` gives `["spx", "SX5E"]` and one warning at `app.pricing.underlyings`.
  - `underlyings = "SPX"` gives `[]` and one warning.
  - An absent key gives `[]` and no warning.
- A reload test beside `a_config_reload_hands_the_pricer_factory_its_templates`. After a reload whose `app.toml` sets `[pricing] underlyings = ["NDX"]`, the factory's source lists `NDX` and its revision has moved.
- `the_pricer_config_key_changes_only_with_what_the_pricer_reads` gains an `underlyings` change case.

Use the existing test config builders in `bridge.rs`.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer underlying_list` and `cargo test -p geode-app pricing_underlyings`. Both fail to compile.

- [ ] **Step 3: Implement**

`content.rs`:
- Add the trait and `UnderlyingList` as specified.
- `impl UnderlyingSource for UnderlyingList` returns `self.list.borrow().clone()` and `self.revision.get()`.
- `set` builds `Vec<SharedString>` with `trim`, upper-case, skip empty and skip repeats, then compares with the current list before storing and bumping.
- Add `underlyings: RefCell<Rc<dyn UnderlyingSource>>` to `Shared`, initialised to `Rc::new(UnderlyingList::default())`.
- Add the builder:

```rust
    /// The entry bar's underlying suggestions. Without this the list is
    /// empty and the bar says so.
    pub fn with_underlyings(self, source: Rc<dyn UnderlyingSource>) -> Self {
        *self.shared.underlyings.borrow_mut() = source;
        self
    }
```

`bridge.rs`:
- Add `pricing_underlyings_from_config`, which reads `config.get("app", "pricing.underlyings")`. Build each warning as a `Diagnostic` with `Severity::Warning`, `layer: config.explain("app", "pricing.underlyings")` (the pattern `pricing.adapter` uses), `path: Some("app.pricing.underlyings")`, and a message naming the problem.
- At setup, create `let underlyings = Rc::new(UnderlyingList::default()); underlyings.set(&names);`, pass `.with_underlyings(underlyings.clone())` where the app's factory is built, and keep `underlyings` on the bridge value that the reload observer can reach.
- Extend the diagnostics with the reader's.
- `PricerConfigKey` gains `underlyings: config.get("app", "pricing.underlyings").cloned()`.
- In the reload observer, after the key gate, read the list and call `underlyings.set(&names)`, merging its diagnostics with the others.

`docs/current/configuration.md`, Pricing section: add a paragraph after `refresh`:

"`underlyings` lists the underlyings the pricer's entry bar suggests, in the
order it offers them: an array of strings, upper-cased, with repeats
dropped. A non-array value or a non-string element warns at
`app.pricing.underlyings` and is skipped. A reload applies it to open tiles
without a restart. Without it the bar says no underlyings are configured."

- [ ] **Step 4: Run**

Run: `cargo test -p geode-pricer` and `cargo test -p geode-app`. All pass.

- [ ] **Step 5: Mutation entries**

```zsh
# A reload must reach the underlying list, or a desk edit to it waits for
# a restart.
run_mutation "pricer app: a reload leaves the underlying list stale" \
  crates/geode-app/src/bridge.rs \
  '<the reload observer'"'"'s `underlyings.set(&names);` line, verbatim>' \
  '' \
  geode-app <the reload test's name>

# The revision moves only on a real change; a bump on every set makes
# every tile re-rank on every unrelated reload.
run_mutation "pricer underlyings: an unchanged set bumps the revision" \
  crates/geode-pricer/src/content.rs \
  '<the equality guard line in `set`, verbatim>' \
  '<the same line with the guard removed>' \
  geode-pricer an_underlying_list_normalises_and_bumps_only_on_change
```

Fill in the anchors from the code as written, so each is unique in its file. Commit before running; both must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-pricer crates/geode-app scripts/mutation-check.sh docs/current/configuration.md
git commit -m "feat(pricer): an underlying source for the entry bar, from [pricing] underlyings

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: The bar completes

**Files:**
- Modify: `crates/geode-pricer/src/tile.rs` (`Entry` fields, refresh, keys, writes, picks, reload)
- Modify: `crates/geode-pricer/src/header.rs` (`render_entry_bar`: hint line, key context and listener, list slot)
- Modify: `crates/geode-pricer/src/popup.rs` (`render_entry_list`)
- Modify: `crates/geode-pricer/src/lib.rs` (reclaim `tab`/`shift-tab` in the bar's context)
- Modify: `docs/current/features.md`, `crates/geode-pricer/README.md`
- Modify: `scripts/mutation-check.sh`

**Interfaces:**
- Consumes: `core::complete::{Completion, Inputs, Write, Slot}` (Task 1), `UnderlyingSource`, `with_underlyings` (Task 2).
- Produces:
  - `pub const ENTRY_CONTEXT: &str = "PricerEntry";` in `header.rs`
  - `PricerTile::entry_key(&mut self, event: &KeyDownEvent, window, cx) -> bool`
  - `PricerTile::entry_pick(&mut self, i: usize, window, cx)`
  - Harness helpers: `entry_slot(&VisualTestContext) -> Option<Slot>`, `entry_hint(&VisualTestContext) -> Option<String>`, `entry_rows(&VisualTestContext) -> Vec<String>`, and `open_configured_with_underlyings(cx, templates, source)` (or an added parameter on the existing configured opener)

- [ ] **Step 1: Failing tile tests**

Add a `// ---- entry-bar completion ----` section. Use a helper that opens a seeded tile whose factory has `.with_underlyings(list)` where `list = Rc::new(UnderlyingList::default())` set to `["SPX", "SX5E", "NDX"]`. Keep the `Rc<UnderlyingList>` for revision tests.

```rust
    #[gpui::test]
    fn tab_writes_the_lit_underlying_and_cycles(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &["SPX", "SX5E", "NDX"]);
        h.dispatch(&mut vcx, "add_below", None);
        // An empty token keeps the provider's order: SPX, SX5E, NDX.
        vcx.simulate_keystrokes("tab");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("SPX"));
        vcx.simulate_keystrokes("tab");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("SX5E"), "the next Tab cycles");
        vcx.simulate_keystrokes("shift-tab");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("SPX"));
        assert_eq!(h.mode(&mut vcx), "insert");
        assert!(focused(&mut vcx), "Tab never moves focus out of the field");
    }

    #[gpui::test]
    fn the_hint_and_list_follow_the_slot(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &["SPX"]);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SPX Z26 4800/5200 ");
        assert_eq!(h.entry_hint(&vcx).as_deref(), Some("TYPE  C · P · or a template"));
        assert_eq!(&h.entry_rows(&vcx)[..3], ["C", "P", "CS"]);
        h.draw(&mut vcx);
        assert!(vcx.debug_bounds("pricer-entry-hint").is_some());
        assert!(vcx.debug_bounds("pricer-entry-list").is_some());
        typed(&h, &mut vcx, "CS ");
        assert!(h.entry_rows(&vcx).is_empty(), "past the end: no list");
        h.draw(&mut vcx);
        assert!(vcx.debug_bounds("pricer-entry-list").is_none());
    }

    #[gpui::test]
    fn a_row_click_writes_and_keeps_typing_in_the_field(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &["SPX", "SX5E", "NDX"]);
        h.dispatch(&mut vcx, "add_below", None);
        let at = centre_of(&mut vcx, "pricer-entry-row-2");
        click_at(&mut vcx, at, 1);
        h.draw(&mut vcx);
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("NDX"));
        assert!(focused(&mut vcx));
        typed(&h, &mut vcx, " Z26");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("NDX Z26"), "the keyboard stayed in the field");
    }

    #[gpui::test]
    fn enter_adds_the_line_as_typed_without_expanding(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &["SPX"]);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "SP Z26 5000 C");
        h.dispatch(&mut vcx, "commit", None);
        let last = h.tile.read_with(&vcx, |t, _| t.sheet.shorthand(t.sheet.len() - 1));
        assert_eq!(last, "SP Z26 5000 C");
    }

    #[gpui::test]
    fn up_walks_history_with_the_list_open_and_the_list_follows(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &["SPX"]);
        h.dispatch(&mut vcx, "add_below", None);
        assert!(!h.entry_rows(&vcx).is_empty());
        h.dispatch(&mut vcx, "insert_up", None);
        let text = h.entry_text(&vcx).unwrap();
        assert!(!text.is_empty(), "history recalled a line");
        assert_eq!(
            h.entry_slot(&vcx),
            Some(crate::core::complete::slot_at(&text, text.len()).0),
            "the list was re-ranked for the recalled line (set_value emits no Change)"
        );
    }

    #[gpui::test]
    fn a_tab_after_the_caret_moved_ranks_at_the_live_caret(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &["SPX", "NDX"]);
        h.dispatch(&mut vcx, "add_below", None);
        typed(&h, &mut vcx, "N Z26");
        vcx.simulate_keystrokes("home right");
        vcx.simulate_keystrokes("tab");
        assert_eq!(h.entry_text(&vcx).as_deref(), Some("NDX Z26"));
    }

    #[gpui::test]
    fn a_revision_bump_reaches_an_open_bar(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, list) = open_with_underlyings(cx, &["SPX"]);
        h.dispatch(&mut vcx, "add_below", None);
        list.set(&["SPX".into(), "NKY".into()]);
        typed(&h, &mut vcx, "N");
        assert_eq!(h.entry_rows(&vcx), ["NKY"]);
    }

    #[gpui::test]
    fn an_empty_provider_says_none_are_configured(cx: &mut gpui::TestAppContext) {
        let (h, mut vcx, _list) = open_with_underlyings(cx, &[]);
        h.dispatch(&mut vcx, "add_below", None);
        h.draw(&mut vcx);
        assert!(vcx.debug_bounds("pricer-entry-none").is_some());
    }
```

`home right` must move the caret in the gpui-component `Input`. If the harness's keystroke spelling differs, use the one other tests in this file use.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p geode-pricer entry_` (and the new names). They fail to compile.

- [ ] **Step 3: Implement**

`tile.rs`:
- `Entry` gains:
  - `pub completion: Completion`;
  - `echo: Option<String>`, the text the tile's own last write left, so its `Change` echo is skipped.
- The tile gains `underlyings: Rc<[SharedString]>` and `underlyings_rev: Option<u64>`, both empty and `None` at construction.
- Add:

```rust
    /// Re-read the provider when its revision moved since the last read.
    fn read_underlyings(&mut self, cx: &App) {
        let source = self.shared.underlyings.borrow().clone();
        let rev = source.revision(cx);
        if self.underlyings_rev != Some(rev) {
            self.underlyings = source.underlyings(cx);
            self.underlyings_rev = Some(rev);
        }
    }

    /// Re-rank the bar's completion at the field's live text and caret.
    /// Runs on open, on a typed edit, after a history step, after a
    /// commit clears the field, on reload, and on a Tab at a moved caret;
    /// never in render.
    fn refresh_entry_completion(&mut self, cx: &mut Context<Self>) {
        self.read_underlyings(cx);
        let today = self.clock.today(chrono::Utc::now());
        let Some(entry) = self.entry.as_mut() else { return };
        let input = entry.input.read(cx);
        let (text, caret) = (input.value().to_string(), input.cursor());
        let inputs = Inputs { templates: self.sheet.templates(), underlyings: &self.underlyings, today };
        entry.completion.refresh(&text, caret, &inputs);
    }
```

  Borrowing: `self.sheet.templates()` and `self.entry` are distinct fields. If the borrow checker objects, clone the `Arc<TemplateSet>` first.

- In `open_entry`'s `InputEvent::Change` subscriber:
  - skip when `entry.echo.take()` equals the live text;
  - otherwise clear the error, call `refresh_entry_completion` and notify.
  - After building the `Entry`, call `refresh_entry_completion`.
- In `step_history`, after `set_value`, call `refresh_entry_completion`.
- In `commit_entry`, after the success arm's `set_value("")`, call `refresh_entry_completion`.
- In `config_changed`, after the history recompute, call `refresh_entry_completion` when the bar is open.
- Writes and keys:

```rust
    /// One completion write as one range replace, so undo takes it back;
    /// the replace's own Change is recorded as its echo. Focus stays in
    /// the field.
    fn write_entry(&mut self, write: Write, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else { return };
        let echo = entry.input.update(cx, |s, cx| {
            s.set_selected_range(write.range.clone(), cx);
            s.replace(write.text.clone(), window, cx);
            s.focus(window, cx);
            s.value().to_string()
        });
        entry.echo = Some(echo);
        cx.notify();
    }

    /// The bar's own keys, ahead of the shell: bare `tab` writes the next
    /// suggestion and `shift-tab` the previous one. Both are consumed while
    /// the bar is up, even with nothing to offer, so neither moves focus.
    pub(crate) fn entry_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let ks = &event.keystroke;
        let m = &ks.modifiers;
        if ks.key != "tab" || m.control || m.alt || m.platform || m.function {
            return false;
        }
        let Some(entry) = &self.entry else { return false };
        if entry.completion.stale_at(entry.input.read(cx).cursor()) {
            self.refresh_entry_completion(cx);
        }
        let Some(entry) = self.entry.as_mut() else { return false };
        let text = entry.input.read(cx).value().to_string();
        if let Some(write) = entry.completion.cycle(&text, !m.shift) {
            self.write_entry(write, window, cx);
        }
        true
    }

    /// A list row's press: the same write a Tab makes.
    pub(crate) fn entry_pick(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entry.as_mut() else { return };
        let text = entry.input.read(cx).value().to_string();
        if let Some(write) = entry.completion.pick(&text, i) {
            self.write_entry(write, window, cx);
        }
    }
```

  Confirm the gpui-component 0.6.2 `InputState` names in the registry source: `set_selected_range`, `replace`, `cursor`, `focus`. The timeseries tile uses exactly these.
- In `render`, pass the tile entity to `render_entry_bar` together with `&e.completion`.

`header.rs`:
- `pub const ENTRY_CONTEXT: &str = "PricerEntry";`
- `render_entry_bar(input, label, error, completion, tile, cx)`:
  - Wrap in `.relative()`, add `.key_context(ENTRY_CONTEXT)`, and add an `.on_key_down` that calls `tile.update(cx, |t, cx| t.entry_key(event, window, cx))` and stops propagation when it returns true.
  - Between the field row and the error, a hint line when `completion.hint()` is non-empty: `div().text_xs().text_color(theme.muted_foreground).font_family(fonts::MONO).debug_selector(|| "pricer-entry-hint".into()).child(completion.hint().clone())`.
  - Last, `.when_some(popup::render_entry_list(completion, tile, cx), |el, list| el.child(div().absolute().left_0().bottom_0().child(list)))`.

`popup.rs`: add `render_entry_list(c: &Completion, tile: &Entity<PricerTile>, cx: &App) -> Option<impl IntoElement>`, modelled on `render_choice` and the timeseries `render_expr_list`:
- Return `None` when `!c.no_underlyings() && c.candidate_count() == 0`.
- Use `popover_surface(cx)` with `.debug_selector(|| "pricer-entry-list".into())` and `.occlude()`, so the table under it gets no press.
- With `c.no_underlyings()`, show one muted row, `no underlyings configured ([pricing] underlyings)`, selector `pricer-entry-none`.
- Otherwise, one row per `c.painted()`:
  - `ROW_HEIGHT`, `ROW_INSET`, the theme radius, and `bg(theme.accent)` / `text_color(theme.accent_foreground)` when lit, like `render_choice`;
  - `.debug_selector(move || format!("pricer-entry-row-{i}"))`;
  - an `on_mouse_down(Left)` that stops propagation and calls `entry_pick(i)`;
  - the label, then the detail in `text_xs` muted when present.
- Anchor with `deferred(anchored().anchor(Anchor::TopLeft).position_mode(AnchoredPositionMode::Local).snap_to_window_with_margin(px(8.)).child(list)).with_priority(1)`, the same arrangement `render_choice` uses.

`lib.rs` `init`: after the DataTable loop, add

```rust
    // The entry bar's completion owns tab/shift-tab: gpui-component's Root
    // binds both to focus cycling, and a matched action runs before the
    // bar's key listener (the timeseries expression field's reason).
    for key in ["tab", "shift-tab"] {
        cx.bind_keys([gpui::KeyBinding::new(key, gpui::NoAction, Some(header::ENTRY_CONTEXT))]);
    }
```

Harness helpers in `tile.rs` tests:
- `entry_slot`: reads `t.entry.as_ref().and_then(|e| e.completion.slot())`.
- `entry_hint`: reads `t.entry.as_ref().map(|e| e.completion.hint().to_string())`.
- `entry_rows`: maps `e.completion.painted()` to labels.
- `open_with_underlyings(cx, names) -> (Harness, VisualTestContext, Rc<UnderlyingList>)`: builds the factory through the existing configured opener plus `.with_underlyings(list.clone())`, seeded with `BOOK`, and calls `h.visible(&mut vcx, true)`.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-pricer`, then `cargo test -p geode-app pricer`. All pass.

- [ ] **Step 5: Mutation entries**

```zsh
# The echo of the tile's own write must not reset the cycle, or a second
# Tab writes the first suggestion again.
run_mutation "pricer entry bar: the write's echo resets the cycle" \
  crates/geode-pricer/src/tile.rs \
  '<the echo-skip condition line in the Change subscriber, verbatim>' \
  '<the same line made always-false>' \
  geode-pricer tab_writes_the_lit_underlying_and_cycles

# A Tab at a caret moved without typing must re-rank there first.
run_mutation "pricer entry bar: a Tab at a moved caret uses the old range" \
  crates/geode-pricer/src/tile.rs \
  '        if entry.completion.stale_at(entry.input.read(cx).cursor()) {' \
  '        if false {' \
  geode-pricer a_tab_after_the_caret_moved_ranks_at_the_live_caret

# A provider change must reach an open bar on the next keystroke.
run_mutation "pricer entry bar: the provider is read once" \
  crates/geode-pricer/src/tile.rs \
  '        if self.underlyings_rev != Some(rev) {' \
  '        if self.underlyings_rev.is_none() {' \
  geode-pricer a_revision_bump_reaches_an_open_bar

# A history step sets the text without a Change; it must re-rank itself.
run_mutation "pricer entry bar: a history step leaves the list stale" \
  crates/geode-pricer/src/tile.rs \
  '<the refresh call added in step_history, with enough context to be unique>' \
  '<the same without the call>' \
  geode-pricer up_walks_history_with_the_list_open_and_the_list_follows
```

Fill in the verbatim anchors from the code as written. Commit before running `zsh scripts/mutation-check.sh "pricer entry bar"`; all must be caught. `--anchors-only` must exit 0.

- [ ] **Step 6: Docs**

`docs/current/features.md`, pricer section, after the entry-bar paragraph:

"As you type, the bar suggests the part of the line under the caret and a
hint line names what goes there: underlyings from `[pricing] underlyings`,
the next eight monthly expiries and common tenors, `C`, `P` and every
template with how many strikes and expiries it takes, and the four barrier
kinds after a single leg. Tab writes the lit suggestion over the token (only
the `/`-separated part for expiries and strikes) and repeated Tab cycles;
Shift+Tab cycles back; a click writes a suggestion and keeps typing in the
field. `enter` adds the line exactly as typed. `up`/`down` still walk
history."

`crates/geode-pricer/README.md`:
- The module-map row `complete`: "Entry-bar completion: the slot at the caret, suggestions, hint, and the Tab cycle."
- An invariant: "Completion never runs in render; the tile refreshes it on every text change, history step, commit and reload, and a Tab at a moved caret re-ranks first."

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings
git add -A crates/geode-pricer scripts/mutation-check.sh docs/current/features.md
git commit -m "feat(pricer): the entry bar suggests and completes the token at the caret

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Branch verification (controller)

- [ ] `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo check -p geode-shell --features test-support --all-targets`, `zsh scripts/mutation-check.sh --anchors-only`, and `zsh scripts/mutation-check.sh --changed=<branch base>`, run detached and against the branch point, not the main tip.
- [ ] Display check list for the user:
  - the hint line and the list under the bar, their height and alignment against the field;
  - the list overlapping the table without reflowing it;
  - the lit row color;
  - details muted beside labels;
  - the empty-provider row;
  - a real-keyboard Tab cycle;
  - a click on a row.
