# As-of dialog Part 3: the dialog — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `mod+t` opens one ranked list under a filter — `Current` (while pinned), `Live`, the five business-day presets, a `Custom` row holding the segmented date-time field, and the recent publishes — every row showing the instant it resolves to on the configured clock; the calendar pane and the free-text grammar are gone.

**Architecture:** `asof_view.rs` is rewritten around a pure `AsOfState` (rows, ranking, highlight, an optional open `DateTimeField`) with the gpui half painting sections through `row_paint` and the Custom row through `geode_widgets::datefield::paint`. Keys: `listfilter::nav_command` moves, `1`–`5` on an empty field jump, `enter` commits, `tab` opens/leaves the field, the widget's `route` owns keys while the field is open. Requires Parts 1 and 2 merged.

**Tech Stack:** geode-widgets (Part 1), geode_core::clock (Part 2), gpui / gpui-component 0.6.2, the shell's dialog doors (`open_shell_dialog_with_key`, `filter_row`, `hint_rows`, `row_paint`), the mutation harness.

**Spec:** `docs/superpowers/specs/2026-09-20-geode-as-of-dialog-design.md` §5 (and §2 ruling 4, §7, §9).

## Global Constraints

- Filter-first: the shared dialog `Input` is focused on open (`focus_filter = true`); `j`/`k` are typing, never navigation. Move keys are exactly `listfilter::nav_command`'s.
- `enter` commits the HIGHLIGHTED row. `1`–`5` jump only while the field is EMPTY.
- While the Custom field is open the modal key handler claims every bare key through `geode_widgets::datefield::route`; a chord (`route` → `None`) is not claimed and reaches the shell as everywhere. There is no state where the field is open and the filter takes keys.
- `Current` and `Live` rows exist only while `frame.as_of()` is `At(_)`.
- The right column is paint only, never matched by the filter.
- `commit_at`/`commit_live`, `Frame::set_as_of`, `previous_as_of`, `frame::live` and `frame::as_of_undo` are unchanged.
- The calendar, `as_of_calendar`, `on_calendar_selected`, `compose_with_date`, `calendar_date`, `shows_calendar`, `resolve_input`, `AsOfState::{error, resolved}` and `geode-shell`'s direct `gpui-base` dependency are deleted. `parse_as_of` stays in `geode-core` for `:asof`.
- The mouse-opened-dialog rule: the toolbar chip's open goes through `open_shell_dialog_with_key` (unchanged) and its test types after the click.
- Segment colours over the popover clear 3:1 on every bundled theme, with no exception list.
- CI: fmt, clippy `-D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`, both platforms. Harness entries per load-bearing branch; `--anchors-only` exits 0 before merge. Commit before mutating.

---

## File structure

| File | Responsibility |
|---|---|
| `crates/geode-shell/src/shell/asof_view.rs` | rewritten: pure `AsOfState`/`Row`/`Section` + `open`/`handle_key`/`build` |
| `crates/geode-shell/src/shell/asof_rows.rs` (new) | the pure row model: building rows, ranking by section, highlight moves, the digit jump, the Custom seed, commit resolution — no `gpui` |
| `crates/geode-shell/src/shell/mod.rs` | drop `as_of_calendar` (field, init, subscription, accessor); the dialog-input subscription arm calls `asof_rows::set_query` |
| `crates/geode-shell/Cargo.toml`, root `Cargo.toml` | drop `gpui-base` from the shell (the root pin stays for `geode-app`); rewrite the pin comment |
| `crates/geode-shell/src/shell/tests/asof.rs` | rewritten window tests |
| `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md`, the spec's "as built" | entries, rules, history, display-check list |

---

### Task 1: The pure row model — `asof_rows.rs`

**Files:**
- Create: `crates/geode-shell/src/shell/asof_rows.rs`
- Modify: `crates/geode-shell/src/shell/mod.rs` (add `mod asof_rows;` beside `mod asof_view;`)

**Interfaces:**
- Consumes: `geode_core::clock::{Clock, Preset, presets}`, `geode_core::query::AsOf`, `crate::frame::Publish`, `crate::listfilter::{rank, Ranked, NavCommand}`, `crate::vimnav::apply`, `geode_widgets::datefield::{DateTimeField, Precision, Segment}`.
- Produces:

```rust
pub enum Section { Current, Live, Presets, Custom, Publishes }
pub enum Row { Current(DateTime<Utc>), Live, Preset(usize), Custom, Publish(usize) }
pub struct Painted { pub row: Row, pub label: String, pub right: String, pub indices: Vec<usize>, pub section: Section }
pub struct AsOfState {
    // private: rows: Vec<(Row, String /*label*/, String /*right*/)>, presets: Vec<Preset>, publishes: Vec<(DateTime<Utc>, String, String)>,
    //          ranked: Vec<Painted>, highlighted: usize, query: String, field: Option<DateTimeField>, clock: Clock, pinned: Option<DateTime<Utc>>
}
impl AsOfState {
    pub fn build(as_of: &AsOf, publishes: &[Publish], clock: Clock, now: DateTime<Utc>) -> AsOfState;
    pub fn set_query(&mut self, query: &str) -> bool;    // re-ranks; highlight to 0; false if unchanged
    pub fn query(&self) -> &str;
    pub fn painted(&self) -> &[Painted];                  // section-stable rank order
    pub fn highlighted(&self) -> usize;
    pub fn nav(&mut self, cmd: NavCommand);               // vimnav::apply over painted().len()
    pub fn set_highlighted(&mut self, row: usize) -> bool;
    pub fn jump_digit(&self, key: &str) -> Option<Commit>; // 1–5 on an EMPTY query → that preset's commit
    pub fn open_field(&mut self);                          // tab: seed from the highlighted row (or pinned/now), highlight Custom
    pub fn close_field(&mut self);
    pub fn field(&self) -> Option<&DateTimeField>;
    pub fn field_mut(&mut self) -> Option<&mut DateTimeField>;
    pub fn field_refusal(&self) -> Option<&str>;           // a DST-gap refusal to paint on the Custom row
    pub fn commit(&mut self) -> Result<Commit, String>;    // the highlighted row's outcome (field open: the field's value)
}
pub enum Commit { At(DateTime<Utc>), Live }
```

