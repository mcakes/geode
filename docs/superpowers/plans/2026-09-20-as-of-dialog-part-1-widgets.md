# As-of dialog Part 1: `geode-widgets` and the date-time field — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A new `geode-widgets` crate holding the segmented date-time field (pure core, one key table, one painter), with the market-data panel's date field migrated onto it and no visible change.

**Architecture:** `crates/geode-widgets` sits below the shell (depends on `geode-core`, `gpui`, `gpui-component`; never on `geode-shell` or a module). Its `datefield` module is today's `geode_marketdata::core::datefield`, moved and generalised to a `DateTimeField` with a `Precision` (`Date` | `DateTime`), plus `route`/`FieldKey`/`apply` and a colour-parameterised `paint`. The panel keeps its editor ownership (`EditorState::Date`, focus handle, container, key listener) and calls into the crate for everything segment-shaped.

**Tech Stack:** Rust 2024, chrono 0.4, gpui / gpui-component 0.6.2 (pinned), criterion (bench = false on the lib), the mutation harness `scripts/mutation-check.sh`.

**Spec:** `docs/superpowers/specs/2026-09-20-geode-as-of-dialog-design.md` §4 (and §2 ruling 3).

## Global Constraints

- Every new lib target has `bench = false` (CLAUDE.md, workspace invariant).
- A new crate's dev-dependencies enable the same `geode-*` `test-support` features the workspace does (test-feature parity, commit `cccd2fb`).
- Both CI platforms (macOS, Windows) must build; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run` all green.
- No crate other than `geode-data` opens a file or socket; `geode-widgets` never depends on `geode-shell` or any module.
- The painter never reads `cx.theme()`: every colour is handed in (the `key_chip` precedent), so it can be called from closures that cannot borrow the theme.
- The market-data panel's behaviour does not change; its existing date-field tests are the migration's check and stay green without edits to their assertions.
- Every load-bearing branch changed gets a `run_mutation` entry naming its test; `zsh scripts/mutation-check.sh --anchors-only` exits 0 before merge.
- Commit before mutating (the harness restores files with `git checkout`).

---

## File structure

| File | Responsibility |
|---|---|
| `Cargo.toml` (root) | add `crates/geode-widgets` to `members`; add `geode-widgets = { path = "crates/geode-widgets" }` to `[workspace.dependencies]` |
| `crates/geode-widgets/Cargo.toml` | new crate manifest |
| `crates/geode-widgets/src/lib.rs` | crate doc + `pub mod datefield;` |
| `crates/geode-widgets/src/datefield/mod.rs` | the pure `DateTimeField` core: `Precision`, `Segment`, `SegmentText`, `FieldKey`, `route`, `apply` |
| `crates/geode-widgets/src/datefield/paint.rs` | `SegmentPaint` and `paint(...)` — the gpui painter |
| `crates/geode-marketdata/src/core/datefield.rs` | **deleted** (moved) |
| `crates/geode-marketdata/src/core/mod.rs` | re-export from `geode_widgets` |
| `crates/geode-marketdata/Cargo.toml` | depend on `geode-widgets` |
| `crates/geode-marketdata/src/tile.rs` | `EditorState::Date` holds a `DateTimeField`; `date_field_key` uses `route`/`apply`; `DateFieldPaint::of` reads `segments()` |
| `crates/geode-marketdata/src/header.rs` | `render_date_field` builds a `SegmentPaint` and calls the crate's `paint` inside its own container |
| `scripts/mutation-check.sh` | four `widgets:` entries, one `marketdata:` entry |
| `CLAUDE.md`, `docs/phase-history.md` | architecture tree, status row, history paragraph |

---

### Task 1: The crate, with the date field moved verbatim

**Files:**
- Create: `crates/geode-widgets/Cargo.toml`, `crates/geode-widgets/src/lib.rs`, `crates/geode-widgets/src/datefield/mod.rs`
- Modify: `Cargo.toml` (root, `members` and `[workspace.dependencies]`), `crates/geode-marketdata/Cargo.toml`, `crates/geode-marketdata/src/core/mod.rs`
- Delete: `crates/geode-marketdata/src/core/datefield.rs`

**Interfaces:**
- Produces: crate `geode_widgets` with `geode_widgets::datefield::{DateField, Segment, SegmentText}` — the exact API the panel uses today (`DateField::open(NaiveDate)`, `left`, `right`, `select`, `step`, `digit`, `complete_pending`, `backspace`, `text`, `segments() -> [SegmentText; 3]`, `value() -> NaiveDate`, public field `segment`).

- [ ] **Step 1: Manifest and root wiring**

`crates/geode-widgets/Cargo.toml`:

```toml
[package]
name = "geode-widgets"
version.workspace = true
edition.workspace = true
publish.workspace = true

[lib]
bench = false

# Shared widgets: a pure core per widget plus a colour-parameterised gpui
# painter (as-of dialog spec 2026-09-20 §4). Below the shell on purpose —
# `geode-shell` and every module depend on this crate, never the reverse —
# so two hosts paint one widget through one door and cannot drift.
[dependencies]
geode-core.workspace = true
gpui.workspace = true
gpui-component.workspace = true
chrono = "0.4.42"

[dev-dependencies]
# Test-feature parity (2026-09-19): match the workspace's `geode-*`
# features so `cargo test -p geode-widgets` shares the workspace graph.
geode-core = { workspace = true, features = ["test-support"] }
gpui = { workspace = true, features = ["test-support"] }
```

Root `Cargo.toml`: add `"crates/geode-widgets",` to `members` after `"crates/geode-shell",`, and `geode-widgets = { path = "crates/geode-widgets" }` under `[workspace.dependencies]` beside `geode-shell`.

`crates/geode-widgets/src/lib.rs`:

```rust
//! Shared widgets (as-of dialog spec 2026-09-20 §4): each widget is a
//! pure core (no `gpui`, unit-testable) beside a painter that takes its
//! colours as a value, so the shell and any module paint the same thing
//! through the same door. Below the shell in the dependency graph —
//! nothing here may name `geode_shell` or a module.

