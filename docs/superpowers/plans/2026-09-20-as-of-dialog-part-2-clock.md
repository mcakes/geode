# As-of dialog Part 2: `[time]`, `Clock`, `AppClock` and one clock everywhere — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every displayed time in Geode reads on one configured clock (`[time] zone`, the machine's zone by default), with `sod`/`eod` and the business-day presets defined once in `geode_core::clock`, and `chrono::Local` gone from the workspace.

**Architecture:** `geode_core::clock::Clock` (pure, `Copy`) owns the zone and the two day-boundary times, resolves local instants, walks business days and formats the three displayed forms. `geode_shell::clock::AppClock(Clock)` is the third gpui global (beside `UiSettings` and `Chords`): the shell writes it at startup and on reload, modules read it and `observe_global` it. Pure code that formats or resolves a time takes `&Clock`. A textual guard test keeps `chrono::Local` out of every crate.

**Tech Stack:** chrono 0.4, chrono-tz 0.10, iana-time-zone 0.1 (already in the lockfile via chrono), gpui globals, the mutation harness.

**Spec:** `docs/superpowers/specs/2026-09-20-geode-as-of-dialog-design.md` §3 and §6 (rulings 1 and 2 in §2).

## Global Constraints

- `chrono-tz` and `iana-time-zone` are dependencies of `geode-core` alone; no other crate names a `Tz`.
- `chrono::Local` leaves the workspace entirely (§6.3); the guard test in Task 8 enforces it.
- Storage timestamps, the daily log/crash file names (UTC date), `DataService`, the query compiler and the series family are untouched (§6.4).
- A bad `[time]` key is an **error** diagnostic at `time.<key>` with the default applied; the machine zone unreadable is a **warning** at `time.zone` with UTC applied (§3.1).
- A `[time]` change on reload repaints; nothing requeries, no version counter bumps.
- Only the shell writes `AppClock` (startup in `ShellView::new`, a changed clock in `apply_reload`); modules only read it.
- Existing tests that compute an expected value with `Local` are rewritten against an explicit zone (`Clock::utc()` or `Clock::in_zone(chrono_tz::America::New_York)`), never against the machine.
- CI: fmt, clippy `-D warnings`, `cargo test --workspace`, `cargo bench --workspace --no-run`, `cargo check -p geode-shell --features test-support --all-targets`, both platforms.
- Every load-bearing branch gets a `run_mutation` entry naming its test; `--anchors-only` exits 0 before merge. Commit before mutating.

---

## File structure

| File | Responsibility |
|---|---|
| `crates/geode-core/Cargo.toml` | add `chrono-tz = "0.10"`, `iana-time-zone = "0.1"` |
| `crates/geode-core/src/clock.rs` (new) | `Clock`, `ClockError`, `Preset`, `presets`, `business_days_back`, `from_config`, the guard test |
| `crates/geode-core/src/lib.rs` | `pub mod clock;` |
| `crates/geode-core/src/query.rs` | `parse_as_of(text, now, &Clock)`; `resolve_local` moves to `clock.rs` |
| `crates/geode-shell/src/clock.rs` (new) | `AppClock` global |
| `crates/geode-shell/src/lib.rs` | `pub mod clock;` |
| `crates/geode-shell/src/shell/mod.rs` | set the global at startup, seed `today` from it, fold `[time]` diagnostics, the poll tick |
| `crates/geode-shell/src/shell/hot_reload.rs` | re-derive the clock on reload, set the global when changed, fold diagnostics |
| `crates/geode-shell/src/frame.rs`, `scopebar.rs`, `shell/render.rs` | `bar_model`/`build_model` take the clock; the as-of chip formats through it |
| `crates/geode-shell/src/shell/asof_view.rs` | every `Local` → the clock (interim; Part 3 rewrites the file) |
| `crates/geode-blotter/src/tile.rs` | `:asof` parses with the clock; freshness and `AS OF` readouts format through it |
| `crates/geode-marketdata/src/{tile.rs, header.rs, core/draft.rs, core/menu.rs}` | header time, `local_hhmm`, the date-field seed |
| `crates/geode-diagnostics/src/{tile.rs, sections.rs}` | section formatters take `&Clock` |
| `crates/geode-app/src/main.rs` | the demo generator's `today` from the config's clock |
| `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md` | entries, rules, history |

---

### Task 1: `geode_core::clock::Clock` — the pure core

**Files:**
- Create: `crates/geode-core/src/clock.rs`
- Modify: `crates/geode-core/Cargo.toml`, `crates/geode-core/src/lib.rs`

**Interfaces:**
- Produces:

```rust
pub struct Clock { /* zone: Tz, sod: NaiveTime, eod: NaiveTime */ }   // Copy, PartialEq, Debug
pub enum ClockError { NoSuchLocalTime(String) }                        // Display: "'<text>' does not name a valid local time (a DST gap or overlap)"
impl Clock {
    pub const DEFAULT_SOD: NaiveTime; // 08:00
    pub const DEFAULT_EOD: NaiveTime; // 18:00
    pub fn utc() -> Clock;                        // UTC, default times — tests and the fallback
    pub fn in_zone(zone: Tz) -> Clock;            // default times
    pub fn with_times(self, sod: NaiveTime, eod: NaiveTime) -> Clock;
    pub fn machine() -> (Clock, Option<String>);  // iana_time_zone; Some(warning) when it fell back to UTC
    pub fn zone(&self) -> Tz;
    pub fn zone_name(&self) -> &'static str;
    pub fn today(&self, now: DateTime<Utc>) -> NaiveDate;
    pub fn local(&self, t: DateTime<Utc>) -> DateTime<Tz>;
    pub fn resolve_local(&self, date: NaiveDate, time: NaiveTime) -> Result<DateTime<Utc>, ClockError>;
    pub fn sod_of(&self, date: NaiveDate) -> Result<DateTime<Utc>, ClockError>;
    pub fn eod_of(&self, date: NaiveDate) -> Result<DateTime<Utc>, ClockError>;
    pub fn hm(&self, t: DateTime<Utc>) -> String;   // "HH:MM"
    pub fn hms(&self, t: DateTime<Utc>) -> String;  // "HH:MM:SS"
    pub fn full(&self, t: DateTime<Utc>) -> String; // "YYYY-MM-DD HH:MM:SS EDT"
    pub fn abbreviation(&self, t: DateTime<Utc>) -> String; // "EDT"
}
pub fn business_days_back(date: NaiveDate, n: u32) -> NaiveDate;
```

- [ ] **Step 1: Dependencies and module**

`crates/geode-core/Cargo.toml` `[dependencies]`, after `chrono`:

```toml
# The IANA zone database, for `[time] zone` (as-of dialog spec §3): the
# one crate in the workspace that names a `Tz`; everything else reaches
# a zone through `clock::Clock`.
chrono-tz = "0.10"
# The machine's own zone NAME (chrono uses this crate internally for
# `Local`, which `clock::Clock::machine` replaces — `Local` is banned
# workspace-wide by `clock::tests::no_crate_uses_chrono_local`).
iana-time-zone = "0.1"
```

`crates/geode-core/src/lib.rs`: add `pub mod clock;` after `pub mod attribution;` (alphabetical).