- [ ] **Step 1: Write the failing tests** (bottom of `asof_rows.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use geode_core::clock::Clock;

    fn publish(dataset: &str, batch: &str, books: usize, at: DateTime<Utc>) -> Publish {
        Publish { dataset: dataset.into(), batch: batch.into(), books, at }
    }

    // Monday 21 Sep 2026, 10:42 UTC.
    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 21, 10, 42, 0).unwrap()
    }

    fn state(as_of: AsOf, publishes: &[Publish]) -> AsOfState {
        AsOfState::build(&as_of, publishes, Clock::utc(), now())
    }

    fn labels(s: &AsOfState) -> Vec<String> {
        s.painted().iter().map(|p| p.label.clone()).collect()
    }

    #[test]
    fn under_live_the_rows_are_presets_custom_and_publishes_in_section_order() {
        let pubs = [publish("risk", "EOD", 12, now() - chrono::Duration::hours(1))];
        let s = state(AsOf::Live, &pubs);
        assert_eq!(
            labels(&s),
            ["EOD T-1", "SOD T", "EOD T-2", "EOD T-3", "EOD T-5", "custom", "risk / EOD · 12 books"]
        );
        assert_eq!(s.painted()[0].right, "Fri 18 Sep 18:00");
        assert_eq!(s.painted()[6].right, "Mon 09:42:00");
        assert_eq!(s.highlighted(), 0, "the first preset is highlighted on open");
    }

    #[test]
    fn while_pinned_current_and_live_lead_and_current_reads_the_pinned_instant() {
        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let s = state(AsOf::At(pinned), &[]);
        assert_eq!(labels(&s)[..2], ["current".to_string(), "live".to_string()]);
        assert_eq!(s.painted()[0].right, "2026-09-18 16:00:00 UTC");
        assert!(matches!(s.painted()[0].row, Row::Current(t) if t == pinned));
    }

    #[test]
    fn a_query_ranks_within_sections_and_drops_empty_sections() {
        let pubs = [
            publish("risk", "EOD", 12, now() - chrono::Duration::hours(1)),
            publish("greeks", "INTRADAY", 12, now() - chrono::Duration::minutes(5)),
        ];
        let mut s = state(AsOf::Live, &pubs);
        assert!(s.set_query("eod"));
        let l = labels(&s);
        assert!(l.iter().all(|x| x.to_lowercase().contains("eod")), "{l:?}");
        assert!(!l.contains(&"custom".to_string()));
        assert!(!l.contains(&"SOD T".to_string()));
        assert_eq!(l.last().unwrap(), "risk / EOD · 12 books", "publishes stay after presets");
        assert!(!s.painted()[0].indices.is_empty(), "match glyphs are carried for painting");
        assert!(!s.set_query("eod"), "an unchanged query is a no-op");
    }

    #[test]
    fn the_right_column_is_never_matched() {
        let mut s = state(AsOf::Live, &[]);
        s.set_query("18");
        assert!(s.painted().is_empty(), "'18' is in every preset's right column, in no label");
    }

    #[test]
    fn a_digit_jumps_only_on_an_empty_query() {
        let mut s = state(AsOf::Live, &[]);
        assert!(matches!(s.painted()[1].row, Row::Preset(1)), "row 2 is the second preset");
        let sod_t = Clock::utc().sod_of(chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()).unwrap();
        assert_eq!(s.jump_digit("2"), Some(Commit::At(sod_t)), "2 is SOD T");
        assert_eq!(s.jump_digit("9"), None, "past the painted presets is inert");
        assert_eq!(s.jump_digit("0"), None);
        s.set_query("e");
        assert_eq!(s.jump_digit("1"), None, "with text typed, a digit is a filter character");
    }

    #[test]
    fn tab_seeds_the_field_from_the_highlighted_row_or_the_pin_or_now() {
        let mut s = state(AsOf::Live, &[]);
        s.nav(NavCommand::Move(1)); // SOD T
        s.open_field();
        assert!(matches!(s.painted()[s.highlighted()].row, Row::Custom));
        assert_eq!(s.field().unwrap().text(), "2026-09-21 08:00:00", "seeded from SOD T");
        s.close_field();
        assert!(s.field().is_none());

        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let mut s = state(AsOf::At(pinned), &[]);
        s.set_highlighted(0); // Current
        s.open_field();
        assert_eq!(s.field().unwrap().text(), "2026-09-18 16:00:00", "Current seeds the pin");

        let mut s = state(AsOf::Live, &[]);
        s.set_query("custom");
        s.open_field();
        assert!(s.query().is_empty(), "opening the field clears the query");
        assert_eq!(s.field().unwrap().text(), "2026-09-21 10:42:00", "no instant on the row: now");
        assert_eq!(s.field().unwrap().segment(), Segment::Day);
    }

    #[test]
    fn commit_answers_the_highlighted_row_and_the_open_fields_value() {
        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let mut s = state(AsOf::At(pinned), &[]);
        s.set_highlighted(1);
        assert_eq!(s.commit().unwrap(), Commit::Live);
        s.set_highlighted(2);
        assert_eq!(s.commit().unwrap(), Commit::At(Clock::utc().eod_of(chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()).unwrap()));
        s.open_field();
        s.field_mut().unwrap().step(1); // day +1
        assert_eq!(s.commit().unwrap(), Commit::At(Utc.with_ymd_and_hms(2026, 9, 19, 18, 0, 0).unwrap()));
    }

    #[test]
    fn a_field_value_in_a_dst_gap_is_refused_and_named_on_the_row() {
        let ny = Clock::in_zone_named("America/New_York");
        let mut s = AsOfState::build(&AsOf::Live, &[], ny, Utc.with_ymd_and_hms(2026, 3, 9, 15, 0, 0).unwrap());
        s.set_query("custom");
        s.open_field();
        let f = s.field_mut().unwrap();
        // 2026-03-08 02:30 New York does not exist.
        f.select(Segment::Day); f.step(-1);
        f.select(Segment::Hour); f.step(-13); // 15 → 02
        f.select(Segment::Minute); f.step(30);
        let err = s.commit().unwrap_err();
        assert!(err.contains("does not name a valid local time"), "{err}");
        assert_eq!(s.field_refusal(), Some(err.as_str()));
        assert!(s.field().is_some(), "the field stays open");
    }
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-shell asof_rows` → compile error.

- [ ] **Step 3: Implement**

```rust
//! The as-of dialog's pure row model (as-of dialog spec 2026-09-20 §5.1):
//! one ranked list in five fixed sections — `Current` and `Live` (only
//! while the frame is pinned), the business-day presets, the `Custom`
//! row holding the segmented field, the recent publishes — ranked by
//! `listfilter::rank` over each row's LABEL (the right column is paint,
//! never matched), section order kept and rank order inside a section.
//! No `gpui`: every transition is unit-tested here; `asof_view` paints.

use chrono::{DateTime, Utc};

use geode_core::clock::{Clock, Preset, presets};
use geode_core::query::AsOf;
use geode_widgets::datefield::{DateTimeField, Precision, Segment};

use crate::frame::Publish;
use crate::listfilter::{self, NavCommand};
use crate::vimnav;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Current,
    Live,
    Presets,
    Custom,
    Publishes,
}

impl Section {
    /// The eyebrow painted above the section, `None` for the two single
    /// rows.
    pub fn eyebrow(self) -> Option<&'static str> {
        match self {
            Section::Current | Section::Live => None,
            Section::Presets => Some("Presets"),
            Section::Custom => Some("Custom"),
            Section::Publishes => Some("Recent publishes"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Current(DateTime<Utc>),
    Live,
    Preset(usize),
    Custom,
    Publish(usize),
}

/// One row as painted: its label (what the filter matched), the right
/// column, the matched glyph indices, and its section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Painted {
    pub row: Row,
    pub label: String,
    pub right: String,
    pub indices: Vec<usize>,
    pub section: Section,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Commit {
    At(DateTime<Utc>),
    Live,
}

#[derive(Debug, Clone)]
struct Entry {
    row: Row,
    label: String,
    right: String,
    section: Section,
}

#[derive(Debug, Clone)]
pub struct AsOfState {
    entries: Vec<Entry>,
    presets: Vec<Preset>,
    publishes: Vec<DateTime<Utc>>,
    ranked: Vec<Painted>,
    highlighted: usize,
    query: String,
    field: Option<DateTimeField>,
    refusal: Option<String>,
    clock: Clock,
    pinned: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
}

impl AsOfState {
    pub fn build(as_of: &AsOf, publishes: &[Publish], clock: Clock, now: DateTime<Utc>) -> Self {
        let pinned = match as_of {
            AsOf::Live => None,
            AsOf::At(t) => Some(*t),
        };
        let presets = presets(&clock, now);
        let mut entries = Vec::new();
        if let Some(t) = pinned {
            entries.push(Entry {
                row: Row::Current(t),
                label: "current".into(),
                right: clock.full(t),
                section: Section::Current,
            });
            entries.push(Entry {
                row: Row::Live,
                label: "live".into(),
                right: "follow new publishes".into(),
                section: Section::Live,
            });
        }
        for (i, p) in presets.iter().enumerate() {
            entries.push(Entry {
                row: Row::Preset(i),
                label: p.label.to_string(),
                right: clock.local(p.at).format("%a %-d %b %H:%M").to_string(),
                section: Section::Presets,
            });
        }
        entries.push(Entry {
            row: Row::Custom,
            label: "custom".into(),
            right: String::new(),
            section: Section::Custom,
        });
        let mut instants = Vec::with_capacity(publishes.len());
        for (i, p) in publishes.iter().enumerate() {
            instants.push(p.at);
            entries.push(Entry {
                row: Row::Publish(i),
                label: format!(
                    "{} / {} · {} book{}",
                    p.dataset,
                    p.batch,
                    p.books,
                    if p.books == 1 { "" } else { "s" }
                ),
                right: clock.local(p.at).format("%a %H:%M:%S").to_string(),
                section: Section::Publishes,
            });
        }
        let mut state = AsOfState {
            entries,
            presets,
            publishes: instants,
            ranked: Vec::new(),
            highlighted: 0,
            query: String::new(),
            field: None,
            refusal: None,
            clock,
            pinned,
            now,
        };
        state.rerank();
        state
    }

    /// Rank every section's labels against the query, sections in fixed
    /// order, rank order inside each; an empty query keeps file order.
    fn rerank(&mut self) {
        const ORDER: [Section; 5] = [
            Section::Current,
            Section::Live,
            Section::Presets,
            Section::Custom,
            Section::Publishes,
        ];
        let mut out = Vec::new();
        for section in ORDER {
            let members: Vec<usize> = (0..self.entries.len())
                .filter(|i| self.entries[*i].section == section)
                .collect();
            let texts: Vec<String> = members.iter().map(|i| self.entries[*i].label.clone()).collect();
            for r in listfilter::rank(&texts, &self.query) {
                let e = &self.entries[members[r.row]];
                out.push(Painted {
                    row: e.row.clone(),
                    label: e.label.clone(),
                    right: e.right.clone(),
                    indices: r.indices,
                    section,
                });
            }
        }
        self.ranked = out;
        self.highlighted = 0;
    }

    pub fn set_query(&mut self, query: &str) -> bool {
        if self.query == query {
            return false;
        }
        self.query = query.to_string();
        self.rerank();
        true
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn painted(&self) -> &[Painted] {
        &self.ranked
    }

    pub fn highlighted(&self) -> usize {
        self.highlighted
    }

    pub fn nav(&mut self, cmd: NavCommand) {
        self.highlighted = vimnav::apply(self.highlighted, self.ranked.len(), cmd);
    }

    pub fn set_highlighted(&mut self, row: usize) -> bool {
        if row >= self.ranked.len() {
            return false;
        }
        self.highlighted = row;
        true
    }

    /// `1`–`5` on an EMPTY query: that preset's commit (the grouping
    /// picker's digit-jump precedent). `None` for a typed query, `0`, a
    /// non-digit, or a digit past the painted presets.
    pub fn jump_digit(&self, key: &str) -> Option<Commit> {
        if !self.query.is_empty() {
            return None;
        }
        let digit = key.parse::<usize>().ok().filter(|d| (1..=9).contains(d))?;
        let preset = self.presets.get(digit - 1)?;
        Some(Commit::At(preset.at))
    }

    /// The instant a painted row stands for, `None` for `Live` and `Custom`.
    fn instant_of(&self, row: &Row) -> Option<DateTime<Utc>> {
        match row {
            Row::Current(t) => Some(*t),
            Row::Live | Row::Custom => None,
            Row::Preset(i) => self.presets.get(*i).map(|p| p.at),
            Row::Publish(i) => self.publishes.get(*i).copied(),
        }
    }

    /// `tab`: open the Custom field seeded from the highlighted row's
    /// instant, else the pinned instant, else `now`; on the day segment;
    /// and move the highlight onto the Custom row (re-showing it if the
    /// query had filtered it out is not needed: the field is painted in
    /// its place only while it is painted, so the query is cleared).
    pub fn open_field(&mut self) {
        let seed = self
            .ranked
            .get(self.highlighted)
            .and_then(|p| self.instant_of(&p.row))
            .or(self.pinned)
            .unwrap_or(self.now);
        let local = self.clock.local(seed).naive_local();
        self.field = Some(DateTimeField::open(local, Precision::DateTime, Segment::Day));
        self.refusal = None;
        if !self.query.is_empty() {
            self.query.clear();
            self.rerank();
        }
        if let Some(i) = self.ranked.iter().position(|p| p.row == Row::Custom) {
            self.highlighted = i;
        }
    }

    pub fn close_field(&mut self) {
        self.field = None;
        self.refusal = None;
    }

    pub fn field(&self) -> Option<&DateTimeField> {
        self.field.as_ref()
    }

    pub fn field_mut(&mut self) -> Option<&mut DateTimeField> {
        self.refusal = None;
        self.field.as_mut()
    }

    pub fn field_refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    pub fn clock(&self) -> Clock {
        self.clock
    }

    /// What `enter` does: with the field open, its value resolved on the
    /// clock (a DST gap is the one refusal, kept on `refusal` for the row
    /// to paint); otherwise the highlighted row's own instant.
    pub fn commit(&mut self) -> Result<Commit, String> {
        if let Some(field) = self.field.as_mut() {
            if let Err(segment) = field.complete_pending() {
                let msg = format!("finish the {} or backspace", segment.name());
                self.refusal = Some(msg.clone());
                return Err(msg);
            }
            let v = field.value();
            return match self.clock.resolve_local(v.date(), v.time()) {
                Ok(t) => Ok(Commit::At(t)),
                Err(e) => {
                    let msg = e.to_string();
                    self.refusal = Some(msg.clone());
                    Err(msg)
                }
            };
        }
        let Some(p) = self.ranked.get(self.highlighted) else {
            return Err("nothing to set".into());
        };
        match &p.row {
            Row::Live => Ok(Commit::Live),
            Row::Custom => Err("tab opens the custom time".into()),
            row => self
                .instant_of(row)
                .map(Commit::At)
                .ok_or_else(|| "nothing to set".into()),
        }
    }
}
```

Note `open_field` clears a non-empty query so the Custom row is always painted while the field is open; the test's third block asserts it.

- [ ] **Step 4: Run** — `cargo test -p geode-shell asof_rows` → green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell/src/shell/asof_rows.rs crates/geode-shell/src/shell/mod.rs
git commit -m "shell: the as-of dialog's pure row model (sections, ranking, digit jump, custom seed, commit)"
```

---

### Task 2: The dialog — `asof_view.rs` rewritten, calendar and free-text grammar removed

**Files:**
- Modify: `crates/geode-shell/src/shell/asof_view.rs` (rewrite), `crates/geode-shell/src/shell/mod.rs` (lines ~928–943 fields, ~1193–1229 the subscription arm and calendar subscription, ~1642–1643 init, ~1910 accessor), `crates/geode-shell/Cargo.toml` (drop `gpui-base` and its comment), root `Cargo.toml:64-72` (comment: `geode-app` alone depends on `gpui-base`, for the pin)
- Modify: `crates/geode-shell/src/shell/tests/asof.rs` (rewrite; delete the seven calendar tests)

**Interfaces:**
- Consumes: `asof_rows::{AsOfState, Row, Section, Commit}`, `geode_widgets::datefield::{route, FieldKey, paint, SegmentPaint, SegmentText}`, `dialog::{open_shell_dialog_with_key, filter_row, hint_rows}`, `footer::{Hint, HintRow}`, `listrow::row_paint`, `keybindings_view::highlighted_text`, `ShellView::clock`.
- Produces: `asof_view::open(view, window, cx)` (unchanged signature), `asof_view::on_query_changed(state: &mut AsOfState, text: &str) -> bool` (the subscription arm's call), `asof_view::segment_paint(theme) -> SegmentPaint`.

- [ ] **Step 1: Write the failing window tests** (`tests/asof.rs`, replacing the file's body; keep the module doc and the two tooltip tests, `hovering_the_status_as_of_segment_names_the_selector_chord` and `hovering_the_as_of_badge_names_the_selector_chord`, and `the_as_of_chip_leads_the_bar_and_opens_the_selector` — that last one must TYPE after the click: add `vcx.simulate_input("eod"); assert_eq!(shell.read_with(&vcx, |s, _| s.as_of_dialog.as_ref().unwrap().query().to_string()), "eod");` after its open assertion)

```rust
fn open_as_of(cx: &mut gpui::TestAppContext) -> (Entity<ShellView>, gpui::VisualTestContext) {
    let (window, mut vcx) = open_shell(cx, test_services());
    let shell = shell_of(&window, &mut vcx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    let newest = chrono::Utc::now();
    frame.update(&mut vcx, |f, _| {
        f.note_published(Publish { dataset: "risk".into(), batch: "EOD".into(), books: 12, at: newest });
    });
    vcx.simulate_keystrokes("alt-t");
    (shell, vcx)
}

fn state_of(shell: &Entity<ShellView>, vcx: &gpui::VisualTestContext) -> crate::shell::asof_rows::AsOfState {
    shell.read_with(vcx, |s, _| s.as_of_dialog.clone().expect("dialog open"))
}

#[gpui::test]
fn typing_eod_and_enter_commits_eod_t_minus_one(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_input("eod");
    let s = state_of(&shell, &vcx);
    assert_eq!(s.painted()[0].label, "EOD T-1");
    let expected = match s.painted()[0].row { crate::shell::asof_rows::Row::Preset(_) => s.clone().commit().unwrap(), _ => panic!() };
    vcx.simulate_keystrokes("enter");
    let crate::shell::asof_rows::Commit::At(t) = expected else { panic!() };
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(t));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-stripe").is_some());
}

#[gpui::test]
fn the_list_takes_nav_keys_and_a_digit_jumps_on_an_empty_field(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("ctrl-n");
    assert_eq!(state_of(&shell, &vcx).highlighted(), 1, "ctrl+n is down");
    vcx.simulate_keystrokes("up");
    assert_eq!(state_of(&shell, &vcx).highlighted(), 0);
    vcx.simulate_keystrokes("2");
    let s = shell.read_with(&vcx, |s, _| s.as_of_dialog.is_none());
    assert!(s, "a digit on an empty field committed and closed");
    assert!(matches!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(_)));
}