pub mod datefield;
```

- [ ] **Step 2: Move the file**

```bash
mkdir -p crates/geode-widgets/src/datefield
git mv crates/geode-marketdata/src/core/datefield.rs crates/geode-widgets/src/datefield/mod.rs
```

Edit `crates/geode-marketdata/src/core/mod.rs`: replace `pub mod datefield;` with nothing, and replace

```rust
pub use datefield::{DateField, Segment, SegmentText};
```

with

```rust
pub use geode_widgets::datefield::{DateField, Segment, SegmentText};
```

Edit `crates/geode-marketdata/Cargo.toml` `[dependencies]`: add `geode-widgets.workspace = true` after `geode-shell.workspace = true`, and extend the block comment above with: `` `geode-widgets` for the segmented date field (its core and painter live there since the as-of dialog work, 2026-09-20). ``

- [ ] **Step 3: Build and run both suites**

Run: `cargo test -p geode-widgets && cargo test -p geode-marketdata datefield && cargo test -p geode-marketdata date_`
Expected: the 21 moved tests pass under `geode-widgets`; every panel test whose name contains `date_` passes unchanged.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/geode-widgets crates/geode-marketdata/Cargo.toml crates/geode-marketdata/src/core/mod.rs
git commit -m "widgets: new crate; move the date field core out of the market-data panel"
```

---

### Task 2: `DateTimeField` — precision and the three time segments

**Files:**
- Modify: `crates/geode-widgets/src/datefield/mod.rs`
- Modify: `crates/geode-marketdata/src/core/mod.rs`, `crates/geode-marketdata/src/tile.rs` (lines near 276–310 `EditorState::Date`/`DateFieldPaint::of`, 2059–2061 and 2855 the two `DateField::open` sites, 4119 `date_field`), `crates/geode-marketdata/src/header.rs:159` (`Segment::at`)
- Test: `crates/geode-widgets/src/datefield/mod.rs` `#[cfg(test)]`, the panel's tests at `tile.rs` ~9594 (`date_segments` helper)

**Interfaces:**
- Produces:

```rust
pub enum Precision { Date, DateTime }
pub enum Segment { Year, Month, Day, Hour, Minute, Second }
impl Segment {
    pub fn index(self) -> usize;                 // painted position, 0..=5
    pub fn name(self) -> &'static str;           // "year" … "second"
    pub fn at(index: usize) -> Option<Segment>;  // inverse of index
    pub fn last(precision: Precision) -> Segment; // Day | Second
    pub fn fits(self, precision: Precision) -> bool;
}
pub struct SegmentText { pub text: String, pub active: bool, pub typing: bool }
pub struct DateTimeField { /* private */ }
impl DateTimeField {
    pub fn open(value: NaiveDateTime, precision: Precision, segment: Segment) -> Self;
    pub fn precision(&self) -> Precision;
    pub fn segment(&self) -> Segment;
    pub fn left(&mut self); pub fn right(&mut self);
    pub fn select(&mut self, segment: Segment) -> bool;   // false past the precision
    pub fn step(&mut self, n: i64);
    pub fn digit(&mut self, d: u8) -> bool;
    pub fn complete_pending(&mut self) -> Result<(), Segment>;
    pub fn backspace(&mut self);
    pub fn text(&self) -> String;            // "YYYY-MM-DD" or "YYYY-MM-DD HH:MM:SS"
    pub fn segments(&self) -> Vec<SegmentText>; // precision's segments, painted order
    pub fn value(&self) -> NaiveDateTime;
    pub fn date(&self) -> NaiveDate;
}
```

`DateField` no longer exists; the panel names `DateTimeField`.

- [ ] **Step 1: Write the failing tests** (append to the module's `tests`)

```rust
    fn dt(y: i32, m: u32, day: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, day)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    fn texts_of(f: &DateTimeField) -> Vec<String> {
        f.segments().into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn a_date_time_field_paints_six_segments_and_a_date_field_three() {
        let full = DateTimeField::open(dt(2026, 9, 18, 18, 0, 0), Precision::DateTime, Segment::Day);
        assert_eq!(texts_of(&full), ["2026", "09", "18", "18", "00", "00"]);
        assert_eq!(full.text(), "2026-09-18 18:00:00");
        let date = DateTimeField::open(dt(2026, 9, 18, 18, 0, 0), Precision::Date, Segment::Day);
        assert_eq!(texts_of(&date), ["2026", "09", "18"]);
        assert_eq!(date.text(), "2026-09-18");
        assert_eq!(date.date(), d(2026, 9, 18));
    }

    #[test]
    fn right_stops_at_the_precisions_last_segment() {
        let mut date = DateTimeField::open(dt(2026, 9, 18, 0, 0, 0), Precision::Date, Segment::Day);
        date.right();
        assert_eq!(date.segment(), Segment::Day, "Date precision ends at the day");
        assert!(!date.select(Segment::Hour), "a segment past the precision is refused");
        assert_eq!(date.segment(), Segment::Day);

        let mut full = DateTimeField::open(dt(2026, 9, 18, 0, 0, 0), Precision::DateTime, Segment::Day);
        full.right();
        assert_eq!(full.segment(), Segment::Hour);
        full.right();
        full.right();
        assert_eq!(full.segment(), Segment::Second);
        full.right();
        assert_eq!(full.segment(), Segment::Second, "DateTime precision ends at the second");
        full.left();
        assert_eq!(full.segment(), Segment::Minute);
    }

    #[test]
    fn a_time_step_wraps_within_its_segment_without_carrying() {
        let mut f = DateTimeField::open(dt(2026, 9, 18, 23, 59, 59), Precision::DateTime, Segment::Hour);
        f.step(1);
        assert_eq!(f.value(), dt(2026, 9, 18, 0, 59, 59), "23 ↑ is 00, the day unchanged");
        f.step(-1);
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 59, 59));
        f.select(Segment::Minute);
        f.step(10);
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 9, 59), "59 + 10 wraps to 09");
        f.select(Segment::Second);
        f.step(-60);
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 9, 59), "a full turn is a no-op");
    }

    #[test]
    fn hour_typing_refuses_a_second_digit_past_twenty_three() {
        let mut f = DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::DateTime, Segment::Hour);
        assert!(!f.digit(2), "a leading 2 waits: 20–23 are still possible");
        assert!(!f.digit(5), "25 is refused, the 2 stays");
        assert_eq!(f.segments()[3].text, "2");
        assert!(f.digit(3), "23 completes and advances");
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 0, 0));
        assert_eq!(f.segment(), Segment::Minute);
        assert!(f.digit(7), "a leading 7 in the minute completes as 07");
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 7, 0));
        assert_eq!(f.segment(), Segment::Second);
    }

    #[test]
    fn a_pending_single_time_digit_completes_at_commit() {
        let mut f = DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::DateTime, Segment::Minute);
        assert!(!f.digit(4));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.value(), dt(2026, 9, 18, 10, 4, 0), "a lone 4 in the minute is :04");
        assert!(!f.digit(0), "a lone 0 can stand alone too — 00 is a valid minute");
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.value(), dt(2026, 9, 18, 10, 0, 0));
    }

    #[test]
    fn the_day_segment_still_advances_to_the_hour_under_date_time_and_stays_under_date() {
        let mut full = DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::DateTime, Segment::Day);
        assert!(full.digit(5), "5 completes as 05");
        assert_eq!(full.segment(), Segment::Hour);
        let mut date = DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::Date, Segment::Day);
        assert!(date.digit(5));
        assert_eq!(date.segment(), Segment::Day, "the panel's contract: the day stays the day");
    }
```

Also update the existing tests mechanically: every `DateField::open(d(y, m, day))` becomes `DateTimeField::open(d(y, m, day).and_hms_opt(0, 0, 0).unwrap(), Precision::Date, Segment::Day)`, every `f.value()` compared to a `NaiveDate` becomes `f.date()`, every `f.segment` becomes `f.segment()`, and `texts()` becomes

```rust
    fn texts(f: &DateTimeField) -> [String; 3] {
        let v = f.segments();
        [v[0].text.clone(), v[1].text.clone(), v[2].text.clone()]
    }
```

with the tuple assertion in `the_field_opens_on_the_day_segment_with_nothing_typed` reading `segs.iter().map(|s| (s.active, s.typing)).collect::<Vec<_>>()` against `vec![(false, false), (false, false), (true, false)]`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-widgets`
Expected: compile errors — `DateTimeField`, `Precision`, `Segment::Hour` do not exist.

- [ ] **Step 3: Implement**

Replace the top of `crates/geode-widgets/src/datefield/mod.rs` (everything down to the `clamped_ymd` helper; keep `clamped_ymd` and `days_in_month`) with:

```rust
//! The segmented date-time field (as-of dialog spec 2026-09-20 §4.2;
//! originally the market-data header's date field, header spec §5.2):
//! a value, a precision, an active segment and the digits typed into it
//! this visit. Pure — a host routes keys here through [`route`] and
//! paints what [`DateTimeField::segments`] answers. The value is a valid
//! [`NaiveDateTime`] at every moment: a step rolls, clamps, wraps or
//! saturates, a digit that would make an impossible segment is refused,
//! and `enter` therefore has nothing to refuse.

mod paint;

pub use paint::{SegmentPaint, paint};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

/// How many segments the field shows: a date alone (the market-data
/// attribute strip) or a date with a time to the second (the as-of
/// dialog's Custom row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precision {
    Date,
    DateTime,
}