- [ ] **Step 2: Write the failing tests** (`clock.rs`, bottom)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use chrono_tz::America::New_York;
    use chrono_tz::Europe::London;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn hm(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn today_and_the_local_view_follow_the_zone_not_utc() {
        // 01:30 UTC on the 21st is still the 20th in New York.
        let now = Utc.with_ymd_and_hms(2026, 9, 21, 1, 30, 0).unwrap();
        assert_eq!(Clock::in_zone(New_York).today(now), d(2026, 9, 20));
        assert_eq!(Clock::utc().today(now), d(2026, 9, 21));
        assert_eq!(Clock::in_zone(New_York).hm(now), "21:30");
        assert_eq!(Clock::in_zone(New_York).hms(now), "21:30:00");
        assert_eq!(Clock::in_zone(New_York).full(now), "2026-09-20 21:30:00 EDT");
        assert_eq!(Clock::in_zone(New_York).abbreviation(now), "EDT");
        assert_eq!(Clock::in_zone(London).abbreviation(now), "BST");
    }

    #[test]
    fn sod_and_eod_resolve_in_the_zone_either_side_of_a_dst_change() {
        let c = Clock::in_zone(New_York);
        // 2026-11-01 is the US fall-back day: EDT before 02:00, EST after.
        assert_eq!(
            c.sod_of(d(2026, 10, 31)).unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0).unwrap(),
            "08:00 EDT is 12:00 UTC"
        );
        assert_eq!(
            c.eod_of(d(2026, 11, 2)).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 2, 23, 0, 0).unwrap(),
            "18:00 EST is 23:00 UTC"
        );
        let custom = c.with_times(hm(7, 30), hm(17, 0));
        assert_eq!(
            custom.eod_of(d(2026, 9, 18)).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 18, 21, 0, 0).unwrap()
        );
    }

    #[test]
    fn resolve_local_refuses_the_spring_forward_gap_and_the_fall_back_overlap() {
        let c = Clock::in_zone(New_York);
        // 2026-03-08 02:30 does not exist in New York.
        let err = c.resolve_local(d(2026, 3, 8), hm(2, 30)).unwrap_err();
        assert!(err.to_string().contains("does not name a valid local time"), "{err}");
        // 2026-11-01 01:30 exists twice.
        assert!(c.resolve_local(d(2026, 11, 1), hm(1, 30)).is_err());
        assert!(c.resolve_local(d(2026, 11, 1), hm(3, 30)).is_ok());
    }

    #[test]
    fn business_days_back_skips_weekends_and_snaps_a_weekend_start_to_friday() {
        let mon = d(2026, 9, 21);
        assert_eq!(business_days_back(mon, 0), mon);
        assert_eq!(business_days_back(mon, 1), d(2026, 9, 18), "Monday's T-1 is Friday");
        assert_eq!(business_days_back(mon, 2), d(2026, 9, 17));
        assert_eq!(business_days_back(mon, 5), d(2026, 9, 14), "T-5 is the previous Monday");
        let sat = d(2026, 9, 19);
        assert_eq!(business_days_back(sat, 0), d(2026, 9, 18), "a Saturday snaps to Friday");
        assert_eq!(business_days_back(sat, 1), d(2026, 9, 17), "…and then walks");
        let sun = d(2026, 9, 20);
        assert_eq!(business_days_back(sun, 1), d(2026, 9, 17));
    }

    #[test]
    fn a_machine_clock_never_panics_and_utc_is_the_fallback_shape() {
        let (clock, _warning) = Clock::machine();
        let _ = clock.today(Utc::now());
        assert_eq!(Clock::utc().zone_name(), "UTC");
        assert_eq!(Clock::utc().sod, Clock::DEFAULT_SOD);
        assert_eq!(Clock::utc().eod, Clock::DEFAULT_EOD);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p geode-core clock`
Expected: compile error, module empty/missing items.

- [ ] **Step 4: Implement**

```rust
//! The trader's clock (as-of dialog spec 2026-09-20 §3): ONE owner of
//! "which zone is this?" for every displayed time in the app. `[time]
//! zone` names it (an IANA name; the machine's zone by default), and
//! `sod`/`eod` are the two day boundaries the as-of presets resolve to.
//! Pure and `Copy`; the shell publishes it as the `AppClock` gpui global
//! and every module reads it there.
//!
//! Storage, the query compiler and the log/crash file names are UTC and
//! never see this — the data layer is zone-free by design.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

use crate::config::{Config, Diagnostic, Severity};

/// A local time that does not exist or is ambiguous (a DST gap or overlap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockError {
    NoSuchLocalTime(String),
}

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClockError::NoSuchLocalTime(text) => write!(
                f,
                "'{text}' does not name a valid local time (a DST gap or overlap)"
            ),
        }
    }
}

impl std::error::Error for ClockError {}

/// The zone plus the two day boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    zone: Tz,
    pub sod: NaiveTime,
    pub eod: NaiveTime,
}

impl Clock {
    /// `[time] sod`'s default: 08:00.
    pub const DEFAULT_SOD: NaiveTime = match NaiveTime::from_hms_opt(8, 0, 0) {
        Some(t) => t,
        None => unreachable!(),
    };
    /// `[time] eod`'s default: 18:00.
    pub const DEFAULT_EOD: NaiveTime = match NaiveTime::from_hms_opt(18, 0, 0) {
        Some(t) => t,
        None => unreachable!(),
    };

    /// UTC with the default times — tests, and the fallback when the
    /// machine's zone cannot be read.
    pub fn utc() -> Clock {
        Clock::in_zone(Tz::UTC)
    }

    /// `zone` with the default times.
    pub fn in_zone(zone: Tz) -> Clock {
        Clock {
            zone,
            sod: Clock::DEFAULT_SOD,
            eod: Clock::DEFAULT_EOD,
        }
    }

    pub fn with_times(mut self, sod: NaiveTime, eod: NaiveTime) -> Clock {
        self.sod = sod;
        self.eod = eod;
        self
    }

    /// The machine's own zone, read once (the caller keeps the answer).
    /// `Some(warning)` when it could not be read or named a zone the
    /// database does not know, in which case the clock is UTC.
    pub fn machine() -> (Clock, Option<String>) {
        match iana_time_zone::get_timezone() {
            Ok(name) => match Tz::from_str(&name) {
                Ok(zone) => (Clock::in_zone(zone), None),
                Err(_) => (
                    Clock::utc(),
                    Some(format!(
                        "time.zone: the machine's zone '{name}' is not in the IANA database — using UTC; set [time] zone"
                    )),
                ),
            },
            Err(e) => (
                Clock::utc(),
                Some(format!(
                    "time.zone: could not read the machine's zone ({e}) — using UTC; set [time] zone"
                )),
            ),
        }
    }

    pub fn zone(&self) -> Tz {
        self.zone
    }