#[gpui::test]
fn a_digit_after_typing_is_a_filter_character(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_input("t-");
    vcx.simulate_keystrokes("1");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()));
    assert_eq!(state_of(&shell, &vcx).query(), "t-1");
    assert_eq!(state_of(&shell, &vcx).painted()[0].label, "EOD T-1");
}

#[gpui::test]
fn tab_opens_the_custom_field_up_steps_the_day_and_enter_commits(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("tab");
    let s = state_of(&shell, &vcx);
    let field = s.field().expect("tab opened the field");
    let before = field.value();
    assert_eq!(field.segment(), geode_widgets::datefield::Segment::Day);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-custom-seg-2").is_some(), "the day segment is painted");
    assert!(vcx.debug_bounds("as-of-custom-seg-suffix").is_some(), "the zone suffix is painted");
    vcx.simulate_keystrokes("up");
    let after = state_of(&shell, &vcx).field().unwrap().value();
    assert_eq!(after, before + chrono::Duration::days(1));
    vcx.simulate_keystrokes("enter");
    let clock = shell.read_with(&vcx, |s, cx| s.clock(cx));
    let expected = clock.resolve_local(after.date(), after.time()).unwrap();
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(expected));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn escape_closes_the_field_first_and_the_dialog_second(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    assert!(state_of(&shell, &vcx).field().is_some());
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_some()), "first escape: field closed, dialog up");
    assert!(state_of(&shell, &vcx).field().is_none());
    vcx.simulate_keystrokes("escape");
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn a_chord_inside_the_open_field_still_reaches_the_shell(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.simulate_keystrokes("tab");
    // `ctrl+n` is a nav chord, not the field's: the field stays open and
    // the highlight does not move off Custom (nav is refused while the
    // field is open — see handle_key), which is the observable "not typed
    // into the field" outcome.
    let before = state_of(&shell, &vcx).field().unwrap().clone();
    vcx.simulate_keystrokes("ctrl-n");
    assert_eq!(state_of(&shell, &vcx).field().unwrap(), &before);
}