/// One of the six segments, in painted order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl Segment {
    const ALL: [Segment; 6] = [
        Segment::Year,
        Segment::Month,
        Segment::Day,
        Segment::Hour,
        Segment::Minute,
        Segment::Second,
    ];

    /// The segment's painted position: year 0 … second 5.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// The segment's name as a notice spells it.
    pub fn name(self) -> &'static str {
        match self {
            Segment::Year => "year",
            Segment::Month => "month",
            Segment::Day => "day",
            Segment::Hour => "hour",
            Segment::Minute => "minute",
            Segment::Second => "second",
        }
    }

    /// The segment painted at `index`, `None` past the second.
    pub fn at(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// The last segment a precision shows.
    pub fn last(precision: Precision) -> Segment {
        match precision {
            Precision::Date => Segment::Day,
            Precision::DateTime => Segment::Second,
        }
    }

    /// Whether this segment is shown under `precision`.
    pub fn fits(self, precision: Precision) -> bool {
        self.index() <= Self::last(precision).index()
    }

    fn left(self) -> Self {
        Self::at(self.index().saturating_sub(1)).unwrap_or(Segment::Year)
    }

    fn right(self, precision: Precision) -> Self {
        let next = Self::at(self.index() + 1).unwrap_or(self);
        if next.fits(precision) { next } else { self }
    }

}

/// What one segment paints: its text, whether it carries the cursor, and
/// whether the text is digits mid-typing rather than the committed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentText {
    pub text: String,
    pub active: bool,
    pub typing: bool,
}

/// The field's state: the committed value, the precision, the active
/// segment, and the digits typed into that segment since it became
/// active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateTimeField {
    value: NaiveDateTime,
    precision: Precision,
    segment: Segment,
    /// Digits typed into the active segment this visit — empty once a
    /// segment completes, is left, or is backspaced. Never longer than
    /// the segment's own width, and only ever non-empty for `segment`.
    typed: String,
}

impl DateTimeField {
    /// Open on `value` with `segment` active (both hosts open on the DAY,
    /// user ruling 2026-09-19: the segment a trader changes most). A
    /// `segment` past the precision falls back to the precision's last.
    pub fn open(value: NaiveDateTime, precision: Precision, segment: Segment) -> Self {
        let segment = if segment.fits(precision) { segment } else { Segment::last(precision) };
        Self {
            value,
            precision,
            segment,
            typed: String::new(),
        }
    }

    pub fn precision(&self) -> Precision {
        self.precision
    }

    pub fn segment(&self) -> Segment {
        self.segment
    }

    /// Move to the segment on the left, clamped at the year. Leaving a
    /// segment drops its partial digits: the committed value shows again.
    pub fn left(&mut self) {
        let next = self.segment.left();
        self.select(next);
    }

    /// Move to the segment on the right, clamped at the precision's last.
    pub fn right(&mut self) {
        let next = self.segment.right(self.precision);
        self.select(next);
    }

    /// Make `segment` the active one (a click, or the arrows above). A
    /// re-select of the segment that is already active also drops its
    /// partial digits: every select is "start this segment afresh".
    /// `false`, with nothing changed, for a segment past the precision.
    pub fn select(&mut self, segment: Segment) -> bool {
        if !segment.fits(self.precision) {
            return false;
        }
        self.segment = segment;
        self.typed.clear();
        true
    }