    pub fn zone_name(&self) -> &'static str {
        self.zone.name()
    }

    /// `now` on this clock's date.
    pub fn today(&self, now: DateTime<Utc>) -> NaiveDate {
        now.with_timezone(&self.zone).date_naive()
    }

    pub fn local(&self, t: DateTime<Utc>) -> DateTime<Tz> {
        t.with_timezone(&self.zone)
    }

    /// `date` at `time` in this zone, mapped to UTC. `text` in the error
    /// is `"YYYY-MM-DD HH:MM:SS"`.
    pub fn resolve_local(
        &self,
        date: NaiveDate,
        time: NaiveTime,
    ) -> Result<DateTime<Utc>, ClockError> {
        let naive = date.and_time(time);
        self.zone
            .from_local_datetime(&naive)
            .single()
            .map(|local| local.to_utc())
            .ok_or_else(|| ClockError::NoSuchLocalTime(naive.format("%Y-%m-%d %H:%M:%S").to_string()))
    }

    pub fn sod_of(&self, date: NaiveDate) -> Result<DateTime<Utc>, ClockError> {
        self.resolve_local(date, self.sod)
    }

    pub fn eod_of(&self, date: NaiveDate) -> Result<DateTime<Utc>, ClockError> {
        self.resolve_local(date, self.eod)
    }

    pub fn hm(&self, t: DateTime<Utc>) -> String {
        self.local(t).format("%H:%M").to_string()
    }

    pub fn hms(&self, t: DateTime<Utc>) -> String {
        self.local(t).format("%H:%M:%S").to_string()
    }

    /// `YYYY-MM-DD HH:MM:SS ZONE` — the as-of preview's and tooltip's form.
    pub fn full(&self, t: DateTime<Utc>) -> String {
        self.local(t).format("%Y-%m-%d %H:%M:%S %Z").to_string()
    }

    /// The zone's abbreviation at `t` (`EDT`, `BST`, `UTC`).
    pub fn abbreviation(&self, t: DateTime<Utc>) -> String {
        self.local(t).format("%Z").to_string()
    }
}

/// Walk `n` business days back from `date`, weekends skipped (spec §2
/// ruling 1: no holiday calendar). A `date` that is itself a Saturday or
/// Sunday first snaps to the Friday before it, so `T-0` on a weekend is
/// the last business day and `T-1` the one before that.
pub fn business_days_back(date: NaiveDate, n: u32) -> NaiveDate {
    let mut day = date;
    while matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
        day = day.pred_opt().unwrap_or(day);
    }
    let mut left = n;
    while left > 0 {
        day = day.pred_opt().unwrap_or(day);
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            left -= 1;
        }
    }
    day
}
```

(`from_config`, `Preset`/`presets` and the guard test come in Tasks 2, 3 and 8; leave `Config`, `Diagnostic`, `Severity` imports out until Task 2 so clippy stays clean.)

- [ ] **Step 5: Run**

Run: `cargo test -p geode-core clock && cargo clippy -p geode-core --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/geode-core/Cargo.toml crates/geode-core/src/lib.rs crates/geode-core/src/clock.rs Cargo.lock
git commit -m "core: clock — Clock (zone, sod, eod), resolve_local, business_days_back"
```

---

### Task 2: `Clock::from_config` and the `[time]` diagnostics

**Files:**
- Modify: `crates/geode-core/src/clock.rs`

**Interfaces:**
- Produces: `pub fn from_config(config: &Config) -> (Clock, Vec<Diagnostic>)` on `Clock` — reads doc `app`, keys `time.zone`, `time.sod`, `time.eod`; a missing key is its default; a bad value is an error diagnostic at path `time.<key>` and the default.

- [ ] **Step 1: Write the failing tests**

```rust
    use crate::config::test_support::config_from;

    #[test]
    fn from_config_reads_the_three_keys_and_defaults_the_absent_ones() {
        let cfg = config_from("app", "[time]\nzone = \"Europe/London\"\neod = \"17:30\"\n");
        let (clock, diags) = Clock::from_config(&cfg);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(clock.zone_name(), "Europe/London");
        assert_eq!(clock.sod, Clock::DEFAULT_SOD);
        assert_eq!(clock.eod, hm(17, 30));
    }

    #[test]
    fn a_bad_zone_or_time_is_an_error_at_its_key_and_the_default_applies() {
        let cfg = config_from("app", "[time]\nzone = \"Mars/Olympus\"\nsod = \"eight\"\n");
        let (clock, diags) = Clock::from_config(&cfg);
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert_eq!(diags[0].path.as_deref(), Some("time.zone"));
        assert_eq!(diags[1].path.as_deref(), Some("time.sod"));
        assert_eq!(clock.zone_name(), Clock::machine().0.zone_name(), "the machine's zone");
        assert_eq!(clock.sod, Clock::DEFAULT_SOD);
    }

    #[test]
    fn an_absent_section_is_the_machine_clock() {
        let cfg = config_from("app", "config_version = 1\n");
        let (clock, diags) = Clock::from_config(&cfg);
        let (machine, warning) = Clock::machine();
        assert_eq!(clock, machine);
        assert_eq!(diags.len(), usize::from(warning.is_some()));
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p geode-core clock::tests::from_config`
Expected: compile error, no `from_config`.

- [ ] **Step 3: Implement** (inside `impl Clock`)

```rust
    /// Resolve the clock from the layered config: doc `app`, keys
    /// `time.zone` (IANA name; absent = the machine's zone), `time.sod`
    /// and `time.eod` (`HH:MM`; absent = 08:00 / 18:00). A value that
    /// does not parse is an ERROR diagnostic at `time.<key>` and that
    /// key's default — the same shape every other refused config key
    /// takes. The machine zone being unreadable is a WARNING at
    /// `time.zone` and UTC.
    pub fn from_config(config: &Config) -> (Clock, Vec<Diagnostic>) {
        let mut diags = Vec::new();
        let diag = |severity: Severity, key: &str, message: String| Diagnostic {
            severity,
            layer: config.explain("app", &format!("time.{key}")),
            file: None,
            message: format!("app: time.{key}: {message}"),
            path: Some(format!("time.{key}")),
        };
        let zone_value = config.get("app", "time.zone").and_then(|v| v.as_str());
        let mut clock = match zone_value {
            Some(name) => match Tz::from_str(name) {
                Ok(zone) => Clock::in_zone(zone),
                Err(_) => {
                    diags.push(diag(
                        Severity::Error,
                        "zone",
                        format!("'{name}' is not an IANA zone name (e.g. \"America/New_York\") — using the machine's zone"),
                    ));
                    Clock::machine().0
                }
            },
            None => {
                let (machine, warning) = Clock::machine();
                if let Some(w) = warning {
                    diags.push(diag(Severity::Warning, "zone", w));
                }
                machine
            }
        };
        for (key, slot, default) in [
            ("sod", &mut clock.sod, Clock::DEFAULT_SOD),
            ("eod", &mut clock.eod, Clock::DEFAULT_EOD),
        ] {
            *slot = default;
            if let Some(v) = config.get("app", &format!("time.{key}")) {
                match v.as_str().and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok()) {
                    Some(t) => *slot = t,
                    None => diags.push(diag(
                        Severity::Error,
                        key,
                        format!("expected \"HH:MM\", got {v} — using {}", default.format("%H:%M")),
                    )),
                }
            }
        }
        (clock, diags)
    }
```

(The `for` over `&mut` fields needs the tuple to borrow two distinct fields; if the borrow checker objects, unroll it into two calls of a small `fn read_time(config, key, default, diags) -> NaiveTime` helper.) Add `use crate::config::{Config, Diagnostic, Severity};` to the module's imports.

- [ ] **Step 4: Run**

Run: `cargo test -p geode-core clock && cargo clippy -p geode-core --all-targets -- -D warnings`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/clock.rs
git commit -m "core: clock — [time] zone/sod/eod from config with error diagnostics"
```