#[gpui::test]
fn a_row_click_commits_it(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.run_until_parked();
    let row = vcx.debug_bounds("as-of-row-1").expect("second row painted");
    vcx.simulate_mouse_down(row.center(), gpui::MouseButton::Left, gpui::Modifiers::none());
    assert!(matches!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(_)));
    assert!(shell.read_with(&vcx, |s, _| s.modal.is_none()));
}

#[gpui::test]
fn while_pinned_current_and_live_lead_and_live_returns_to_live(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    let frame = shell.read_with(&vcx, |s, _| s.frame().clone());
    vcx.simulate_keystrokes("enter"); // EOD T-1
    assert!(matches!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::At(_)));
    vcx.simulate_keystrokes("alt-t");
    let s = state_of(&shell, &vcx);
    assert_eq!(s.painted()[0].label, "current");
    assert_eq!(s.painted()[1].label, "live");
    vcx.simulate_keystrokes("down enter");
    assert_eq!(frame.read_with(&vcx, |f, _| f.as_of().clone()), AsOf::Live);
}

#[gpui::test]
fn the_footer_swaps_to_the_fields_keys_while_it_is_open(cx: &mut gpui::TestAppContext) {
    let (shell, mut vcx) = open_as_of(cx);
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-hint-tab").is_some());
    assert!(vcx.debug_bounds("as-of-hint-step").is_none());
    vcx.simulate_keystrokes("tab");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("as-of-hint-step").is_some());
    assert!(vcx.debug_bounds("as-of-hint-tab").is_none());
    let _ = shell;
}
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-shell tests::asof` → compile errors (`as_of_dialog` type, selectors).

- [ ] **Step 3: Implement**

`crates/geode-shell/src/shell/asof_view.rs`, whole file:

```rust
//! The as-of selector (as-of dialog spec 2026-09-20 §5): a filter-first
//! modal over [`AsOfState`]'s ranked rows — `Current`/`Live` while pinned,
//! the business-day presets, the `Custom` row holding the segmented
//! date-time field, the recent publishes — each painted with the instant
//! it resolves to on the configured clock. The pure model is
//! `asof_rows`; this file is the gpui half: `open`, the modal key
//! handler and `build`.
//!
//! Keys (§5.2): `listfilter::nav_command` moves; `1`–`5` on an EMPTY
//! field jump to a preset; `enter` commits the highlighted row; `tab`
//! opens the Custom field (and leaves it); while the field is open every
//! bare key goes through `geode_widgets::datefield::route` — `enter`
//! commits the field's value, `escape` closes the field, a chord is not
//! the field's and falls through to the shell. A second `escape` closes
//! the dialog through `handle_key_down`'s own modal branch.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Hsla, MouseButton, SharedString, Window, div};
use gpui_component::{ActiveTheme as _, Theme, h_flex, v_flex};