    /// Step the active segment by `n`. Date segments as before: days roll
    /// over into the next month, months clamp the day to the new month's
    /// length, years clamp Feb 29 to Feb 28, and all three saturate at
    /// chrono's bounds rather than panicking. Time segments WRAP within
    /// their own range without carrying — `23 ↑` is `00` on the same day
    /// (the "step this segment" reading; a trader who wants the next day
    /// moves to the day). Drops partial digits.
    pub fn step(&mut self, n: i64) {
        self.typed.clear();
        let date = self.value.date();
        let time = self.value.time();
        match self.segment {
            Segment::Day | Segment::Month | Segment::Year => {
                let stepped = match self.segment {
                    Segment::Day => Duration::try_days(n).and_then(|d| date.checked_add_signed(d)),
                    Segment::Month => {
                        let months = i64::from(date.year()) * 12 + i64::from(date.month() - 1);
                        months.checked_add(n).and_then(|total| {
                            let month = total.rem_euclid(12) as u32 + 1;
                            i32::try_from(total.div_euclid(12))
                                .ok()
                                .and_then(|year| clamped_ymd(year, month, date.day()))
                        })
                    }
                    _ => {
                        let delta =
                            i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX });
                        let year = date.year().saturating_add(delta);
                        clamped_ymd(year, date.month(), date.day())
                    }
                }
                .unwrap_or(if n < 0 { NaiveDate::MIN } else { NaiveDate::MAX });
                self.value = stepped.and_time(time);
            }
            Segment::Hour | Segment::Minute | Segment::Second => {
                let modulus: i64 = if self.segment == Segment::Hour { 24 } else { 60 };
                let current = i64::from(match self.segment {
                    Segment::Hour => time.hour(),
                    Segment::Minute => time.minute(),
                    _ => time.second(),
                });
                let next = (current + n.rem_euclid(modulus)).rem_euclid(modulus) as u32;
                let (h, m, s) = match self.segment {
                    Segment::Hour => (next, time.minute(), time.second()),
                    Segment::Minute => (time.hour(), next, time.second()),
                    _ => (time.hour(), time.minute(), next),
                };
                if let Some(t) = NaiveTime::from_hms_opt(h, m, s) {
                    self.value = date.and_time(t);
                }
            }
        }
    }

    /// Type digit `d` into the active segment. A typed digit REPLACES the
    /// segment's value rather than appending to it. Answers whether the
    /// segment COMPLETED — its value applied (the day clamped if the month
    /// changed) and the next segment made active. The day is the one
    /// segment that does not advance under `Precision::Date` (the panel's
    /// contract: the day stays the day); under `DateTime` it advances to
    /// the hour like any other.
    ///
    /// - Year: four digits complete.
    /// - Month: a first digit `2`–`9` completes as `0d` at once; `0`/`1`
    ///   waits for a second digit; a second digit making `00` or more
    ///   than `12` is refused (the first digit stays).
    /// - Day: a first digit `4`–`9` completes as `0d`; `0`–`3` waits; a
    ///   second digit making `00` or more than the month holds is refused.
    /// - Hour: `3`–`9` completes as `0d`; `0`–`2` waits; a second digit
    ///   past `23` is refused.
    /// - Minute, second: `6`–`9` completes as `0d`; `0`–`5` waits; a
    ///   second digit past `59` is refused.
    ///
    /// A digit left WAITING is not lost at `enter`: the commit runs
    /// [`Self::complete_pending`] first.
    pub fn digit(&mut self, d: u8) -> bool {
        let d = d.min(9);
        let segment = self.segment;
        if segment == Segment::Year {
            self.typed.push(char::from(b'0' + d));
            if self.typed.len() < 4 {
                return false;
            }
            let year = self.typed.parse::<i32>().unwrap_or(self.value.date().year());
            let date = self.value.date();
            self.apply_date(clamped_ymd(year, date.month(), date.day()));
            self.select(Segment::Month);
            return true;
        }
        // Every two-digit segment: the smallest first digit that cannot
        // begin a larger valid value completes at once; a smaller one
        // waits; a second digit past the segment's range is refused.
        let (waits_below, max) = match segment {
            Segment::Month => (2, 12),
            Segment::Day => (4, days_in_month(self.value.date())),
            Segment::Hour => (3, 23),
            _ => (6, 59),
        };
        let zero_ok = matches!(segment, Segment::Hour | Segment::Minute | Segment::Second);
        let value = match self.typed.as_str() {
            "" if u32::from(d) >= waits_below => u32::from(d),
            "" => {
                self.typed.push(char::from(b'0' + d));
                return false;
            }
            first => {
                let candidate = first.parse::<u32>().unwrap_or(0) * 10 + u32::from(d);
                if (candidate == 0 && !zero_ok) || candidate > max {
                    return false;
                }
                candidate
            }
        };
        self.apply_segment(segment, value);
        let next = match segment {
            Segment::Day if self.precision == Precision::Date => Segment::Day,
            other => other.right(self.precision),
        };
        self.select(next);
        true
    }

    /// Finish whatever is still typed into the active segment, as a commit
    /// must before it reads [`Self::value`] (user ruling 2026-09-19): a
    /// trader who typed `1` in the day and pressed `enter` meant the 1st.
    /// A single waiting digit that can stand alone completes as `0d`;
    /// `Ok(())` too when nothing is pending. A pending entry that cannot
    /// complete — `0` in the month or day, a year of fewer than four
    /// digits — is `Err(segment)` with nothing changed, for the caller to
    /// refuse the commit and name the segment. A lone `0` in a TIME
    /// segment completes (`00` is a valid hour, minute and second).
    pub fn complete_pending(&mut self) -> Result<(), Segment> {
        if self.typed.is_empty() {
            return Ok(());
        }
        let value = self.typed.parse::<u32>().unwrap_or(0);
        let ok = match self.segment {
            Segment::Year => false,
            Segment::Month | Segment::Day => value >= 1,
            Segment::Hour | Segment::Minute | Segment::Second => true,
        };
        if !ok {
            return Err(self.segment);
        }
        let segment = self.segment;
        self.apply_segment(segment, value);
        self.typed.clear();
        Ok(())
    }

    /// Clear what was typed into the active segment; the committed value
    /// shows again.
    pub fn backspace(&mut self) {
        self.typed.clear();
    }

    /// The committed value: `YYYY-MM-DD` under `Date`, `YYYY-MM-DD
    /// HH:MM:SS` under `DateTime`. Partial digits are not part of it.
    pub fn text(&self) -> String {
        match self.precision {
            Precision::Date => self.value.format("%Y-%m-%d").to_string(),
            Precision::DateTime => self.value.format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }

    /// What each shown segment paints, year first.
    pub fn segments(&self) -> Vec<SegmentText> {
        let typing = !self.typed.is_empty();
        let v = self.value;
        Segment::ALL
            .iter()
            .copied()
            .filter(|s| s.fits(self.precision))
            .map(|segment| {
                let committed = match segment {
                    Segment::Year => format!("{:04}", v.year()),
                    Segment::Month => format!("{:02}", v.month()),
                    Segment::Day => format!("{:02}", v.day()),
                    Segment::Hour => format!("{:02}", v.hour()),
                    Segment::Minute => format!("{:02}", v.minute()),
                    Segment::Second => format!("{:02}", v.second()),
                };
                let active = segment == self.segment;
                SegmentText {
                    text: if typing && active { self.typed.clone() } else { committed },
                    active,
                    typing: typing && active,
                }
            })
            .collect()
    }

    /// The committed value. Partial digits are not in it — which is why a
    /// commit calls [`Self::complete_pending`] first and reads this only
    /// on `Ok`.
    pub fn value(&self) -> NaiveDateTime {
        self.value
    }

    /// The committed date — the `Date` precision's whole answer.
    pub fn date(&self) -> NaiveDate {
        self.value.date()
    }

    fn apply_date(&mut self, date: Option<NaiveDate>) {
        if let Some(date) = date {
            self.value = date.and_time(self.value.time());
        }
    }

    /// Install a completed two-digit segment's value, clamping the day
    /// when a new month is shorter.
    fn apply_segment(&mut self, segment: Segment, value: u32) {
        let date = self.value.date();
        let time = self.value.time();
        match segment {
            Segment::Year => self.apply_date(clamped_ymd(value as i32, date.month(), date.day())),
            Segment::Month => self.apply_date(clamped_ymd(date.year(), value, date.day())),
            Segment::Day => {
                self.apply_date(NaiveDate::from_ymd_opt(date.year(), date.month(), value))
            }
            Segment::Hour => {
                if let Some(t) = NaiveTime::from_hms_opt(value, time.minute(), time.second()) {
                    self.value = date.and_time(t);
                }
            }
            Segment::Minute => {
                if let Some(t) = NaiveTime::from_hms_opt(time.hour(), value, time.second()) {
                    self.value = date.and_time(t);
                }
            }
            Segment::Second => {
                if let Some(t) = NaiveTime::from_hms_opt(time.hour(), time.minute(), value) {
                    self.value = date.and_time(t);
                }
            }
        }
    }
}
```

Until Task 4 exists, keep `mod paint;` and the `pub use paint::…` line OUT (add them in Task 4).

Now the panel. In `crates/geode-marketdata/src/core/mod.rs`:

```rust
pub use geode_widgets::datefield::{DateTimeField, Precision, Segment, SegmentText};
```

In `crates/geode-marketdata/src/tile.rs`:
- the `use crate::core::{…, DateField, …}` list: `DateField` → `DateTimeField, Precision`.
- `EditorState::Date { field: DateField, … }` → `field: DateTimeField`.
- `DateFieldPaint::of`:

```rust
    fn of(field: &DateTimeField) -> Self {
        let segs = field.segments();
        let active = field.segment().index();
        let typing = segs.iter().any(|s| s.typing);
        let mut segments: [SharedString; 3] = Default::default();
        for (slot, seg) in segments.iter_mut().zip(segs) {
            *slot = seg.text.into();
        }
        Self {
            segments,
            active,
            typing,
        }
    }