---

### Task 3: The presets

**Files:**
- Modify: `crates/geode-core/src/clock.rs`

**Interfaces:**
- Produces:

```rust
pub struct Preset { pub label: &'static str, pub at: DateTime<Utc> }
/// EOD T-1, SOD T, EOD T-2, EOD T-3, EOD T-5 in that order; a preset after
/// `now` or on an unresolvable local time is dropped.
pub fn presets(clock: &Clock, now: DateTime<Utc>) -> Vec<Preset>;
```

- [ ] **Step 1: Write the failing tests**

```rust
    #[test]
    fn presets_resolve_on_business_days_in_order_and_drop_a_future_one() {
        let c = Clock::in_zone(New_York);
        // Monday 21 Sep 2026, 10:42 New York (14:42 UTC).
        let now = Utc.with_ymd_and_hms(2026, 9, 21, 14, 42, 0).unwrap();
        let p = presets(&c, now);
        let labels: Vec<&str> = p.iter().map(|p| p.label).collect();
        assert_eq!(labels, ["EOD T-1", "SOD T", "EOD T-2", "EOD T-3", "EOD T-5"]);
        assert_eq!(p[0].at, c.eod_of(d(2026, 9, 18)).unwrap(), "Friday's close");
        assert_eq!(p[1].at, c.sod_of(d(2026, 9, 21)).unwrap());
        assert_eq!(p[4].at, c.eod_of(d(2026, 9, 14)).unwrap());

        // 07:00 New York: SOD T (08:00) is in the future and is dropped.
        let early = Utc.with_ymd_and_hms(2026, 9, 21, 11, 0, 0).unwrap();
        let labels: Vec<&str> = presets(&c, early).iter().map(|p| p.label).collect();
        assert_eq!(labels, ["EOD T-1", "EOD T-2", "EOD T-3", "EOD T-5"]);
    }

    #[test]
    fn a_preset_landing_in_a_dst_gap_is_dropped_not_panicked() {
        // 02:30 does not exist on 2026-03-08 in New York; an SOD there is dropped.
        let c = Clock::in_zone(New_York).with_times(hm(2, 30), hm(18, 0));
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 20, 0, 0).unwrap(); // Sunday 16:00 EDT
        let labels: Vec<&str> = presets(&c, now).iter().map(|p| p.label).collect();
        assert!(!labels.contains(&"SOD T"), "{labels:?}");
        assert!(labels.contains(&"EOD T-1"));
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-core presets` → compile error.

- [ ] **Step 3: Implement**

```rust
/// One named preset the as-of dialog offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub label: &'static str,
    pub at: DateTime<Utc>,
}

/// The fixed preset list (spec §3.3), in display order. `T` is `now` on
/// the clock's date; `T-n` walks business days. A preset that resolves
/// AFTER `now` is dropped — it would mean live, and the dialog's `Live`
/// row already says that — and one whose local time does not exist on
/// that day (a DST gap on `sod`/`eod`) is dropped with a debug line,
/// since it depends on the date and cannot be a config diagnostic.
pub fn presets(clock: &Clock, now: DateTime<Utc>) -> Vec<Preset> {
    let today = clock.today(now);
    let table: [(&'static str, Result<DateTime<Utc>, ClockError>); 5] = [
        ("EOD T-1", clock.eod_of(business_days_back(today, 1))),
        ("SOD T", clock.sod_of(business_days_back(today, 0))),
        ("EOD T-2", clock.eod_of(business_days_back(today, 2))),
        ("EOD T-3", clock.eod_of(business_days_back(today, 3))),
        ("EOD T-5", clock.eod_of(business_days_back(today, 5))),
    ];
    table
        .into_iter()
        .filter_map(|(label, at)| match at {
            Ok(at) if at <= now => Some(Preset { label, at }),
            Ok(_) => None,
            Err(e) => {
                tracing::debug!(target: "geode::shell", "preset {label} dropped: {e}");
                None
            }
        })
        .collect()
}
```

Note `SOD T` uses `business_days_back(today, 0)` so a weekend `now` offers Friday's open, consistent with the `T-n` rule.

- [ ] **Step 4: Run** — `cargo test -p geode-core clock` → green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/clock.rs
git commit -m "core: clock — the five business-day presets, future and DST-gap ones dropped"
```

---

### Task 4: `parse_as_of` takes the clock

**Files:**
- Modify: `crates/geode-core/src/query.rs:230-280` and its tests (`expect_local`, `expect_local_on`, `as_of_resolves_on_the_local_date_not_utcs`, and every other `parse_as_of` test)
- Modify callers: `crates/geode-shell/src/shell/asof_view.rs:146`, `crates/geode-blotter/src/tile.rs:1052` (temporarily `&Clock::utc()` for the blotter until Task 7 gives it the global — no: do Task 7's blotter read in this task's blotter edit, see Step 3)

**Interfaces:**
- Produces: `pub fn parse_as_of(text: &str, now: DateTime<Utc>, clock: &Clock) -> Result<DateTime<Utc>, String>`; `query::resolve_local` deleted (callers use `Clock::resolve_local`, mapping `ClockError` to `String` with `.map_err(|e| e.to_string())`).

- [ ] **Step 1: Rewrite the tests against an explicit zone**

Replace `expect_local`/`expect_local_on` with:

```rust
    fn ny() -> crate::clock::Clock {
        crate::clock::Clock::in_zone(chrono_tz::America::New_York)
    }

    /// The instant `parse_as_of` should produce for `date` + `time` in
    /// New York, computed independently of the parser.
    fn expect_ny(date: chrono::NaiveDate, time: NaiveTime) -> DateTime<Utc> {
        chrono_tz::America::New_York
            .from_local_datetime(&date.and_time(time))
            .single()
            .expect("test picks a time that exists")
            .to_utc()
    }
```

and every `parse_as_of("…", now)` becomes `parse_as_of("…", now, &ny())` with expectations through `expect_ny(ny().today(now), t)` for the `HH:MM` forms. `as_of_resolves_on_the_local_date_not_utcs` becomes:

```rust
    #[test]
    fn as_of_resolves_on_the_clocks_date_not_utcs() {
        use chrono::TimeZone;
        // 01:00 UTC on the 4th is 21:00 on the 3rd in New York: "14:05"
        // must mean the 3rd, not the 4th.
        let now = Utc.with_ymd_and_hms(2026, 9, 4, 1, 0, 0).unwrap();
        let t = NaiveTime::from_hms_opt(14, 5, 0).unwrap();
        assert_eq!(
            parse_as_of("14:05", now, &ny()),
            Ok(expect_ny(chrono::NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(), t))
        );
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-core query` → compile error (arity).

- [ ] **Step 3: Implement**

```rust
/// `HH:MM` or `HH:MM:SS` means today at that time on the trader's
/// CONFIGURED clock (`clock`; one clock throughout, as-of dialog spec
/// §6); `YYYY-MM-DD` means the end of that day ([`END_OF_DAY`]);
/// `YYYY-MM-DD HH:MM` or `YYYY-MM-DD HH:MM:SS` means that local instant;
/// anything else must be RFC 3339. `now` stays UTC; the date-carrying
/// forms never consult it. A local time that does not exist or is
/// ambiguous (a DST gap or overlap) is an `Err` naming the time.
pub fn parse_as_of(
    text: &str,
    now: DateTime<Utc>,
    clock: &crate::clock::Clock,
) -> Result<DateTime<Utc>, String> {
    let today = clock.today(now);
    let resolve = |date: chrono::NaiveDate, time: NaiveTime| {
        clock.resolve_local(date, time).map_err(|e| e.to_string())
    };
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M") {
        return resolve(today, t);
    }
    if let Ok(t) = NaiveTime::parse_from_str(text, "%H:%M:%S") {
        return resolve(today, t);
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return resolve(d, END_OF_DAY);
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M") {
        return resolve(dt.date(), dt.time());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return resolve(dt.date(), dt.time());
    }
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("'{text}' is not HH:MM, YYYY-MM-DD[ HH:MM[:SS]] or an RFC 3339 time"))
}
```

Delete `resolve_local` and the `Local` import from `query.rs`. Callers for now: `asof_view.rs:146` → `parse_as_of(trimmed, now, &geode_core::clock::Clock::utc())` with a `// Part 2 interim: Task 6 threads the real clock` comment; `geode-blotter/src/tile.rs:1052` → `parse_as_of(&text, chrono::Utc::now(), &geode_core::clock::Clock::utc())` with the same comment (Task 7 replaces both).