use geode_core::colour::{READABLE_RATIO, contrast_ratio, readable_on};
use geode_core::query::AsOf;
use geode_widgets::datefield::{FieldKey, SegmentPaint, route};

use crate::footer::{Hint, HintRow};
use crate::keymap::{Keystroke, Modifiers};
use crate::listfilter;

use super::asof_rows::{AsOfState, Commit, Row, Section};
use super::colours::{over, to_hsla, to_rgb};
use super::{ShellView, dialog, scale};

pub use super::asof_rows::AsOfState as State;

/// Dialog content width at the design rem (the picker's 480 was too
/// narrow for a label and a dated right column side by side).
const WIDTH: f32 = 640.0;

/// Open the dialog (`frame::as_of`, `mod+t`, the toolbar chip). A no-op
/// if a modal is already open, like every other `open` here. The state is
/// built fresh from the frame, the clock and `now` — nothing survives a
/// close/reopen.
pub fn open(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.modal.is_some() {
        return;
    }
    let clock = view.clock(cx);
    let frame = view.frame.read(cx);
    let publishes: Vec<_> = frame.recent_publishes().iter().cloned().collect();
    view.as_of_dialog = Some(AsOfState::build(
        frame.as_of(),
        &publishes,
        clock,
        chrono::Utc::now(),
    ));
    let entity = cx.entity();
    dialog::open_shell_dialog_with_key(
        view,
        window,
        cx,
        "As of",
        move |shell, window, cx| build(shell, &entity, window, cx),
        Some(Rc::new(handle_key)),
        true,
    );
}

