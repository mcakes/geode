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
            .ok_or_else(|| {
                ClockError::NoSuchLocalTime(naive.format("%Y-%m-%d %H:%M:%S").to_string())
            })
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
        let mut clock = match config.get("app", "time.zone") {
            Some(v) => {
                match v.as_str() {
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
                        diags.push(diag(
                        Severity::Error,
                        "zone",
                        format!("expected an IANA zone name string, got {v} — using the machine's zone"),
                    ));
                        Clock::machine().0
                    }
                }
            }
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
                match v
                    .as_str()
                    .and_then(|s| NaiveTime::parse_from_str(s, "%H:%M").ok())
                {
                    Some(t) => *slot = t,
                    None => diags.push(diag(
                        Severity::Error,
                        key,
                        format!(
                            "expected \"HH:MM\", got {v} — using {}",
                            default.format("%H:%M")
                        ),
                    )),
                }
            }
        }
        (clock, diags)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::config_from;
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
        assert_eq!(
            Clock::in_zone(New_York).full(now),
            "2026-09-20 21:30:00 EDT"
        );
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
        assert!(
            err.to_string().contains("does not name a valid local time"),
            "{err}"
        );
        // 2026-11-01 01:30 exists twice.
        assert!(c.resolve_local(d(2026, 11, 1), hm(1, 30)).is_err());
        assert!(c.resolve_local(d(2026, 11, 1), hm(3, 30)).is_ok());
    }

    #[test]
    fn business_days_back_skips_weekends_and_snaps_a_weekend_start_to_friday() {
        let mon = d(2026, 9, 21);
        assert_eq!(business_days_back(mon, 0), mon);
        assert_eq!(
            business_days_back(mon, 1),
            d(2026, 9, 18),
            "Monday's T-1 is Friday"
        );
        assert_eq!(business_days_back(mon, 2), d(2026, 9, 17));
        assert_eq!(
            business_days_back(mon, 5),
            d(2026, 9, 14),
            "T-5 is the previous Monday"
        );
        let sat = d(2026, 9, 19);
        assert_eq!(
            business_days_back(sat, 0),
            d(2026, 9, 18),
            "a Saturday snaps to Friday"
        );
        assert_eq!(
            business_days_back(sat, 1),
            d(2026, 9, 17),
            "…and then walks"
        );
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
        assert_eq!(
            clock.zone_name(),
            Clock::machine().0.zone_name(),
            "the machine's zone"
        );
        assert_eq!(clock.sod, Clock::DEFAULT_SOD);
    }

    #[test]
    fn a_non_string_zone_is_an_error_and_the_machine_zone_applies() {
        let cfg = config_from("app", "[time]\nzone = 42\n");
        let (clock, diags) = Clock::from_config(&cfg);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].path.as_deref(), Some("time.zone"));
        assert_eq!(clock.zone_name(), Clock::machine().0.zone_name());
    }

    #[test]
    fn an_absent_section_is_the_machine_clock() {
        let cfg = config_from("app", "config_version = 1\n");
        let (clock, diags) = Clock::from_config(&cfg);
        let (machine, warning) = Clock::machine();
        assert_eq!(clock, machine);
        assert_eq!(diags.len(), usize::from(warning.is_some()));
    }
}
