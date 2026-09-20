//! The segmented date-time field (as-of dialog spec 2026-09-20 §4.2;
//! originally the market-data header's date field, header spec §5.2):
//! a value, a precision, an active segment and the digits typed into it
//! this visit. Pure — a host routes keys here through [`route`] and
//! paints what [`DateTimeField::segments`] answers. The value is a valid
//! [`NaiveDateTime`] at every moment: a step rolls, clamps, wraps or
//! saturates, a digit that would make an impossible segment is refused,
//! and `enter` therefore has nothing to refuse.

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
        let segment = if segment.fits(precision) {
            segment
        } else {
            Segment::last(precision)
        };
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
                .unwrap_or(if n < 0 {
                    NaiveDate::MIN
                } else {
                    NaiveDate::MAX
                });
                self.value = stepped.and_time(time);
            }
            Segment::Hour | Segment::Minute | Segment::Second => {
                let modulus: i64 = if self.segment == Segment::Hour {
                    24
                } else {
                    60
                };
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
            let year = self
                .typed
                .parse::<i32>()
                .unwrap_or(self.value.date().year());
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
                    text: if typing && active {
                        self.typed.clone()
                    } else {
                        committed
                    },
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

/// `year-month-day`, with the day clamped to the month's length when it
/// would not exist there (Jan 31 → Feb 28/29, Feb 29 → Feb 28 in a common
/// year). `None` only when the year is outside chrono's range.
fn clamped_ymd(year: i32, month: u32, day: u32) -> Option<NaiveDate> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let last = days_in_month(first);
    NaiveDate::from_ymd_opt(year, month, day.min(last))
}