/// The `InputEvent::Change` arm's pure half (`shell/mod.rs`): the field's
/// text is the query. Answers whether the ranking changed.
pub fn on_query_changed(state: &mut AsOfState, text: &str) -> bool {
    state.set_query(text)
}

fn apply_commit(shell: &mut ShellView, commit: Commit, window: &mut Window, cx: &mut Context<ShellView>) {
    shell.frame.update(cx, |f, cx| {
        let next = match commit {
            Commit::At(t) => AsOf::At(t),
            Commit::Live => AsOf::Live,
        };
        if f.set_as_of(next) {
            cx.notify();
        }
    });
    shell.close_modal(window, cx);
}

/// The [`dialog::ModalKeyHandler`] for this modal. Every arm is a pure
/// mutation of [`AsOfState`] plus, on a commit, `apply_commit`; the
/// shared field's text is reconciled by `sync_dialog_text` after this
/// returns, as for every dialog.
fn handle_key(
    shell: &mut ShellView,
    ks: &Keystroke,
    window: &mut Window,
    cx: &mut Context<ShellView>,
) -> bool {
    let Some(state) = shell.as_of_dialog.as_mut() else {
        return false;
    };
    // The field owns every bare key while it is open (§5.2): `route`
    // decides, a chord answers `None` and is left to the shell.
    if state.field().is_some() {
        if ks.mods == Modifiers::NONE && ks.key == "tab" {
            state.close_field();
            cx.notify();
            return true;
        }
        let Some(key) = route(&ks.key, ks.mods.shift, ks.mods.is_chord()) else {
            return false;
        };
        match key {
            FieldKey::Commit => match state.commit() {
                Ok(commit) => apply_commit(shell, commit, window, cx),
                Err(_) => cx.notify(),
            },
            FieldKey::Cancel => {
                state.close_field();
                cx.notify();
            }
            other => {
                if let Some(field) = state.field_mut() {
                    field.apply(other);
                }
                cx.notify();
            }
        }
        return true;
    }
    if ks.mods == Modifiers::NONE {
        match ks.key.as_str() {
            "enter" => {
                let live = shell.dialog_input.read(cx).value().to_string();
                let Some(state) = shell.as_of_dialog.as_mut() else { return false };
                // `set_value` emits no `Change`: re-feed the live text
                // before trusting the highlight (the choice dialogs' rule).
                state.set_query(&live);
                match state.commit() {
                    Ok(commit) => apply_commit(shell, commit, window, cx),
                    Err(_) => cx.notify(),
                }
                return true;
            }
            "tab" => {
                state.open_field();
                cx.notify();
                return true;
            }
            key => {
                if let Some(commit) = state.jump_digit(key) {
                    apply_commit(shell, commit, window, cx);
                    return true;
                }
            }
        }
    }
    if let Some(cmd) = listfilter::nav_command(ks) {
        state.nav(cmd);
        cx.notify();
        return true;
    }
    false
}

/// The Custom row's segment colours over the popover: the active segment
/// on `primary` in `primary_foreground` floored to the readable ratio
/// against it, a typing segment on `accent` in `accent_foreground`
/// floored the same way, the rest bare in `foreground`. The sweep
/// `segment_colours_are_readable_on_every_bundled_theme` checks all
/// three on every theme.
pub fn segment_paint(theme: &Theme) -> SegmentPaint {
    let popover = to_rgb(theme.popover);
    let floored = |text: Hsla, fill: Hsla| -> Hsla {
        let ground = over(fill, popover);
        to_hsla(readable_on(to_rgb(text), ground, to_rgb(theme.foreground)))
    };
    SegmentPaint {
        rest_text: theme.foreground,
        rest_fill: None,
        active_text: floored(theme.primary_foreground, theme.primary),
        active_fill: theme.primary,
        typing_text: floored(theme.accent_foreground, theme.accent),
        typing_fill: theme.accent,
        separator: theme.muted_foreground,
        suffix: theme.muted_foreground,
        radius: theme.radius_tokens().sm,
    }
}