```

- both open sites (`~2061`, `~2855`): `DateField::open(date)` → `DateTimeField::open(date.and_hms_opt(0, 0, 0).expect("midnight exists"), Precision::Date, Segment::Day)`.
- `date_field()` (test accessor): return type `Option<DateTimeField>`.
- every `field.value()` compared to or written as a `NaiveDate` in the commit path (`commit_edit`'s `EditorState::Date` arm formats the date) → `field.date()`; `field.text()` is unchanged.
- the `date_segments` test helper: `field.segment` → `field.segment()`, and destructure `let v = field.segments(); ([v[0].text.clone(), v[1].text.clone(), v[2].text.clone()], field.segment())`.

`header.rs:159` `Segment::at(i)` is unchanged.

- [ ] **Step 4: Run the suites**

Run: `cargo test -p geode-widgets && cargo test -p geode-marketdata && cargo clippy -p geode-widgets -p geode-marketdata --all-targets -- -D warnings`
Expected: all green; every panel date test passes with no assertion edited.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-widgets crates/geode-marketdata
git commit -m "widgets: DateTimeField — precision, hour/minute/second segments; panel on the Date precision"
```

---

### Task 3: One key table — `FieldKey`, `route`, `apply`

**Files:**
- Modify: `crates/geode-widgets/src/datefield/mod.rs`
- Modify: `crates/geode-marketdata/src/tile.rs:2093-2160` (`date_field_key`)

**Interfaces:**
- Produces:

```rust
pub enum FieldKey { Left, Right, Step(i64), Digit(u8), Backspace, Commit, Cancel }
/// The one key table both hosts consult. `None` for a chord (ctrl/alt/cmd
/// held) and for any key the field does not own.
pub fn route(key: &str, shift: bool, chord: bool) -> Option<FieldKey>;
impl DateTimeField {
    /// Perform every arm but `Commit`/`Cancel`, which the host owns.
    /// Answers whether the field changed.
    pub fn apply(&mut self, key: FieldKey) -> bool;
}
```

`route` takes the keystroke's parts rather than a `Keystroke` type because the shell's `keymap::Keystroke` and gpui's `KeyDownEvent` are two types this crate must not choose between; each host passes `event.keystroke.key`, `modifiers.shift`, `modifiers.control || modifiers.alt || modifiers.platform` (the panel) or `ks.key`, `ks.mods.shift`, `ks.mods.is_chord()` (the shell).

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn route_maps_every_field_key_and_lets_a_chord_through() {
        assert_eq!(route("left", false, false), Some(FieldKey::Left));
        assert_eq!(route("right", false, false), Some(FieldKey::Right));
        assert_eq!(route("up", false, false), Some(FieldKey::Step(1)));
        assert_eq!(route("down", false, false), Some(FieldKey::Step(-1)));
        assert_eq!(route("up", true, false), Some(FieldKey::Step(10)));
        assert_eq!(route("down", true, false), Some(FieldKey::Step(-10)));
        assert_eq!(route("7", false, false), Some(FieldKey::Digit(7)));
        assert_eq!(route("backspace", false, false), Some(FieldKey::Backspace));
        assert_eq!(route("enter", false, false), Some(FieldKey::Commit));
        assert_eq!(route("escape", false, false), Some(FieldKey::Cancel));
        assert_eq!(route("a", false, false), None, "a letter is not the field's");
        assert_eq!(route("tab", false, false), None, "tab belongs to the host");
        assert_eq!(route("up", false, true), None, "a chord falls through to the shell");
        assert_eq!(route("7", false, true), None);
    }

    #[test]
    fn apply_performs_the_field_arms_and_reports_a_change() {
        let mut f = DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::DateTime, Segment::Day);
        assert!(f.apply(FieldKey::Step(1)));
        assert_eq!(f.date(), d(2026, 9, 19));
        assert!(f.apply(FieldKey::Right));
        assert_eq!(f.segment(), Segment::Hour);
        assert!(f.apply(FieldKey::Digit(1)), "a waiting digit is a change too");
        assert!(f.apply(FieldKey::Backspace));
        assert!(!f.apply(FieldKey::Backspace), "nothing typed: nothing changed");
        assert!(f.apply(FieldKey::Left));
        assert!(!f.apply(FieldKey::Commit), "commit and cancel are the host's");
        assert!(!f.apply(FieldKey::Cancel));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-widgets route`
Expected: compile error, `FieldKey`/`route`/`apply` missing.

- [ ] **Step 3: Implement** (in `datefield/mod.rs`, after `SegmentText`)

```rust
/// What one keystroke means to the field — [`route`]'s answer, applied
/// by [`DateTimeField::apply`] except for the two the host owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKey {
    Left,
    Right,
    Step(i64),
    Digit(u8),
    Backspace,
    Commit,
    Cancel,
}

