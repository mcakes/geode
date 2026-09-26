//! One query range per tile: a relative preset or two inclusive UTC dates.
//! Sessions retain presets as labels, so restoration resolves them against
//! the current time and frame as-of instead of freezing their original span.

use chrono::{DateTime, Days, Months, NaiveDate, Utc};
use geode_core::query::AsOf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    W1,
    M1,
    M3,
    M6,
    Y1,
    Y2,
    Y5,
}

impl Preset {
    pub const ALL: [Preset; 7] = [
        Preset::W1,
        Preset::M1,
        Preset::M3,
        Preset::M6,
        Preset::Y1,
        Preset::Y2,
        Preset::Y5,
    ];
    pub const WORDS: &'static str = "1w 1m 3m 6m 1y 2y 5y";

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::W1 => "1w",
            Preset::M1 => "1m",
            Preset::M3 => "3m",
            Preset::M6 => "6m",
            Preset::Y1 => "1y",
            Preset::Y2 => "2y",
            Preset::Y5 => "5y",
        }
    }
    pub fn parse(s: &str) -> Option<Preset> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }
    /// The preset written out, for its range-menu row.
    pub fn title(self) -> &'static str {
        match self {
            Preset::W1 => "1 week",
            Preset::M1 => "1 month",
            Preset::M3 => "3 months",
            Preset::M6 => "6 months",
            Preset::Y1 => "1 year",
            Preset::Y2 => "2 years",
            Preset::Y5 => "5 years",
        }
    }
    /// Subtract UTC calendar months (clamping month-end dates) or seven days.
    /// Date arithmetic overflow leaves the start at `to`.
    fn start_before(self, to: DateTime<Utc>) -> DateTime<Utc> {
        let months = |n: u32| to.checked_sub_months(Months::new(n)).unwrap_or(to);
        match self {
            Preset::W1 => to.checked_sub_days(Days::new(7)).unwrap_or(to),
            Preset::M1 => months(1),
            Preset::M3 => months(3),
            Preset::M6 => months(6),
            Preset::Y1 => months(12),
            Preset::Y2 => months(24),
            Preset::Y5 => months(60),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Range {
    Relative(Preset),
    /// Both inclusive as typed; `resolve` makes the span half-open at the
    /// midnight after `to`.
    Absolute {
        from: NaiveDate,
        to: NaiveDate,
    },
}

impl Default for Range {
    fn default() -> Self {
        Range::Relative(Preset::Y1)
    }
}

impl Range {
    /// Parse one exact preset label or two ordered YYYY-MM-DD dates.
    pub fn parse(words: &[&str]) -> Result<Range, String> {
        match words {
            [one] => Preset::parse(one).map(Range::Relative).ok_or_else(|| {
                format!(
                    "'{one}' is not a preset ({}) — or give <from> <to> as YYYY-MM-DD",
                    Preset::WORDS
                )
            }),
            [a, b] => {
                let date = |s: &str| {
                    NaiveDate::parse_from_str(s, "%Y-%m-%d")
                        .map_err(|_| format!("'{s}' is not a date (YYYY-MM-DD)"))
                };
                let (from, to) = (date(a)?, date(b)?);
                if to < from {
                    return Err(format!("'{b}' is before '{a}'"));
                }
                Ok(Range::Absolute { from, to })
            }
            _ => Err(format!(
                "range is a preset ({}) or <from> <to>",
                Preset::WORDS
            )),
        }
    }

    /// Resolve a half-open UTC span for fetches and queries. Relative presets
    /// measure backward from now clipped to the frame's as-of; calendar months
    /// retain their width relative to that clipped endpoint.
    /// Absolute dates span UTC midnights through the day after `to`, clipped
    /// only by as-of, not by now. An as-of before `from` yields an empty span.
    pub fn resolve(&self, now: DateTime<Utc>, as_of: &AsOf) -> (DateTime<Utc>, DateTime<Utc>) {
        let clip = |t: DateTime<Utc>| match as_of {
            AsOf::Live => t,
            AsOf::At(at) => t.min(*at),
        };
        match self {
            Range::Relative(p) => {
                let to = clip(now);
                (p.start_before(to), to)
            }
            Range::Absolute { from, to } => {
                let from = from.and_hms_opt(0, 0, 0).expect("midnight").and_utc();
                let end = to
                    .checked_add_days(Days::new(1))
                    .unwrap_or(*to)
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight")
                    .and_utc();
                (from, clip(end).max(from))
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Range::Relative(p) => p.as_str().to_string(),
            Range::Absolute { from, to } => format!("{from} → {to}"),
        }
    }

    pub fn to_toml(&self) -> toml::Value {
        match self {
            Range::Relative(p) => toml::Value::String(p.as_str().into()),
            Range::Absolute { from, to } => {
                let mut t = toml::Table::new();
                t.insert("from".into(), toml::Value::String(from.to_string()));
                t.insert("to".into(), toml::Value::String(to.to_string()));
                toml::Value::Table(t)
            }
        }
    }

    pub fn from_toml(v: &toml::Value) -> Option<Range> {
        match v {
            toml::Value::String(s) => Preset::parse(s).map(Range::Relative),
            toml::Value::Table(t) => {
                let d = |k: &str| {
                    t.get(k)?
                        .as_str()
                        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                };
                let (from, to) = (d("from")?, d("to")?);
                (to >= from).then_some(Range::Absolute { from, to })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use geode_core::query::AsOf;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc))
    }

    #[test]
    fn presets_round_trip() {
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.as_str()), Some(p));
        }
        assert_eq!(Preset::parse("4m"), None);
    }

    #[test]
    fn a_relative_range_ends_now_and_an_as_of_clips_the_end() {
        let now = t("2026-09-19T15:00:00Z");
        let r = Range::Relative(Preset::Y1);
        let (from, to) = r.resolve(now, &AsOf::Live);
        assert_eq!(to, now);
        assert_eq!(from, t("2025-09-19T15:00:00Z"));
        let at = t("2026-03-01T12:00:00Z");
        let (from, to) = r.resolve(now, &AsOf::At(at));
        assert_eq!(to, at, "the frame's as-of clips the visible end (ruling 4)");
        assert_eq!(
            from,
            t("2025-03-01T12:00:00Z"),
            "and the width is kept, measured back from the clip"
        );
        let (from, _) = Range::Relative(Preset::W1).resolve(now, &AsOf::Live);
        assert_eq!(from, t("2026-09-12T15:00:00Z"));
        let (from, _) = Range::Relative(Preset::M3).resolve(now, &AsOf::Live);
        assert_eq!(from, t("2026-06-19T15:00:00Z"));
    }

    #[test]
    fn an_absolute_range_is_whole_days_half_open() {
        let r = Range::parse(&["2026-01-05", "2026-01-09"]).unwrap();
        let (from, to) = r.resolve(t("2026-09-19T15:00:00Z"), &AsOf::Live);
        assert_eq!(from, t("2026-01-05T00:00:00Z"));
        assert_eq!(
            to,
            t("2026-01-10T00:00:00Z"),
            "`to` is inclusive as typed, so the span ends at the next midnight"
        );
        assert_eq!(r.label(), "2026-01-05 → 2026-01-09");
        assert_eq!(Range::Relative(Preset::Y1).label(), "1y");
    }

    #[test]
    fn parse_refuses_a_backwards_range_and_an_unknown_word() {
        assert!(
            Range::parse(&["2026-01-09", "2026-01-05"])
                .unwrap_err()
                .contains("before")
        );
        assert!(
            Range::parse(&["4m"])
                .unwrap_err()
                .contains("1w 1m 3m 6m 1y 2y 5y")
        );
        assert!(Range::parse(&[]).is_err());
        assert!(
            Range::parse(&["2026-01-05"])
                .unwrap_err()
                .contains("<from> <to>")
        );
        assert_eq!(Range::parse(&["1y"]).unwrap(), Range::Relative(Preset::Y1));
    }

    #[test]
    fn a_range_round_trips_through_toml() {
        for r in [
            Range::Relative(Preset::M6),
            Range::Absolute {
                from: NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                to: NaiveDate::from_ymd_opt(2026, 2, 5).unwrap(),
            },
        ] {
            assert_eq!(Range::from_toml(&r.to_toml()), Some(r));
        }
        assert_eq!(Range::from_toml(&toml::Value::String("bogus".into())), None);
    }
}