fn build(
    shell: &ShellView,
    entity: &Entity<ShellView>,
    _window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(state) = shell.as_of_dialog.as_ref() else {
        return div().into_any_element();
    };
    let theme = cx.theme();
    let paint = super::listrow::row_paint(theme);
    let muted = theme.muted_foreground;
    let danger = theme.danger;
    let radius = theme.radius;
    let segment_paint = segment_paint(theme);
    let clock = state.clock();

    let mut list = v_flex()
        .id("as-of-rows")
        .w_full()
        .gap_0p5()
        .overflow_y_scroll()
        .debug_selector(|| "as-of-rows".to_string());
    if state.painted().is_empty() {
        list = list.child(
            div()
                .px_2()
                .py_1()
                .text_sm()
                .text_color(muted)
                .child("nothing matches — escape clears"),
        );
    }
    let mut last_section: Option<Section> = None;
    for (position, row) in state.painted().iter().enumerate() {
        if last_section != Some(row.section) {
            if let Some(eyebrow) = row.section.eyebrow() {
                list = list.child(
                    div()
                        .px_2()
                        .pt_2()
                        .pb_0p5()
                        .text_xs()
                        .text_color(muted)
                        .child(eyebrow.to_uppercase()),
                );
            }
            last_section = Some(row.section);
        }
        let is_highlighted = position == state.highlighted();
        let mut el = h_flex()
            .w_full()
            .h(scale::design(28.))
            .flex_shrink_0()
            .px_2()
            .items_center()
            .gap_2()
            .text_sm()
            .rounded(radius)
            .debug_selector(move || format!("as-of-row-{position}"));
        if is_highlighted {
            el = el.bg(paint.active).text_color(paint.text);
        } else {
            el = el.hover(move |s| s.bg(paint.hover));
        }
        let entity_for_click = entity.clone();
        let is_custom = matches!(row.row, Row::Custom);
        el = el.child(super::keybindings_view::highlighted_text(
            &row.label,
            &row.indices,
            paint.accent,
        ));
        if is_custom {
            if let Some(field) = state.field() {
                let segments = field.segments();
                let suffix: SharedString = clock.abbreviation(chrono::Utc::now()).into();
                let seg_entity = entity.clone();
                el = el.child(
                    div().font_family(crate::fonts::MONO).child(geode_widgets::datefield::paint(
                        &segments,
                        Some(suffix),
                        segment_paint,
                        "as-of-custom-seg".into(),
                        move |segment, _window, cx| {
                            seg_entity.update(cx, |shell, cx| {
                                if let Some(f) = shell.as_of_dialog.as_mut().and_then(|s| s.field_mut()) {
                                    f.select(segment);
                                }
                                cx.notify();
                            });
                        },
                    )),
                );
                if let Some(refusal) = state.field_refusal() {
                    el = el.child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .text_color(danger)
                            .debug_selector(|| "as-of-custom-refusal".to_string())
                            .child(refusal.to_string()),
                    );
                }
            } else {
                el = el.child(div().ml_auto().text_xs().text_color(muted).child("tab edits"));
            }
        } else {
            el = el.child(
                div()
                    .ml_auto()
                    .font_family(crate::fonts::MONO)
                    .text_xs()
                    .text_color(if is_highlighted { paint.text } else { muted })
                    .child(row.right.clone()),
            );
        }
        el = el.on_mouse_down(MouseButton::Left, move |_event, window, cx| {
            entity_for_click.update(cx, |shell, cx| {
                let Some(state) = shell.as_of_dialog.as_mut() else { return };
                if is_custom {
                    // A click on the Custom row's body (not a segment —
                    // the painter stops propagation there) opens the
                    // field, like `tab`.
                    if state.field().is_none() {
                        state.open_field();
                    }
                    cx.notify();
                    return;
                }
                if state.field().is_some() {
                    state.close_field();
                }
                if state.set_highlighted(position) {
                    match state.commit() {
                        Ok(commit) => apply_commit(shell, commit, window, cx),
                        Err(_) => cx.notify(),
                    }
                }
            });
        });
        list = list.child(el);
    }

    let hints: Vec<Hint> = if state.field().is_some() {
        vec![
            Hint::new(HintRow::Move, &["left", "right"], "segment"),
            Hint::new(HintRow::Move, &["up", "down"], "step").selector("as-of-hint-step"),
            Hint::new(HintRow::Move, &["shift+up"], "×10"),
            Hint::range(HintRow::Edit, "0", "9", "type"),
            Hint::new(HintRow::Edit, &["backspace"], "clear segment"),
            Hint::new(HintRow::Go, &["enter"], "set as-of"),
            Hint::new(HintRow::Go, &["escape"], "back to list"),
        ]
    } else {
        vec![
            Hint::prose(HintRow::Move, "type to filter"),
            Hint::new(HintRow::Move, &["up", "down"], "row"),
            Hint::range(HintRow::Move, "1", "5", "preset"),
            Hint::new(HintRow::Edit, &["tab"], "custom time").selector("as-of-hint-tab"),
            Hint::new(HintRow::Go, &["enter"], "set as-of"),
            Hint::new(HintRow::Go, &["escape"], "close"),
        ]
    };
    let footer = v_flex()
        .w_full()
        .gap_1()
        .pt_2()
        .border_t_1()
        .border_color(theme.border)
        .child(dialog::hint_rows(&hints, theme.muted_foreground, theme.muted, theme.radius));

    v_flex()
        .gap_2()
        .w(scale::design(WIDTH))
        .child(dialog::filter_row(&shell.dialog_input, None, cx))
        .child(list)
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_component::ActiveTheme as _;

    /// The three segment states over the popover on every bundled theme,
    /// no exception list — the same floor `chip_paint` and `row_paint`
    /// keep.
    #[gpui::test]
    fn segment_colours_are_readable_on_every_bundled_theme(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let (service, _) = crate::theme::load_bundled();
        let mut failures = Vec::new();
        let mut checked = 0;
        for name in service.names() {
            let entry = service.resolve(&name).unwrap().clone();
            cx.update(|cx| {
                Theme::global_mut(cx).apply_config(&entry);
                let theme = cx.theme();
                let p = segment_paint(theme);
                let popover = to_rgb(theme.popover);
                for (state, text, fill) in [
                    ("rest", p.rest_text, None),
                    ("active", p.active_text, Some(p.active_fill)),
                    ("typing", p.typing_text, Some(p.typing_fill)),
                ] {
                    checked += 1;
                    let ground = fill.map(|f| over(f, popover)).unwrap_or(popover);
                    let ratio = contrast_ratio(to_rgb(text), ground);
                    if ratio < READABLE_RATIO {
                        failures.push(format!("{name}: {state} at {ratio:.2}:1"));
                    }
                }
            });
        }
        assert!(checked >= 3 * 40, "the sweep saw {checked} checks — bundled themes missing?");
        assert!(failures.is_empty(), "unreadable segments:\n{}", failures.join("\n"));
    }
}
```

`Hint::new`, `Hint::prose`, `Hint::range(row, from, to, word)` and `.selector("…")` are `crate::footer::Hint`'s existing constructors.

`shell/mod.rs`:
- field `as_of_dialog: Option<asof_view::AsOfState>` stays (the type now re-exported from `asof_rows`); delete `as_of_calendar` (field, its `cx.new(CalendarState::new)` and `subscribe_in` block, the `as_of_calendar()` accessor, the `as_of_calendar,` init).
- the subscription arm (~1193):

```rust
            } else if let Some(state) = view.as_of_dialog.as_mut() {
                // The field's text is the query (spec §5.1).
                asof_view::on_query_changed(state, &query);
            }