/// The ONE key table every host consults (spec §4.2): `left`/`right`
/// move, `up`/`down` step (`shift` = ten), a digit types, `backspace`
/// clears, `enter` commits, `escape` cancels. `chord` is "ctrl, alt or
/// cmd held" — such a keystroke is never the field's, so a shell chord
/// keeps working while a field is open.
pub fn route(key: &str, shift: bool, chord: bool) -> Option<FieldKey> {
    if chord {
        return None;
    }
    let big = if shift { 10 } else { 1 };
    Some(match key {
        "left" => FieldKey::Left,
        "right" => FieldKey::Right,
        "up" => FieldKey::Step(big),
        "down" => FieldKey::Step(-big),
        "backspace" => FieldKey::Backspace,
        "enter" => FieldKey::Commit,
        "escape" => FieldKey::Cancel,
        _ => {
            let mut chars = key.chars();
            match (chars.next().and_then(|c| c.to_digit(10)), chars.next()) {
                (Some(d), None) => FieldKey::Digit(d as u8),
                _ => return None,
            }
        }
    })
}

impl DateTimeField {
    /// Perform `key`'s arm on the field. `Commit` and `Cancel` do nothing
    /// here — the host owns what a commit writes and what a cancel
    /// restores — and answer `false`. Every other arm answers whether
    /// anything about the field (value, segment or partial digits)
    /// changed, so a host can skip a repaint on a no-op.
    pub fn apply(&mut self, key: FieldKey) -> bool {
        let before = self.clone();
        match key {
            FieldKey::Left => self.left(),
            FieldKey::Right => self.right(),
            FieldKey::Step(n) => self.step(n),
            FieldKey::Digit(d) => {
                self.digit(d);
            }
            FieldKey::Backspace => self.backspace(),
            FieldKey::Commit | FieldKey::Cancel => return false,
        }
        *self != before
    }
}
```

Then rewrite the panel's `date_field_key` body (`tile.rs`, keep its doc comment):

```rust
        let modifiers = event.keystroke.modifiers;
        let chord = modifiers.control || modifiers.alt || modifiers.platform;
        let Some(key) = geode_widgets::datefield::route(
            event.keystroke.key.as_str(),
            modifiers.shift,
            chord,
        ) else {
            return false;
        };
        let Some(Editing {
            state: EditorState::Date { field, paint, .. },
            ..
        }) = self.editor.as_mut()
        else {
            return false;
        };
        match key {
            FieldKey::Commit => {
                self.commit_edit(window, cx);
                self.sync_cursor(cx);
                self.changed(cx);
                return true;
            }
            FieldKey::Cancel => {
                self.close_editor(window, cx);
                self.sync_cursor(cx);
                self.changed(cx);
                return true;
            }
            other => {
                field.apply(other);
            }
        }
        *paint = DateFieldPaint::of(field);
        // A keystroke that changed the field retires a standing refusal
        // (`finish the day or backspace`, say) — the notice is header
        // chrome, so that one case re-prepares it.
        if self.notice.take().is_some() {
            self.changed(cx);
        } else {
            cx.notify();
        }
        true