- [ ] **Step 4: Run** — `cargo test -p geode-core && cargo check --workspace --all-targets` → green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-core/src/query.rs crates/geode-shell/src/shell/asof_view.rs crates/geode-blotter/src/tile.rs
git commit -m "core: parse_as_of resolves on the configured clock, not chrono::Local"
```

---

### Task 5: `AppClock` — the shell's global, startup and reload

**Files:**
- Create: `crates/geode-shell/src/clock.rs`
- Modify: `crates/geode-shell/src/lib.rs`, `crates/geode-shell/src/shell/mod.rs` (fields ~1006 `today`, ~1447 the globals, ~1527 the startup diagnostics, ~1341 the poll tick, ~1651 `today:` init), `crates/geode-shell/src/shell/hot_reload.rs` (~175–200 diagnostic folding, ~390–400 the re-derives)
- Test: `crates/geode-shell/src/shell/tests/reload.rs`

**Interfaces:**
- Produces: `geode_shell::clock::AppClock(pub Clock)` implementing `gpui::Global`; `ShellView::clock(&self, cx: &App) -> Clock` (a read of the global) for the shell's own painters.

- [ ] **Step 1: Write the failing window test** (`tests/reload.rs`)

```rust
/// `[time] zone` is live (as-of dialog spec §6.1): a reload with a new
/// zone re-publishes `AppClock`, and nothing requeries — the frame's
/// data and as-of versions are untouched.
#[gpui::test]
fn a_time_zone_reload_republishes_the_clock_without_a_requery(cx: &mut gpui::TestAppContext) {
    use geode_core::clock::Clock;
    let dir = tempfile::tempdir().unwrap();
    let (window, mut vcx) = open_shell_with_user_dir(cx, test_services(), dir.path());
    let shell = shell_of(&window, &mut vcx);
    let before = vcx.update(|_w, cx| cx.global::<crate::clock::AppClock>().0);
    let versions_before = shell.read_with(&vcx, |s, cx| s.frame().read(cx).versions());

    std::fs::write(
        dir.path().join("app.toml"),
        "config_version = 1\n[time]\nzone = \"Asia/Tokyo\"\n",
    )
    .unwrap();
    let builtin = shell.read_with(&vcx, |shell, _| shell.services.builtin.clone());
    let new_config = reload::load_config(builtin, None, Some(dir.path().to_path_buf()));
    shell.update(&mut vcx, |shell, cx| shell.apply_reload(new_config, cx));

    let after = vcx.update(|_w, cx| cx.global::<crate::clock::AppClock>().0);
    assert_ne!(before, after);
    assert_eq!(after.zone_name(), "Asia/Tokyo");
    assert_eq!(after, Clock::in_zone(chrono_tz_tokyo()));
    let versions_after = shell.read_with(&vcx, |s, cx| s.frame().read(cx).versions());
    assert_eq!(versions_before.data, versions_after.data);
    assert_eq!(versions_before.as_of, versions_after.as_of);
}
```

`geode-shell` does not depend on `chrono-tz`; write the last assertion as `assert_eq!(after.zone_name(), "Asia/Tokyo")` only and delete the `chrono_tz_tokyo()` line — the zone name is the whole check.

- [ ] **Step 2: Run to verify it fails** — `cargo test -p geode-shell a_time_zone_reload` → compile error, no `crate::clock`.

- [ ] **Step 3: Implement**

`crates/geode-shell/src/clock.rs`:

```rust
//! The app-wide clock as a gpui global (as-of dialog spec 2026-09-20
//! §6.1) — the workspace's THIRD global beside `linenumbers::UiSettings`
//! and `tips::Chords`, under the same rule: written by the shell alone
//! (startup in `ShellView::new`, a changed `[time]` in `apply_reload`),
//! read by modules with `cx.global::<AppClock>()` and followed with
//! `cx.observe_global::<AppClock>`. A module has no path to `ShellView`,
//! and `ConfigReloaded` fires only for the docs a tile runs on, so
//! neither existing route could carry a zone change to a live tile.

use geode_core::clock::Clock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppClock(pub Clock);

impl gpui::Global for AppClock {}
```

`lib.rs`: `pub mod clock;` (alphabetical, after `choice`).

`shell/mod.rs`, in `ShellView::new` where `UiSettings` is set (~1447):

```rust
        let (clock, clock_diags) = geode_core::clock::Clock::from_config(&services.config);
        cx.set_global(crate::clock::AppClock(clock));
```

and in the startup diagnostics block (~1527) add, after `modules_default_diagnostic`:

```rust
            diags.extend(clock_diags.iter().cloned());
```

(move the `from_config` call ABOVE that block so `clock_diags` is in scope; the four groups become five: config, mod alias, `modules.default`, `[time]`, keymap — keep the same order in `apply_reload`). Field init `today: chrono::Local::now().date_naive()` → `today: clock.today(chrono::Utc::now())`. The poll tick (~1341): `let today = chrono::Local::now().date_naive();` → `let today = cx.global::<crate::clock::AppClock>().0.today(chrono::Utc::now());` (the closure is `this.update(cx, |view, cx| …)` — rename its `_cx` to `cx`).

Add to `impl ShellView`:

```rust
    /// The configured clock (`AppClock`), for the shell's own painters.
    pub fn clock(&self, cx: &gpui::App) -> geode_core::clock::Clock {
        cx.global::<crate::clock::AppClock>().0
    }
```

`hot_reload.rs`, beside `modules_default` (~199): `let (clock, clock_diags) = geode_core::clock::Clock::from_config(&new_config);` and fold `clock_diags` into the same `Vec` in the same position (fourth group). In the applied branch beside the `line_numbers` re-derive (~397):

```rust
            if clock != cx.global::<crate::clock::AppClock>().0 {
                cx.set_global(crate::clock::AppClock(clock));
                self.today = clock.today(chrono::Utc::now());
                cx.notify();
            }