```

- `close_modal`'s `self.as_of_dialog = None;` stays.

`crates/geode-shell/Cargo.toml`: delete the `gpui-base.workspace = true` line and its comment. Root `Cargo.toml` comment (lines 64–72): replace the last sentence with "`geode-app` depends on both for that reason alone (the shell's one former use, `CalendarView` in the as-of dialog, went with the calendar on 2026-09-20)."

`shell/render.rs:1051` and `input.rs:394` call `asof_view::open` unchanged.

- [ ] **Step 4: Run** — `cargo test -p geode-shell asof && cargo test -p geode-shell && cargo clippy -p geode-shell --all-targets -- -D warnings` → green. Any test elsewhere in `shell/tests` that referenced `as_of_calendar` or `compose_with_date` is deleted with the calendar.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell Cargo.toml Cargo.lock
git commit -m "shell: the as-of dialog — one ranked list with presets, custom segmented field and publishes; calendar and free-text grammar removed"
```

---

### Task 3: Harness entries, docs, the as-built section

**Files:**
- Modify: `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md`, `docs/superpowers/specs/2026-09-20-geode-as-of-dialog-design.md` (an "As built" section)

- [ ] **Step 1: Harness entries**

```zsh
# As-of dialog Part 3 (2026-09-20): the dialog. Current/Live only while
# pinned — under live, `Live` is a no-op row and `Current` has no instant.
run_mutation "asof: current and live rows only while pinned" \
  crates/geode-shell/src/shell/asof_rows.rs \
  '        if let Some(t) = pinned {' \
  '        if let Some(t) = pinned.or(Some(now)) {' \
  geode-shell \
  under_live_the_rows_are_presets_custom_and_publishes_in_section_order

# A digit jumps only on an EMPTY query; typed, it is a filter character.
run_mutation "asof: the digit jump is gated on an empty query" \
  crates/geode-shell/src/shell/asof_rows.rs \
  '        if !self.query.is_empty() {
            return None;
        }' \
  '        let _ = &self.query;' \
  geode-shell \
  a_digit_jumps_only_on_an_empty_query

# `tab` seeds the field from the highlighted row, not from now.
run_mutation "asof: tab seeds the field from the highlighted row" \
  crates/geode-shell/src/shell/asof_rows.rs \
  '            .and_then(|p| self.instant_of(&p.row))
            .or(self.pinned)' \
  '            .and_then(|_p| None::<DateTime<Utc>>)' \
  geode-shell \
  tab_seeds_the_field_from_the_highlighted_row_or_the_pin_or_now

# The right column is paint, never matched: ranking over it would make
# "18" light every preset.
run_mutation "asof: the right column is not matched" \
  crates/geode-shell/src/shell/asof_rows.rs \
  '            let texts: Vec<String> = members.iter().map(|i| self.entries[*i].label.clone()).collect();' \
  '            let texts: Vec<String> = members.iter().map(|i| format!("{} {}", self.entries[*i].label, self.entries[*i].right)).collect();' \
  geode-shell \
  the_right_column_is_never_matched

# A DST-gap value is refused with the field left open, never committed.
run_mutation "asof: a DST-gap custom value is refused" \
  crates/geode-shell/src/shell/asof_rows.rs \
  '            return match self.clock.resolve_local(v.date(), v.time()) {' \
  '            return match self.clock.resolve_local(v.date(), v.time()).or_else(|_| Ok::<_, geode_core::clock::ClockError>(self.now)) {' \
  geode-shell \
  a_field_value_in_a_dst_gap_is_refused_and_named_on_the_row

# While the field is open every bare key is the field's; a chord is not.
run_mutation "asof: a chord is not claimed while the field is open" \
  crates/geode-shell/src/shell/asof_view.rs \
  '        let Some(key) = route(&ks.key, ks.mods.shift, ks.mods.is_chord()) else {
            return false;
        };' \
  '        let Some(key) = route(&ks.key, ks.mods.shift, false) else {
            return true;
        };' \
  geode-shell \
  a_chord_inside_the_open_field_still_reaches_the_shell

# `escape` with the field open closes the FIELD, not the dialog.
run_mutation "asof: escape closes the field before the dialog" \
  crates/geode-shell/src/shell/asof_view.rs \
  '            FieldKey::Cancel => {
                state.close_field();
                cx.notify();
            }' \
  '            FieldKey::Cancel => {
                return false;
            }' \
  geode-shell \
  escape_closes_the_field_first_and_the_dialog_second
```

If `cargo fmt` reflowed any anchor, copy the exact first line(s) from the file. Update the entry count in `CLAUDE.md`.

- [ ] **Step 2: Run** — `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "asof:"` → 0 and every entry `CAUGHT`.

- [ ] **Step 3: Docs**

`CLAUDE.md`:
- Status table: `| As-of dialog Part 3 (2026-09-20) | `mod+t` is one ranked list: Current/Live while pinned, the five presets, a Custom row holding the segmented field, recent publishes; `1`–`5` jump, `tab` edits, the calendar and the field grammar are gone (`geode-shell` no longer depends on `gpui-base`). Display checks pending. | `2026-09-20-…as-of-dialog` §5 |`
- Load-bearing rule under "Shell: frame, tiles, diagnostics": `- The as-of dialog (2026-09-20) is filter-first over `asof_rows::AsOfState`: `Current`/`Live` rows exist only while pinned; the right column is paint and never matched; `1`–`5` jump only on an EMPTY query; `tab` opens the Custom field seeded from the highlighted row (else the pin, else now) and while it is open every bare key goes through `geode_widgets::datefield::route` — a chord answers `None` and reaches the shell, `escape` closes the field and only a second `escape` the dialog; a DST-gap value is refused on the row with the field left open. `parse_as_of` remains for the tile-local `:asof`.`
- The "Workspace invariants" bullet on the pin: remove the `CalendarView` sentence and the "only use of the unstyled layer outside geode-app's pin" clause.

`docs/phase-history.md`: a Part 3 paragraph (the rulings, the dropped calendar and `gpui-base` dependency, the harness entries, the pending display checks).

Spec "As built" section (append to the spec): what shipped, the row selectors (`as-of-row-N`, `as-of-custom-seg-N`, `as-of-custom-seg-suffix`, `as-of-hint-tab`, `as-of-hint-step`), and the display checks pending (§9).

- [ ] **Step 4: Full verification and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run && cargo check -p geode-shell --features test-support --all-targets`

```bash
git add scripts/mutation-check.sh CLAUDE.md docs/phase-history.md docs/superpowers/specs/2026-09-20-geode-as-of-dialog-design.md
git commit -m "asof: harness entries, rules and history for the redesigned dialog"
```