```

with `use geode_widgets::datefield::FieldKey;` added to the tile's imports.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-widgets && cargo test -p geode-marketdata date && cargo clippy -p geode-marketdata --all-targets -- -D warnings`
Expected: green. The panel tests `arrows_step_each_segment_and_enter_commits_the_date`, `typed_digits_fill_a_segment_and_advance`, `escape_restores_the_painted_date_and_blurs_the_field`, `enter_completes_a_pending_digit_rather_than_committing_the_old_date`, `enter_on_an_incompletable_digit_is_refused_naming_the_segment` pass unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-widgets crates/geode-marketdata/src/tile.rs
git commit -m "widgets: one key table (route/FieldKey/apply); the panel's date_field_key routes through it"
```

---

### Task 4: The painter — `SegmentPaint` and `paint`

**Files:**
- Create: `crates/geode-widgets/src/datefield/paint.rs`
- Modify: `crates/geode-widgets/src/datefield/mod.rs` (`mod paint; pub use paint::{SegmentPaint, paint};`)
- Modify: `crates/geode-marketdata/src/header.rs:60-170` (`date_segment_paint` stays; `render_date_field` calls the crate)
- Test: `crates/geode-widgets/src/datefield/paint.rs` (a `TestAppContext` render test), the panel's `the_delegate_mirrors_a_date_cells_field`, `a_click_on_a_segment_selects_it`, `a_segment_click_refocuses_an_unfocused_field`, `date_segment_colours_are_readable_on_every_bundled_theme`

**Interfaces:**
- Produces:

```rust
/// Copy. Every colour the painter uses; the host derives it once.
pub struct SegmentPaint {
    pub rest_text: Hsla,
    pub rest_fill: Option<Hsla>,
    pub active_text: Hsla,
    pub active_fill: Hsla,
    pub typing_text: Hsla,
    pub typing_fill: Hsla,
    pub separator: Hsla,
    pub suffix: Hsla,
    pub radius: Pixels,
}
/// The segments in painted order with separators, an optional trailing
/// suffix (a zone abbreviation), and a mouse-down on any segment routed
/// to `on_segment`. Each segment carries the selector `"{selector}-{i}"`.
pub fn paint(
    segments: &[SegmentText],
    suffix: Option<SharedString>,
    paint: SegmentPaint,
    selector: SharedString,
    on_segment: impl Fn(Segment, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement;
```

`paint` takes the prepared `&[SegmentText]` (not the field) so a host that prepares its paint outside `render` (the panel's `DateFieldPaint`) can hand over what it already holds; the separators are decided from the segment index (`-`, `-`, ` `, `:`, `:`).

- [ ] **Step 1: Write the failing test** (`paint.rs`, `#[cfg(test)]`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext, div, hsla, px};

    struct Probe;
    impl gpui::Render for Probe {
        fn render(&mut self, _w: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let segments = vec![
                SegmentText { text: "2026".into(), active: false, typing: false },
                SegmentText { text: "09".into(), active: false, typing: false },
                SegmentText { text: "18".into(), active: true, typing: false },
                SegmentText { text: "18".into(), active: false, typing: false },
                SegmentText { text: "00".into(), active: false, typing: false },
                SegmentText { text: "00".into(), active: false, typing: false },
            ];
            let paint = SegmentPaint {
                rest_text: hsla(0., 0., 0.9, 1.),
                rest_fill: None,
                active_text: hsla(0., 0., 0.1, 1.),
                active_fill: hsla(0.1, 1., 0.5, 1.),
                typing_text: hsla(0., 0., 0.9, 1.),
                typing_fill: hsla(0.6, 0.5, 0.3, 1.),
                separator: hsla(0., 0., 0.5, 1.),
                suffix: hsla(0., 0., 0.5, 1.),
                radius: px(3.),
            };
            div().child(super::paint(
                &segments,
                Some("EDT".into()),
                paint,
                "probe-seg".into(),
                |_segment, _w, _cx| {},
            ))
        }
    }

    #[gpui::test]
    fn every_segment_and_the_suffix_are_painted_with_their_selectors(cx: &mut TestAppContext) {
        let window = cx
            .update(|cx| cx.open_window(gpui::WindowOptions::default(), |_w, cx| cx.new(|_| Probe)))
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|w, cx| {
            let _ = w.draw(cx);
        });
        for i in 0..6 {
            assert!(vcx.debug_bounds(&format!("probe-seg-{i}")).is_some(), "segment {i}");
        }
        assert!(vcx.debug_bounds("probe-seg-suffix").is_some());
        let y = vcx.debug_bounds("probe-seg-0").unwrap();
        let s = vcx.debug_bounds("probe-seg-5").unwrap();
        assert!(s.origin.x > y.origin.x, "painted left to right");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p geode-widgets paint`
Expected: compile error, no `paint` module.

- [ ] **Step 3: Implement** (`paint.rs`)

```rust
//! The date-time field's painter (spec §4.3): the segments in the data
//! face with the three states — rest, active, typing — separators
//! between, an optional suffix after. Every colour is the host's, handed
//! in as a [`SegmentPaint`]: the painter never reads `cx.theme()`, so it
//! can be called from a closure that cannot borrow it (the `key_chip`
//! precedent), and a host derives its colours once (the panel's
//! `FlooredTones`, the shell's chip/control doors) rather than per paint.

use gpui::prelude::*;
use gpui::{AnyElement, App, Hsla, MouseButton, Pixels, SharedString, Window, div};
use gpui_component::h_flex;

use super::{Segment, SegmentText};

/// Every colour the painter uses. `Copy`, derived once by the host and
/// handed in per paint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentPaint {
    pub rest_text: Hsla,
    pub rest_fill: Option<Hsla>,
    pub active_text: Hsla,
    pub active_fill: Hsla,
    pub typing_text: Hsla,
    pub typing_fill: Hsla,
    pub separator: Hsla,
    pub suffix: Hsla,
    pub radius: Pixels,
}

/// The separator painted BEFORE segment `i`: none before the year, `-`
/// inside the date, a space between date and time, `:` inside the time.
fn separator_before(i: usize) -> Option<&'static str> {
    match i {
        0 => None,
        1 | 2 => Some("-"),
        3 => Some(" "),
        _ => Some(":"),
    }
}