```

- [ ] **Step 4: Run** — `cargo test -p geode-shell reload && cargo test -p geode-shell` → green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell/src/clock.rs crates/geode-shell/src/lib.rs crates/geode-shell/src/shell/mod.rs crates/geode-shell/src/shell/hot_reload.rs crates/geode-shell/src/shell/tests/reload.rs
git commit -m "shell: AppClock global from [time], set at startup and on reload; [time] diagnostics folded"
```

---

### Task 6: The shell's own displayed times — scope bar, frame bar model, as-of dialog (interim)

**Files:**
- Modify: `crates/geode-shell/src/scopebar.rs:118-160` (`build_model` signature and the as-of chip), `crates/geode-shell/src/frame.rs:628-650` (`bar_model`), `crates/geode-shell/src/shell/render.rs:949`, `crates/geode-shell/src/shell/asof_view.rs` (every `Local`), and the tests in `scopebar.rs` (208, 228, 304), `frame.rs` (1184–1224), `asof_view.rs` (563–726), `shell/tests/asof.rs` (any `Local`)

**Interfaces:**
- Produces: `scopebar::build_model(frame: &Frame, clock: Clock, today: NaiveDate) -> ScopeBarModel`; `Frame::bar_model(&self, clock: Clock, today: NaiveDate) -> Rc<ScopeBarModel>` (cache key gains `clock`); `asof_view::presets(frame, clock)`, `resolve_input(text, now, clock)`, `on_query_changed(state, text, now, clock)`, `calendar_date(state, now, clock)`.

- [ ] **Step 1: Write the failing tests**

`scopebar.rs` tests — add:

```rust
    #[test]
    fn the_as_of_chip_reads_on_the_clock_not_utc() {
        use chrono::TimeZone;
        let mut f = Frame::new(GroupingSlots::default(), SavedScopes::new(), None);
        let t = chrono::Utc.with_ymd_and_hms(2026, 9, 18, 22, 0, 0).unwrap();
        f.set_as_of(geode_core::query::AsOf::At(t));
        let utc = geode_core::clock::Clock::utc();
        let m = build_model(&f, utc, utc.today(t));
        assert_eq!(m.as_of.as_deref(), Some("22:00"), "today on the clock: HH:MM alone");
        let m = build_model(&f, utc, utc.today(t).succ_opt().unwrap());
        assert_eq!(m.as_of.as_deref(), Some("2026-09-18 22:00"), "another day: dated");
        assert_eq!(m.as_of_full.as_deref(), Some("2026-09-18 22:00:00"));
    }
```

