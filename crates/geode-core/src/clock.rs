//! The configured clock for displayed times and local-time input. `[time] zone`
//! selects an IANA zone, defaulting to the machine's zone; `sod` and `eod` supply
//! day-boundary times for as-of presets. The shell publishes this `Copy` value
//! through `AppClock` for modules to read.
//!
//! Storage, query timestamps, and log/crash filenames remain UTC. Local-time
//! conversion belongs at the input and display boundaries.

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

    /// `zone`, named — for a test in a downstream crate that wants a
    /// specific non-UTC zone without depending on `chrono-tz` itself
    /// directly (this crate is the one place in the workspace that
    /// names a `Tz`; see the module doc). Panics on a name the IANA
    /// database doesn't have — every call site names a real zone
    /// literally, so an unknown name is a typo in the test, not
    /// something to route through `Result`.
    #[doc(hidden)]
    pub fn in_zone_named(name: &str) -> Clock {
        Clock::in_zone(Tz::from_str(name).expect("test names a valid IANA zone"))
    }

    pub fn with_times(mut self, sod: NaiveTime, eod: NaiveTime) -> Clock {
        self.sod = sod;
        self.eod = eod;
        self
    }

    /// Read and memoize the machine's zone once per process. This keeps OS zone
    /// lookup out of repeated render-time fallbacks when `AppClock` is absent.
    /// Changes to the machine zone during the process are not detected. Return
    /// UTC with a warning if lookup fails or the zone name is unrecognized.
    pub fn machine() -> (Clock, Option<String>) {
        static MACHINE: std::sync::OnceLock<(Clock, Option<String>)> = std::sync::OnceLock::new();
        MACHINE
            .get_or_init(|| machine_from(iana_time_zone::get_timezone()))
            .clone()
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

/// Convert an OS zone-lookup result into a clock and optional warning.
/// Accepting the result as an argument makes lookup failure and unknown-zone
/// fallbacks testable without changing the machine's configuration.
fn machine_from(zone: Result<String, impl std::fmt::Display>) -> (Clock, Option<String>) {
    match zone {
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

/// Walk `n` business days back from `date`, skipping weekends without a
/// holiday calendar. A weekend date first snaps to the preceding Friday, so
/// `T-0` on a weekend is Friday and `T-1` is Thursday.
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

/// One named preset the as-of dialog offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub label: &'static str,
    pub at: DateTime<Utc>,
}

/// The fixed preset list, in display order. `T` is `now` on
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::config_from;
    use chrono::TimeZone;
    use chrono_tz::America::New_York;
    use chrono_tz::Asia::Tehran;
    use chrono_tz::Europe::London;

    /// Unknown zone names and OS lookup failures both return UTC with a warning.
    /// Inject the lookup result so the test is independent of the host machine.
    #[test]
    fn machine_from_an_unreadable_zone_is_utc_with_a_warning_naming_time_zone() {
        let zone: Result<String, &str> = Err("no zone");
        let (clock, warning) = machine_from(zone);
        assert_eq!(clock, Clock::utc());
        let warning = warning.expect("a warning");
        assert!(warning.starts_with("time.zone:"), "{warning}");
        assert!(warning.contains("could not read"), "{warning}");
    }

    #[test]
    fn machine_from_an_unrecognised_zone_name_is_utc_with_a_warning() {
        let (clock, warning) = machine_from(Ok::<_, std::convert::Infallible>(
            "Mars/Olympus".to_string(),
        ));
        assert_eq!(clock, Clock::utc());
        let warning = warning.expect("a warning");
        assert!(warning.starts_with("time.zone:"), "{warning}");
        assert!(warning.contains("Mars/Olympus"), "{warning}");
    }

    #[test]
    fn machine_from_a_known_zone_name_is_that_zone_with_no_warning() {
        let (clock, warning) =
            machine_from(Ok::<_, std::convert::Infallible>("Asia/Tokyo".to_string()));
        assert_eq!(clock.zone_name(), "Asia/Tokyo");
        assert!(warning.is_none(), "{warning:?}");
    }

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

    #[test]
    fn presets_resolve_on_business_days_in_order_and_drop_a_future_one() {
        let c = Clock::in_zone(New_York);
        // Monday 21 Sep 2026, 10:42 New York (14:42 UTC).
        let now = Utc.with_ymd_and_hms(2026, 9, 21, 14, 42, 0).unwrap();
        let p = presets(&c, now);
        let labels: Vec<&str> = p.iter().map(|p| p.label).collect();
        assert_eq!(
            labels,
            ["EOD T-1", "SOD T", "EOD T-2", "EOD T-3", "EOD T-5"]
        );
        assert_eq!(p[0].at, c.eod_of(d(2026, 9, 18)).unwrap(), "Friday's close");
        assert_eq!(p[1].at, c.sod_of(d(2026, 9, 21)).unwrap());
        assert_eq!(p[4].at, c.eod_of(d(2026, 9, 14)).unwrap());

        // 07:00 New York: SOD T (08:00) is in the future and is dropped.
        let early = Utc.with_ymd_and_hms(2026, 9, 21, 11, 0, 0).unwrap();
        let labels: Vec<&str> = presets(&c, early).iter().map(|p| p.label).collect();
        assert_eq!(labels, ["EOD T-1", "EOD T-2", "EOD T-3", "EOD T-5"]);
    }

    #[test]
    fn a_preset_whose_local_time_does_not_exist_is_dropped() {
        // Two facts, checked separately, because business_days_back's
        // weekend snap means the preset walk itself never lands `SOD T`
        // on a Sunday: (2026-03-08 is the US spring-forward day, so a
        // clock whose `sod` is 02:30 cannot resolve it in New York that
        // day — but `today` on a Sunday `now` snaps `SOD T`'s business
        // day back to Friday 2026-03-06, where 02:30 DOES exist, so the
        // gap is never actually hit by `presets` from this `now`.)
        //
        // Fact 1: `presets` never panics on a config that CAN produce a
        // DST-gap `Err` from `resolve_local`, and every preset it does
        // return resolves at or before `now`.
        let c = Clock::in_zone(New_York).with_times(hm(2, 30), hm(18, 0));
        let now = Utc.with_ymd_and_hms(2026, 3, 8, 20, 0, 0).unwrap(); // Sunday 16:00 EDT
        let p = presets(&c, now);
        for preset in &p {
            assert!(preset.at <= now, "{preset:?} resolved after now");
        }
        // Fact 2: the drop arm's own input condition — a DST-gap `Err`
        // from `resolve_local` — is directly reachable: 02:30 does not
        // exist in New York on 2026-03-08 itself.
        assert!(c.sod_of(d(2026, 3, 8)).is_err());
    }

    #[test]
    fn a_preset_in_a_weekday_dst_gap_is_dropped_by_presets_itself() {
        // The previous test proves the drop arm's INPUT condition is
        // reachable but never actually drives an `Err` through `presets`
        // end to end, because every US/EU DST gap falls on a Sunday and
        // `business_days_back`'s weekend snap never selects one. Iran's
        // history (carried by chrono-tz) has a gap on a WEEKDAY instead:
        // 2019-03-22 00:00 -> 01:00 Tehran, and 2019-03-22 is a Friday —
        // an ordinary business day the snap never moves off of — so
        // `SOD T` (00:30) lands squarely in the gap and this test
        // exercises the real `Err(e) => { tracing::debug!(...); None }`
        // arm inside `presets`, not just its precondition.
        let c = Clock::in_zone(Tehran).with_times(hm(0, 30), hm(18, 0));
        // Verify the gap exists on that exact date before relying on it.
        assert!(c.sod_of(d(2019, 3, 22)).is_err());

        let now = Utc.with_ymd_and_hms(2019, 3, 22, 12, 0, 0).unwrap();
        let labels: Vec<&str> = presets(&c, now).iter().map(|p| p.label).collect();
        assert!(!labels.contains(&"SOD T"), "{labels:?}");
        assert!(labels.contains(&"EOD T-1"), "{labels:?}");
    }

    /// Every crate's `src/**/*.rs` — chrono's own machine-zone clock is
    /// disallowed: a stray site displays the wrong time in every
    /// zone but the machine's, which no fixture on a developer's machine
    /// would catch. The scanner is checked against a planted line first,
    /// so an emptied pattern list cannot make this pass vacuously — and
    /// `PATTERNS` plus the planted string are both built with `concat!`
    /// so the literal substrings never appear contiguous in THIS file's
    /// own source, which the scan below also reads.
    #[test]
    fn no_crate_uses_chrono_local() {
        const PATTERNS: [&str; 4] = [
            concat!("chrono", "::Local"),
            concat!("Local", "::now"),
            concat!("with_timezone(&", "Local)"),
            concat!("use chrono::{", "Local"),
        ];
        fn hits(text: &str) -> usize {
            text.lines()
                .filter(|l| PATTERNS.iter().any(|p| l.contains(p)))
                .count()
        }
        let planted = concat!("let t = chrono", "::", "Local", "::now();");
        assert_eq!(hits(planted), 1, "the scanner must see a planted site");
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
        assert!(
            offenders.is_empty(),
            "a banned chrono clock is in use; use geode_core::clock::Clock instead:\n{}",
            offenders.join("\n")
        );
    }
}