/// Paint `segments` (already in painted order, as
/// [`super::DateTimeField::segments`] answers them) with `paint`'s
/// colours, `suffix` after a gap when given, and a left mouse-down on
/// segment `i` calling `on_segment(Segment::at(i))` and stopping
/// propagation — the host's own container mouse-down (a click "elsewhere"
/// that cancels an editor, say) must not also fire for a click aimed
/// into a segment. Each segment carries the selector `"{selector}-{i}"`
/// and the suffix `"{selector}-suffix"`, so a window test can find them.
/// The font face is the caller's: set `font_family` on the container.
pub fn paint(
    segments: &[SegmentText],
    suffix: Option<SharedString>,
    paint: SegmentPaint,
    selector: SharedString,
    on_segment: impl Fn(Segment, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let mut row = h_flex().items_center();
    for (i, seg) in segments.iter().enumerate() {
        if let Some(sep) = separator_before(i) {
            row = row.child(div().text_color(paint.separator).child(sep));
        }
        let (text, fill) = if seg.typing {
            (paint.typing_text, Some(paint.typing_fill))
        } else if seg.active {
            (paint.active_text, Some(paint.active_fill))
        } else {
            (paint.rest_text, paint.rest_fill)
        };
        let on_segment = on_segment.clone();
        let sel = selector.clone();
        row = row.child(
            div()
                .px_0p5()
                .rounded(paint.radius)
                .text_color(text)
                .when_some(fill, |d, f| d.bg(f))
                .debug_selector(move || format!("{sel}-{i}"))
                .on_mouse_down(MouseButton::Left, move |_event, window, cx| {
                    if let Some(segment) = Segment::at(i) {
                        on_segment(segment, window, cx);
                    }
                    cx.stop_propagation();
                })
                .child(seg.text.clone()),
        );
    }
    if let Some(suffix) = suffix {
        let sel = selector.clone();
        row = row.child(
            div()
                .ml_2()
                .text_color(paint.suffix)
                .debug_selector(move || format!("{sel}-suffix"))
                .child(suffix),
        );
    }
    row.into_any_element()
}
```

Add to `datefield/mod.rs` after the doc comment: `mod paint;` and `pub use paint::{SegmentPaint, paint};`.

Then the panel's `render_date_field` (`header.rs`): keep its signature, container (`track_focus`, border, `font_family(fonts::MONO)`, selector `marketdata-date-{tile_id}`, `on_key_down`), and replace the `for (i, text) in paint.segments…` loop with:

```rust
    let rest = date_segment_paint(theme, tones, false, false);
    let active = date_segment_paint(theme, tones, true, false);
    let typing = date_segment_paint(theme, tones, true, true);
    let segment_paint = SegmentPaint {
        rest_text: rest.text,
        rest_fill: rest.fill,
        active_text: active.text,
        active_fill: active.fill.expect("active segment has a fill"),
        typing_text: typing.text,
        typing_fill: typing.fill.expect("typing segment has a fill"),
        separator,
        suffix: separator,
        radius: theme.radius_tokens().sm,
    };
    let segments: Vec<SegmentText> = paint
        .segments
        .iter()
        .enumerate()
        .map(|(i, text)| SegmentText {
            text: text.to_string(),
            active: paint.active == i,
            typing: paint.active == i && paint.typing,
        })
        .collect();
    let tile = tile.clone();
    field.child(geode_widgets::datefield::paint(
        &segments,
        None,
        segment_paint,
        format!("marketdata-date-seg-{tile_id}").into(),
        move |segment, window, cx| {
            tile.update(cx, |t, cx| t.date_segment_clicked(segment, window, cx));
        },
    ))
```

(`field` is the `h_flex` container; the function returns it as before.) Import `geode_widgets::datefield::{SegmentPaint, SegmentText}` and drop the now-unused `Segment` import if clippy says so. The existing selectors `marketdata-date-seg-{tile_id}-{i}` are preserved exactly, which is what the panel's click tests find.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-widgets && cargo test -p geode-marketdata date && cargo test -p geode-marketdata delegate_mirrors && cargo clippy --workspace --all-targets -- -D warnings`
Expected: green; `a_click_on_a_segment_selects_it` and `a_segment_click_refocuses_an_unfocused_field` pass unchanged.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-widgets crates/geode-marketdata/src/header.rs
git commit -m "widgets: SegmentPaint + paint; the panel paints its date field through the crate"
```

---

### Task 5: Harness entries, docs, full verification

**Files:**
- Modify: `scripts/mutation-check.sh` (append after the last `tilepicker:` entry), `CLAUDE.md`, `docs/phase-history.md`

- [ ] **Step 1: Harness entries**

Append:

```zsh
# As-of dialog Part 1 (2026-09-20): the shared date-time field. `right`
# past the precision's last segment must stay — a Date field that
# stepped into a hidden hour would type into a segment nobody can see.
run_mutation "widgets: right stops at the precision's last segment" \
  crates/geode-widgets/src/datefield/mod.rs \
  '        if next.fits(precision) { next } else { self }' \
  '        next' \
  geode-widgets \
  right_stops_at_the_precisions_last_segment

# An hour step wraps within the hour; carrying into the day would move
# the date under a trader stepping the time alone.
run_mutation "widgets: a time step wraps without carrying" \
  crates/geode-widgets/src/datefield/mod.rs \
  '                let next = (current + n.rem_euclid(modulus)).rem_euclid(modulus) as u32;' \
  '                let next = ((current + n).clamp(0, modulus - 1)) as u32;' \
  geode-widgets \
  a_time_step_wraps_within_its_segment_without_carrying

# `enter` is Commit, not Cancel — the host's two arms must stay distinct.
run_mutation "widgets: enter routes to Commit" \
  crates/geode-widgets/src/datefield/mod.rs \
  '        "enter" => FieldKey::Commit,' \
  '        "enter" => FieldKey::Cancel,' \
  geode-widgets \
  route_maps_every_field_key_and_lets_a_chord_through

# A chord is never the field's: with this gate gone, `ctrl+d` inside an
# open field would be swallowed as a non-key instead of reaching the
# shell.
run_mutation "widgets: a chord falls through route" \
  crates/geode-widgets/src/datefield/mod.rs \
  '    if chord {
        return None;
    }' \
  '    let _ = chord;' \
  geode-widgets \
  route_maps_every_field_key_and_lets_a_chord_through

# The panel's Date field still opens on the DAY segment through the
# generalised core (user ruling 2026-09-19).
run_mutation "marketdata: the date field opens on the day segment" \
  crates/geode-marketdata/src/tile.rs \
  '            let field = DateTimeField::open(date.and_hms_opt(0, 0, 0).expect("midnight exists"), Precision::Date, Segment::Day);' \
  '            let field = DateTimeField::open(date.and_hms_opt(0, 0, 0).expect("midnight exists"), Precision::Date, Segment::Year);' \
  geode-marketdata \
  i_on_a_date_attribute_opens_the_field_on_the_day_segment
```

If `cargo fmt` has reflowed the `DateTimeField::open(...)` line in `tile.rs` across lines, copy the FIRST line of that call verbatim as the anchor (it must match exactly once). Update the header's count comment in `CLAUDE.md` (`1200 entries` → the new count from `grep -c '^run_mutation ' scripts/mutation-check.sh`).

- [ ] **Step 2: Run the anchors check and the new entries**

Run: `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "widgets:" && zsh scripts/mutation-check.sh "marketdata: the date field opens"`
Expected: `--anchors-only` exits 0; every entry prints `CAUGHT`.

- [ ] **Step 3: Docs**

`CLAUDE.md`:
- Architecture tree: add a line under `geode-shell`:
  `  ├─ geode-widgets shared widgets: a pure core + colour-parameterised painter each (the date-time field); below the shell, depended on by shell and modules`
- Status table: add a row `| As-of dialog Part 1 (2026-09-20) | `geode-widgets`: `DateTimeField` (`Precision::{Date, DateTime}`, six segments, `route`/`FieldKey`/`apply`, `SegmentPaint` + `paint`); the market-data date field migrated, no visible change. Parts 2 (clock) and 3 (dialog) next. | `2026-09-20-…as-of-dialog` §4 |`
- Load-bearing rules, a new bullet under "Market-data panel": `- The segmented date field is `geode_widgets::datefield` (2026-09-20): `route` is the ONE key table (a chord answers `None` and falls through to the shell), `DateTimeField::apply` performs every arm but `Commit`/`Cancel` (the host's), a time segment's step WRAPS without carrying, and `right` stops at the precision's last segment. The painter takes every colour as a `SegmentPaint` and never reads the theme; the panel's `render_date_field` keeps the container, focus handle and key listener and hands the crate its `FlooredTones`-derived colours.`

`docs/phase-history.md`: append a paragraph "As-of dialog Part 1 (2026-09-20)" stating what moved, the generalisation, the harness entries, and that the panel's tests were the migration check.

- [ ] **Step 4: Full verification**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run && cargo check -p geode-shell --features test-support --all-targets`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add scripts/mutation-check.sh CLAUDE.md docs/phase-history.md
git commit -m "widgets: harness entries and docs for the shared date-time field"
```