/// How many days the month of `date` holds.
fn days_in_month(date: NaiveDate) -> u32 {
    let (year, month) = (date.year(), date.month());
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    };
    match next {
        Some(next) => next.pred_opt().map_or(31, |d| d.day()),
        // December of chrono's last year: no successor month to count
        // back from, and December is always 31 days.
        None => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn dt(y: i32, m: u32, day: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, day)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    fn texts(f: &DateTimeField) -> [String; 3] {
        let v = f.segments();
        [v[0].text.clone(), v[1].text.clone(), v[2].text.clone()]
    }

    fn texts_of(f: &DateTimeField) -> Vec<String> {
        f.segments().into_iter().map(|s| s.text).collect()
    }

    #[test]
    fn the_field_opens_on_the_day_segment_with_nothing_typed() {
        let f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert_eq!(f.segment(), Segment::Day);
        assert_eq!(f.text(), "2026-09-14");
        let segs = f.segments();
        assert_eq!(texts(&f), ["2026", "09", "14"]);
        assert_eq!(
            segs.iter()
                .map(|s| (s.active, s.typing))
                .collect::<Vec<_>>(),
            vec![(false, false), (false, false), (true, false)]
        );
    }

    #[test]
    fn left_and_right_clamp_at_the_year_and_the_day() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.right();
        assert_eq!(f.segment(), Segment::Day, "right at the day stays");
        f.left();
        assert_eq!(f.segment(), Segment::Month);
        f.left();
        assert_eq!(f.segment(), Segment::Year);
        f.left();
        assert_eq!(f.segment(), Segment::Year, "left at the year stays");
        f.right();
        assert_eq!(f.segment(), Segment::Month);
    }

    #[test]
    fn a_day_step_rolls_over_into_the_next_month() {
        let mut f = DateTimeField::open(
            d(2026, 9, 30).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.step(1);
        assert_eq!(f.date(), d(2026, 10, 1));
        f.step(-1);
        assert_eq!(f.date(), d(2026, 9, 30));
        f.step(10);
        assert_eq!(f.date(), d(2026, 10, 10));
        let mut f = DateTimeField::open(
            d(2026, 12, 31).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.step(1);
        assert_eq!(f.date(), d(2027, 1, 1), "and across a year end");
    }

    #[test]
    fn a_month_step_clamps_the_day_to_the_new_months_length() {
        let mut f = DateTimeField::open(
            d(2026, 1, 31).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        f.step(1);
        assert_eq!(f.date(), d(2026, 2, 28));
        let mut f = DateTimeField::open(
            d(2024, 1, 31).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        f.step(1);
        assert_eq!(f.date(), d(2024, 2, 29), "a leap February keeps its 29th");
        f.step(-1);
        assert_eq!(
            f.date(),
            d(2024, 1, 29),
            "a clamped day stays clamped on the way back"
        );
        let mut f = DateTimeField::open(
            d(2026, 9, 17).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        f.step(-1);
        assert_eq!(
            f.date(),
            d(2026, 8, 17),
            "the mockup: 09 → 08 with the day kept"
        );
        f.step(10);
        assert_eq!(f.date(), d(2027, 6, 17), "ten months crosses the year");
    }

    #[test]
    fn a_year_step_clamps_feb_29_to_the_28th() {
        let mut f = DateTimeField::open(
            d(2024, 2, 29).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Year);
        f.step(1);
        assert_eq!(f.date(), d(2025, 2, 28));
        f.step(-1);
        assert_eq!(
            f.date(),
            d(2024, 2, 28),
            "and does not un-clamp on the way back"
        );
        let mut f = DateTimeField::open(
            d(2026, 9, 12).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Year);
        f.step(1);
        assert_eq!(f.date(), d(2027, 9, 12));
    }

    #[test]
    fn a_step_saturates_at_chronos_bounds_rather_than_panicking() {
        let mut f = DateTimeField::open(
            NaiveDate::MAX.and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.step(1);
        assert_eq!(f.date(), NaiveDate::MAX);
        f.select(Segment::Year);
        f.step(i64::MAX);
        assert_eq!(f.date(), NaiveDate::MAX);
        f.select(Segment::Month);
        f.step(i64::MIN);
        assert_eq!(f.date(), NaiveDate::MIN);
        f.select(Segment::Day);
        f.step(-1);
        assert_eq!(f.date(), NaiveDate::MIN);
        // The two arithmetic overflows: the month COUNT itself, and
        // `Duration::days`' own bound (which panics where `try_days`
        // answers `None`).
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        f.step(i64::MAX);
        assert_eq!(f.date(), NaiveDate::MAX);
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.step(i64::MAX);
        assert_eq!(f.date(), NaiveDate::MAX);
        f.step(i64::MIN);
        assert_eq!(f.date(), NaiveDate::MIN);
    }

    #[test]
    fn a_reselect_of_the_active_segment_drops_partial_digits() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.digit(2));
        f.right();
        assert_eq!(texts(&f)[2], "14", "right at the day re-selects it");
        assert!(!f.digit(2));
        f.select(Segment::Day);
        assert_eq!(texts(&f)[2], "14", "a click on the active segment too");
    }

    #[test]
    fn a_pending_single_digit_completes_at_commit() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        assert!(!f.digit(1));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.date(), d(2026, 1, 14));
        assert!(!f.segments()[1].typing);
        f.select(Segment::Day);
        assert!(!f.digit(2));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.date(), d(2026, 1, 2));
        assert_eq!(f.complete_pending(), Ok(()), "nothing pending is Ok");
        assert_eq!(f.date(), d(2026, 1, 2));
        // `3` waits in the day (30/31 possible) and completes as the 3rd.
        assert!(!f.digit(3));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.date(), d(2026, 1, 3));
    }

    #[test]
    fn a_pending_entry_that_cannot_complete_refuses_naming_the_segment() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.digit(0));
        assert_eq!(f.complete_pending(), Err(Segment::Day));
        assert_eq!(texts(&f)[2], "0", "refused, nothing changed");
        assert_eq!(f.date(), d(2026, 9, 14));
        f.select(Segment::Month);
        assert!(!f.digit(0));
        assert_eq!(f.complete_pending(), Err(Segment::Month));
        f.select(Segment::Year);
        assert!(!f.digit(2));
        assert!(!f.digit(0));
        assert_eq!(f.complete_pending(), Err(Segment::Year));
        assert_eq!(texts(&f)[0], "20");
        assert_eq!(f.date(), d(2026, 9, 14));
        assert_eq!(Segment::Year.name(), "year");
    }

    #[test]
    fn a_step_drops_partial_digits() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.digit(1));
        assert_eq!(texts(&f)[2], "1");
        f.step(1);
        assert_eq!(f.date(), d(2026, 9, 15));
        assert_eq!(texts(&f)[2], "15");
        assert!(!f.segments()[2].typing);
    }

    #[test]
    fn month_typing_waits_on_a_leading_one_and_completes_on_the_second_digit() {
        let mut f = DateTimeField::open(
            d(2026, 9, 17).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        assert!(!f.digit(1), "1 could still be 10, 11 or 12");
        let segs = f.segments();
        assert_eq!(
            (segs[1].text.as_str(), segs[1].active, segs[1].typing),
            ("1", true, true)
        );
        assert_eq!(f.date(), d(2026, 9, 17), "nothing is applied while typing");
        assert!(f.digit(2));
        assert_eq!(f.date(), d(2026, 12, 17));
        assert_eq!(
            f.segment(),
            Segment::Day,
            "a completed month advances to the day"
        );
        assert_eq!(texts(&f), ["2026", "12", "17"]);
    }

    #[test]
    fn a_first_month_digit_two_to_nine_completes_at_once() {
        let mut f = DateTimeField::open(
            d(2026, 9, 17).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        assert!(f.digit(3));
        assert_eq!(f.date(), d(2026, 3, 17));
        assert_eq!(f.segment(), Segment::Day);
    }

    #[test]
    fn a_second_month_digit_past_twelve_or_making_zero_is_refused() {
        let mut f = DateTimeField::open(
            d(2026, 9, 17).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        assert!(!f.digit(1));
        assert!(!f.digit(9), "19 is not a month");
        assert_eq!(texts(&f)[1], "1", "the first digit stays for another try");
        assert_eq!(f.segment(), Segment::Month);
        assert!(f.digit(0), "10 completes");
        assert_eq!(f.date(), d(2026, 10, 17));
        let mut f = DateTimeField::open(
            d(2026, 9, 17).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        assert!(!f.digit(0));
        assert!(!f.digit(0), "00 is not a month");
        assert!(f.digit(4), "04 is");
        assert_eq!(f.date(), d(2026, 4, 17));
    }

    #[test]
    fn a_typed_month_clamps_the_day() {
        let mut f = DateTimeField::open(
            d(2026, 1, 31).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Month);
        assert!(f.digit(2));
        assert_eq!(f.date(), d(2026, 2, 28));
    }

    #[test]
    fn day_typing_waits_on_zero_to_three_and_refuses_a_day_the_month_lacks() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.digit(3));
        assert_eq!(texts(&f)[2], "3");
        assert!(!f.digit(1), "31 September is refused");
        assert_eq!(texts(&f)[2], "3", "the 3 stays");
        assert!(f.digit(0), "30 completes");
        assert_eq!(f.date(), d(2026, 9, 30));
        assert_eq!(f.segment(), Segment::Day, "the day stays the day");
        assert!(!f.segments()[2].typing);
        assert!(!f.digit(0));
        assert!(!f.digit(0), "00 is not a day");
        assert!(f.digit(7));
        assert_eq!(f.date(), d(2026, 9, 7));
    }

    #[test]
    fn a_first_day_digit_four_to_nine_completes_as_that_day() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(f.digit(4));
        assert_eq!(f.date(), d(2026, 9, 4));
        assert!(f.digit(9));
        assert_eq!(
            f.date(),
            d(2026, 9, 9),
            "a typed digit replaces, never appends"
        );
    }

    #[test]
    fn year_typing_takes_four_digits_then_advances_to_the_month() {
        let mut f = DateTimeField::open(
            d(2024, 2, 29).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Year);
        assert!(!f.digit(2));
        assert!(!f.digit(0));
        assert!(!f.digit(2));
        assert_eq!(texts(&f)[0], "202");
        assert_eq!(f.date(), d(2024, 2, 29));
        assert!(f.digit(5));
        assert_eq!(
            f.date(),
            d(2025, 2, 28),
            "the day clamps to the new year's February"
        );
        assert_eq!(f.segment(), Segment::Month);
    }

    #[test]
    fn backspace_restores_the_committed_value() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.digit(2));
        assert_eq!(texts(&f)[2], "2");
        f.backspace();
        assert_eq!(texts(&f)[2], "14");
        assert!(!f.segments()[2].typing);
        assert_eq!(f.date(), d(2026, 9, 14));
        f.backspace();
        assert_eq!(texts(&f)[2], "14", "a second backspace changes nothing");
    }

    #[test]
    fn leaving_a_segment_drops_its_partial_digits() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.digit(2));
        f.left();
        assert_eq!(texts(&f), ["2026", "09", "14"]);
        assert!(!f.digit(1));
        f.select(Segment::Day);
        assert_eq!(texts(&f)[1], "09");
        assert_eq!(f.date(), d(2026, 9, 14));
    }

    #[test]
    fn value_ignores_partial_digits() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        f.select(Segment::Year);
        assert!(!f.digit(1));
        assert!(!f.digit(9));
        assert_eq!(f.date(), d(2026, 9, 14));
        assert_eq!(f.text(), "2026-09-14");
    }

    #[test]
    fn segment_index_and_at_round_trip() {
        for i in 0..6 {
            assert_eq!(Segment::at(i).unwrap().index(), i);
        }
        assert_eq!(Segment::at(6), None);
    }

    #[test]
    fn a_date_time_field_paints_six_segments_and_a_date_field_three() {
        let full =
            DateTimeField::open(dt(2026, 9, 18, 18, 0, 0), Precision::DateTime, Segment::Day);
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
        assert_eq!(
            date.segment(),
            Segment::Day,
            "Date precision ends at the day"
        );
        assert!(
            !date.select(Segment::Hour),
            "a segment past the precision is refused"
        );
        assert_eq!(date.segment(), Segment::Day);

        let mut full =
            DateTimeField::open(dt(2026, 9, 18, 0, 0, 0), Precision::DateTime, Segment::Day);
        full.right();
        assert_eq!(full.segment(), Segment::Hour);
        full.right();
        full.right();
        assert_eq!(full.segment(), Segment::Second);
        full.right();
        assert_eq!(
            full.segment(),
            Segment::Second,
            "DateTime precision ends at the second"
        );
        full.left();
        assert_eq!(full.segment(), Segment::Minute);
    }

    #[test]
    fn a_time_step_wraps_within_its_segment_without_carrying() {
        let mut f = DateTimeField::open(
            dt(2026, 9, 18, 23, 59, 59),
            Precision::DateTime,
            Segment::Hour,
        );
        f.step(1);
        assert_eq!(
            f.value(),
            dt(2026, 9, 18, 0, 59, 59),
            "23 ↑ is 00, the day unchanged"
        );
        f.step(-1);
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 59, 59));
        f.select(Segment::Minute);
        f.step(10);
        assert_eq!(f.value(), dt(2026, 9, 18, 23, 9, 59), "59 + 10 wraps to 09");
        f.select(Segment::Second);
        f.step(-60);
        assert_eq!(
            f.value(),
            dt(2026, 9, 18, 23, 9, 59),
            "a full turn is a no-op"
        );
    }

    #[test]
    fn hour_typing_refuses_a_second_digit_past_twenty_three() {
        let mut f = DateTimeField::open(
            dt(2026, 9, 18, 10, 0, 0),
            Precision::DateTime,
            Segment::Hour,
        );
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
        let mut f = DateTimeField::open(
            dt(2026, 9, 18, 10, 0, 0),
            Precision::DateTime,
            Segment::Minute,
        );
        assert!(!f.digit(4));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(
            f.value(),
            dt(2026, 9, 18, 10, 4, 0),
            "a lone 4 in the minute is :04"
        );
        assert!(
            !f.digit(0),
            "a lone 0 can stand alone too — 00 is a valid minute"
        );
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.value(), dt(2026, 9, 18, 10, 0, 0));
    }

    #[test]
    fn the_day_segment_still_advances_to_the_hour_under_date_time_and_stays_under_date() {
        let mut full =
            DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::DateTime, Segment::Day);
        assert!(full.digit(5), "5 completes as 05");
        assert_eq!(full.segment(), Segment::Hour);
        let mut date =
            DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::Date, Segment::Day);
        assert!(date.digit(5));
        assert_eq!(
            date.segment(),
            Segment::Day,
            "the panel's contract: the day stays the day"
        );
    }
}