(`Frame::new(GroupingSlots::default(), SavedScopes::new(), None)` is the fixture the file's other tests build; the `use` lines for `GroupingSlots`/`SavedScopes` already exist there.) `frame.rs` tests: replace each `chrono::Local::now().date_naive()` with `Clock::utc().today(chrono::Utc::now())` and each `f.bar_model(today)` with `f.bar_model(Clock::utc(), today)`; add one assertion to the cache test that a different clock rebuilds:

```rust
        let m3 = f.bar_model(Clock::in_zone_utc_plus_for_test(), today);
```

— no such helper exists; instead use `Clock::utc().with_times(hm(7, 0), hm(17, 0))` (a `PartialEq`-different clock) and assert `!Rc::ptr_eq(&m1, &m3)`.

`asof_view.rs` tests: every expectation built with `Local` becomes the same expression through `Clock::utc()` (`clock.today(now)`, `clock.resolve_local(..)`), and every call gains the `clock` argument.

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-shell scopebar frame asof_view` → compile errors.

- [ ] **Step 3: Implement**

`scopebar.rs`: `pub fn build_model(frame: &Frame, clock: Clock, today: NaiveDate) -> ScopeBarModel`; the as-of arm:

```rust
        geode_core::query::AsOf::At(t) => {
            let local = clock.local(*t);
            let elided = if local.date_naive() == today {
                local.format("%H:%M").to_string()
            } else {
                local.format("%Y-%m-%d %H:%M").to_string()
            };
            let full: SharedString = local.format("%Y-%m-%d %H:%M:%S").to_string().into();
            (Some(elided), Some(full))
        }
```

Remove the `Local` import. `frame.rs`: `bar_model(&self, clock: Clock, today: NaiveDate)`; the cache tuple becomes `(FrameVersions, Clock, NaiveDate, Rc<ScopeBarModel>)` and the hit test adds `&& *cached_clock == clock`; the build is `scopebar::build_model(self, clock, today)`. `render.rs:949`: `self.frame.read(cx).bar_model(self.clock(cx), self.today)`.

`asof_view.rs` (interim, Part 3 deletes most of this): thread `clock: Clock` through `presets`, `cached_presets`, `resolve_input`, `on_query_changed`, `calendar_date`; `open`'s calendar seed uses `view.clock(cx).today(Utc::now())`; the `→` preview uses `clock.full(t)`; the preset label uses `clock.hms(p.at)`; `handle_key`/`build` read `shell.clock(cx)` and pass it; the `mod.rs` subscription arm (`on_query_changed(state, &query, Utc::now())`, ~1199) and `calendar_date` call (~1211) pass `view.clock(cx)` — note the closure there has `cx`; read the clock BEFORE taking `view.as_of_dialog.as_mut()` to avoid a borrow conflict:

```rust
            } else if view.as_of_dialog.is_some() {
                let clock = view.clock(cx);
                let now = chrono::Utc::now();
                if let Some(state) = view.as_of_dialog.as_mut() {
                    asof_view::on_query_changed(state, &query, now, clock);
                }
                …
```

`presets_cache` keys on `(data version)` today; a zone change must also invalidate it — key on `(u64, Clock)`.

- [ ] **Step 4: Run** — `cargo test -p geode-shell && cargo clippy -p geode-shell --all-targets -- -D warnings` → green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-shell
git commit -m "shell: scope bar, bar model and the as-of dialog format on AppClock"
```

---

### Task 7: The modules — blotter, market-data, diagnostics, app

**Files:**
- Modify: `crates/geode-blotter/src/tile.rs` (~307 observe, ~1052 `:asof`, ~1297 `short_time`, ~1425–1445 the header's freshness and `AS OF` readouts, ~1507 `short_time` test)
- Modify: `crates/geode-marketdata/src/tile.rs` (~556 `new`, ~1154, ~1606 `rebuild_chrome`, ~2060, ~2855, the tests at 5553/5624/6124/8340/8394 and 10654/11005/11093), `header.rs` (~174 `HeaderInputs`, ~235, ~261, tests 711/778), `core/draft.rs:1001` (`local_hhmm`), `core/menu.rs:111`
- Modify: `crates/geode-diagnostics/src/sections.rs` (`local_hms`, `local_hms_utc`, `sources_rows`, `data_rows`, `log_rows` and their tests), `crates/geode-diagnostics/src/tile.rs:355-386`
- Modify: `crates/geode-app/src/main.rs:174`

**Interfaces:**
- Consumes: `AppClock`, `Clock::{hm, hms, today, local}`, `parse_as_of(text, now, &clock)`.
- Produces: `geode_marketdata::core::draft::local_hhmm(rfc3339: &str, clock: Clock) -> String`; `HeaderInputs.clock: Clock`; `sections::{sources_rows(d, now, clock), data_rows(d, as_of, collapsed, clock), log_rows(records, filter, clock)}`; blotter `short_time(t: &str, clock: Clock) -> String`.

- [ ] **Step 1: Write the failing tests**

Blotter (`tile.rs`, replace `short_time_falls_back_to_the_whole_string_instead_of_panicking`):

```rust
#[test]
fn short_time_formats_an_rfc3339_instant_on_the_clock_and_echoes_garbage() {
    use geode_core::clock::Clock;
    let utc = Clock::utc();
    assert_eq!(short_time("2026-08-30T14:32:00Z", utc), "14:32");
    let shifted = Clock::in_zone_named("Asia/Tokyo");
    assert_eq!(short_time("2026-08-30T14:32:00Z", shifted), "23:32", "Tokyo is UTC+9");
    // Not a time: echoed, never a panic.
    assert_eq!(short_time("2026", utc), "2026");
    assert_eq!(short_time("", utc), "");
}
```

This needs `Clock::in_zone_named(name: &str) -> Clock` (a test-friendly constructor that panics on an unknown name) — add it to `geode_core::clock` in this task, `#[doc(hidden)]`, since the blotter cannot name `chrono_tz`:

```rust
    /// `in_zone` by IANA name — for tests in crates that do not depend on
    /// `chrono-tz`. Panics on an unknown name.
    #[doc(hidden)]
    pub fn in_zone_named(name: &str) -> Clock {
        Clock::in_zone(Tz::from_str(name).expect("a known IANA zone name"))
    }
```

Market-data (`core/draft.rs` tests, replace the `local_hhmm("not a time")` test's neighbour):

```rust
    #[test]
    fn local_hhmm_reads_on_the_clock() {
        use geode_core::clock::Clock;
        assert_eq!(local_hhmm("2026-09-18T22:00:00Z", Clock::utc()), "22:00");
        assert_eq!(local_hhmm("2026-09-18T22:00:00Z", Clock::in_zone_named("Asia/Tokyo")), "07:00");
        assert_eq!(local_hhmm("not a time", Clock::utc()), "not a time");
    }
```

Diagnostics (`sections.rs` tests): the existing tests that build expectations with `Local` (grep `Local` in that file's tests) switch to `Clock::utc()` and pass it; add:

```rust
    #[test]
    fn log_rows_stamp_each_record_on_the_clock() {
        use geode_core::clock::Clock;
        let at: SystemTime = chrono::DateTime::parse_from_rfc3339("2026-09-18T22:00:00Z")
            .unwrap()
            .to_utc()
            .into();
        let records = vec![Record {
            at,
            level: geode_core::log::Level::INFO,
            target: "geode::ingest",
            message: "loaded".into(),
            seq: 1,
        }];
        let rows = log_rows(&records, "", Clock::in_zone_named("Asia/Tokyo"));
        assert!(rows[0].text.starts_with("07:00:00"), "{}", rows[0].text);
    }
```

(`Record`'s literal form is the one `log_rows_filter_by_target_or_level_text` in the same file already uses.)

- [ ] **Step 2: Run to verify they fail** — `cargo test -p geode-blotter short_time && cargo test -p geode-marketdata local_hhmm && cargo test -p geode-diagnostics log_rows` → compile errors.

- [ ] **Step 3: Implement**

**Blotter.** In `BlotterTile::new` beside the `UiSettings` observer:

```rust
        cx.observe_global::<geode_shell::clock::AppClock>(|_this, cx| cx.notify())
            .detach();
```

`:asof`: `let clock = cx.global::<geode_shell::clock::AppClock>().0; let at = parse_as_of(&text, chrono::Utc::now(), &clock)?;`. `short_time`:

```rust
/// The `HH:MM` of an RFC 3339 `as_of` on the trader's clock, for the
/// header's per-dataset freshness readout; a string that is not an
/// instant is echoed whole rather than sliced (a `&t[11..16]` panicked
/// on short input once).
fn short_time(t: &str, clock: geode_core::clock::Clock) -> String {
    match chrono::DateTime::parse_from_rfc3339(t) {
        Ok(at) => clock.hm(at.to_utc()),
        Err(_) => t.to_string(),
    }
}
```

In the header render (~1425): read `let clock = cx.global::<geode_shell::clock::AppClock>().0;` once at the top of the header build, `short_time(t, clock)`, and the `AS OF` chip:

```rust
            if let Some(req) = &p.as_of_request {
                let text = chrono::DateTime::parse_from_rfc3339(req)
                    .map(|t| format!("AS OF {}", clock.local(t.to_utc()).format("%Y-%m-%d %H:%M")))
                    .unwrap_or_else(|_| format!("AS OF {}", req.get(..16).unwrap_or(req)));
                header = header.child(
                    div()
                        .text_color(warn_chip.text)
                        .when_some(warn_chip.fill, |el, fill| el.bg(fill))
                        .px_1()
                        .rounded(theme.radius_tokens().sm)
                        .child(text),
                );
            }
```

**Market-data.** `local_hhmm(rfc3339: &str, clock: Clock) -> String` using `clock.hm(t.to_utc())`; `menu.rs:111` and `tile.rs:1154` pass `self.clock` / the tile's clock (the menu builder gets a `clock: Clock` parameter from its one caller in the tile). `HeaderInputs` gains `pub clock: Clock`; `prepare`'s `time:` uses `i.clock.hms(t)` and the `Behind` arm `local_hhmm(newer, i.clock)`. The tile stores `clock: Clock` (set from the global in `new`, refreshed in an `observe_global::<AppClock>` handler that also calls `self.rebuild_chrome(); cx.notify();`), passes it in `rebuild_chrome`, and seeds the date field with `self.clock.today(chrono::Utc::now())` at both open sites. The tests at 5553/5624/6124/8340/8394 compute expectations with `Local` — since the test harness's global is whatever `Clock::from_config` gives an empty config (the machine), read the clock the tile holds instead: `let clock = h.tile.read_with(&vcx, |t, _| t.clock);` then `clock.hms(...)`/`clock.hm(...)`. The `header.rs` tests at 711/778 build `HeaderInputs` through `inputs(..)` — give it `clock: Clock::utc()` and format expectations with `Clock::utc()`.

**Diagnostics.** `sections.rs`: `fn local_hms(t: SystemTime, clock: Clock)`, `fn local_hms_utc(t: DateTime<Utc>, clock: Clock)` using `clock.hms(..)`; `sources_rows(d, now, clock)`, `data_rows(d, as_of, collapsed, clock)`, `log_rows(records, filter, clock)` thread it. `tile.rs:355-386` (inside `rebuild`): `let clock = cx.global::<geode_shell::clock::AppClock>().0;` before the match and pass it to the three calls; in `new`, beside the existing `cx.observe(&diagnostics, …)`, add `cx.observe_global::<geode_shell::clock::AppClock>(|this, cx| this.rebuild(cx)).detach();` (`rebuild` is the tile's existing section-rebuild method and already notifies).

**App.** `main.rs:174`: `let today = geode_core::clock::Clock::from_config(&config).0.today(chrono::Utc::now());` (`config` is in scope there; if it has moved into `services` by that line, read it before the move).

- [ ] **Step 4: Run** — `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → green.

- [ ] **Step 5: Commit**

```bash
git add crates/geode-blotter crates/geode-marketdata crates/geode-diagnostics crates/geode-app crates/geode-core/src/clock.rs
git commit -m "modules: blotter, market-data, diagnostics and the demo seed read AppClock"
```

---

### Task 8: The `Local` guard, harness entries, docs

**Files:**
- Modify: `crates/geode-core/src/clock.rs` (guard test), `scripts/mutation-check.sh`, `CLAUDE.md`, `docs/phase-history.md`

- [ ] **Step 1: Write the guard test** (in `clock.rs` tests)

```rust
    /// Every crate's `src/**/*.rs` — `chrono::Local` is banned (spec
    /// §6.3): a stray site is silent wrong-time on every zone but the
    /// machine's, which no fixture on a developer's machine would catch.
    /// The scanner is checked against a planted line first, so an emptied
    /// pattern list cannot make this pass vacuously.
    #[test]
    fn no_crate_uses_chrono_local() {
        const PATTERNS: [&str; 4] =
            ["chrono::Local", "Local::now", "with_timezone(&Local)", "use chrono::{Local"];
        fn hits(text: &str) -> usize {
            text.lines()
                .filter(|l| PATTERNS.iter().any(|p| l.contains(p)))
                .count()
        }
        assert_eq!(hits("let t = chrono::Local::now();"), 1, "the scanner must see a planted site");
        assert_eq!(hits("use chrono::{DateTime, Utc};"), 0);

        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut offenders = Vec::new();
        fn walk(dir: &std::path::Path, out: &mut Vec<String>, hits: &dyn Fn(&str) -> usize) {
            for entry in std::fs::read_dir(dir).expect("readable dir") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    walk(&path, out, hits);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).expect("readable file");
                    let n = hits(&text);
                    if n > 0 {
                        out.push(format!("{}: {n}", path.display()));
                    }
                }
            }
        }
        walk(&crates, &mut offenders, &hits);
        assert!(offenders.is_empty(), "chrono::Local is banned; use Clock:\n{}", offenders.join("\n"));
    }
```

The scanner reads THIS file too; keep the planted string split so it does not match itself (`"chrono::Loc".to_string() + "al::now();"` in the self-check, and write `PATTERNS` with the same split, e.g. `concat!("chrono::", "Local")` for each). Run it and fix the four crates' leftovers it names (there should be none after Task 7; the comment in `geode-shell/Cargo.toml` mentioning `chrono::Local::now()` is prose in a TOML file and not scanned).

- [ ] **Step 2: Run** — `cargo test -p geode-core no_crate_uses_chrono_local` → green (or names the leftover; fix it).

- [ ] **Step 3: Harness entries**

```zsh
# As-of dialog Part 2 (2026-09-20): one clock. A weekend must not count.
run_mutation "clock: business days skip the weekend" \
  crates/geode-core/src/clock.rs \
  '        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {' \
  '        {' \
  geode-core \
  business_days_back_skips_weekends_and_snaps_a_weekend_start_to_friday

# A preset after `now` means live and must be dropped, not offered.
run_mutation "clock: a future preset is dropped" \
  crates/geode-core/src/clock.rs \
  '            Ok(at) if at <= now => Some(Preset { label, at }),' \
  '            Ok(at) => Some(Preset { label, at }),' \
  geode-core \
  presets_resolve_on_business_days_in_order_and_drop_a_future_one

# The guard's scanner must actually see a `Local`: an emptied pattern
# list passes the tree vacuously and the self-check is what catches it.
run_mutation "clock: the Local guard sees a planted site" \
  crates/geode-core/src/clock.rs \
  '                .filter(|l| PATTERNS.iter().any(|p| l.contains(p)))' \
  '                .filter(|_l| false)' \
  geode-core \
  no_crate_uses_chrono_local

# HH:MM resolves on the CLOCK's date: on UTC's date a New York trader at
# 21:00 typing "14:05" would get tomorrow.
run_mutation "clock: parse_as_of resolves on the clock's date" \
  crates/geode-core/src/query.rs \
  '    let today = clock.today(now);' \
  '    let today = now.date_naive();' \
  geode-core \
  as_of_resolves_on_the_clocks_date_not_utcs

# A bad zone is an ERROR, not a silent fallback.
run_mutation "clock: a bad zone name is an error diagnostic" \
  crates/geode-core/src/clock.rs \
  '                        Severity::Error,
                        "zone",' \
  '                        Severity::Warning,
                        "zone",' \
  geode-core \
  a_bad_zone_or_time_is_an_error_at_its_key_and_the_default_applies

# A reload must republish the global or every module keeps the old zone.
run_mutation "clock: a reload republishes AppClock" \
  crates/geode-shell/src/shell/hot_reload.rs \
  '                cx.set_global(crate::clock::AppClock(clock));' \
  '                let _ = clock;' \
  geode-shell \
  a_time_zone_reload_republishes_the_clock_without_a_requery

# The blotter's freshness readout formats on the clock, not by slicing UTC.
run_mutation "clock: the blotter's short_time reads on the clock" \
  crates/geode-blotter/src/tile.rs \
  '        Ok(at) => clock.hm(at.to_utc()),' \
  '        Ok(at) => at.format("%H:%M").to_string(),' \
  geode-blotter \
  short_time_formats_an_rfc3339_instant_on_the_clock_and_echoes_garbage
```

Update `CLAUDE.md`'s entry count.

- [ ] **Step 4: Run** — `zsh scripts/mutation-check.sh --anchors-only && zsh scripts/mutation-check.sh "clock:"` → 0 and every entry `CAUGHT`.

- [ ] **Step 5: Docs**

`CLAUDE.md`:
- Config rule bullet: replace "every *displayed* time is the trader's local clock (Phase 4a ruling), including `HH:MM` as-of input" with "every *displayed* time is the trader's CONFIGURED clock — `[time] zone` (IANA name, the machine's zone by default), `sod`/`eod` beside it (as-of dialog ruling 2026-09-20) — including `HH:MM` as-of input; `chrono::Local` is banned workspace-wide (`clock::tests::no_crate_uses_chrono_local`) and every formatter takes a `geode_core::clock::Clock`. The daily log and crash file names still roll on the UTC date."
- The globals bullet (`[ui] line_numbers` … `tips::Chords` — "the workspace's two globals"): now three — add `geode_shell::clock::AppClock`, written at startup and on a `[time]` reload only, read by modules with `observe_global`.
- Status table row: `| As-of dialog Part 2 (2026-09-20) | `[time] zone/sod/eod`, `geode_core::clock::Clock` (presets over business days), `AppClock` global, every displayed time on it, `chrono::Local` gone (guard test). Part 3 (the dialog) next. | `2026-09-20-…as-of-dialog` §3, §6 |`
- The startup-diagnostics rule ("four groups, same order") → five groups (config, mod alias, `modules.default`, `[time]`, keymap).

`docs/phase-history.md`: a paragraph for Part 2 naming the rulings, the blotter's UTC-slicing finding (the freshness and `AS OF` readouts were UTC before this), and the harness entries.

- [ ] **Step 6: Full verification and commit**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo bench --workspace --no-run && cargo check -p geode-shell --features test-support --all-targets`

```bash
git add crates/geode-core/src/clock.rs scripts/mutation-check.sh CLAUDE.md docs/phase-history.md
git commit -m "clock: Local guard, harness entries and docs — one clock everywhere"
```
