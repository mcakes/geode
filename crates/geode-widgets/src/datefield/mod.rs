//! Segmented date and date-time editing with separate state, key routing, and
//! painting. Hosts retain [`DateTimeField`], route keys through [`route`], and
//! prepare display text with [`DateTimeField::segments`].
//!
//! The stored [`NaiveDateTime`] is always valid. Partial digits remain separate
//! until completed; a host must call [`DateTimeField::complete_pending`] before
//! committing and handle an incomplete segment. The host owns persistence,
//! cancellation, focus, and any timezone conversion or domain validation.

mod paint;

pub use paint::{SegmentPaint, paint};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use gpui::SharedString;

/// Visible and editable segments: year/month/day, or those three plus
/// hour/minute/second. Date precision preserves the value's hidden time.
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

/// Prepared segment text and its active/typing state. `text` is a
/// [`SharedString`] so a painter can clone cached text without formatting it.
/// [`DateTimeField::segments`] allocates this presentation; the host owns caching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentText {
    pub text: SharedString,
    pub active: bool,
    pub typing: bool,
}

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

/// Map arrows to navigation or stepping, digits to entry, Backspace to clearing
/// pending digits, and Enter/Escape to host-owned commit/cancel requests. Shift
/// changes the step from one to ten. `chord` means Ctrl, Alt, or Cmd is held;
/// those keys, Tab, and unrecognized keys return `None` for the host to handle.
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

/// Host-owned editing state: a valid value, precision, active segment, and
/// pending digits. Completed edits update this value immediately; committing it
/// to the application and restoring an earlier value on cancel belong to the host.
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
    /// Start editing `value` at `segment` with no pending digits. A segment outside
    /// `precision` falls back to its last visible segment. The host chooses the
    /// initial segment; the supplied date and time are preserved.
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

    /// Whether the active segment has pending digits. Its display then shows
    /// those digits instead of the stored value. Hosts with digit shortcuts can
    /// use this to distinguish a new entry from continuation of an existing one.
    pub fn typing(&self) -> bool {
        !self.typed.is_empty()
    }

    /// Move to the segment on the left, clamped at the year. Leaving a
    /// segment drops its partial digits: the stored value shows again.
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

    /// Step the active segment by `n` and discard pending digits. Days cross
    /// month and year boundaries; month and year steps clamp the day to the
    /// resulting month's length. Date arithmetic saturates at chrono's bounds.
    /// Time segments wrap within their own range without carrying: stepping the
    /// hour from 23 to 00 leaves the date, minute, and second unchanged.
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

    /// Enter a digit into the active segment, clamping `d` to 9. A new entry
    /// replaces the segment's stored value; pending digits build that replacement.
    /// Return `true` when the segment completes and its value is applied. Completion
    /// advances to the next visible segment, staying put at the last one. Month
    /// and year changes clamp the day to the resulting month's length.
    ///
    /// - Year: four digits complete.
    /// - Month: a first digit `2`–`9` completes immediately; `0`/`1` waits.
    ///   A second digit making `00` or more than `12` is refused.
    /// - Day: a first digit `4`–`9` completes immediately; `0`–`3` waits.
    ///   A second digit making `00` or exceeding the month's length is refused.
    /// - Hour: `3`–`9` completes immediately; `0`–`2` waits.
    ///   A second digit exceeding `23` is refused.
    /// - Minute and second: `6`–`9` completes immediately; `0`–`5` waits.
    ///   A second digit exceeding `59` is refused.
    ///
    /// `false` means entry is pending or the digit was refused; a refused digit
    /// leaves the previous pending digits intact. Before committing, the host
    /// calls [`Self::complete_pending`] to resolve a remaining partial entry.
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

    /// Complete pending digits before the host reads [`Self::value`] for commit.
    /// A single nonzero digit completes a month or day; a single digit including
    /// zero completes a time segment. Success clears pending digits without moving
    /// the active segment. Nothing pending also returns `Ok(())`.
    ///
    /// Zero in a month or day, or fewer than four year digits, returns
    /// `Err(segment)` without changing the field. The host keeps the editor open
    /// and reports the incomplete segment.
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

    /// Clear what was typed into the active segment; the stored value
    /// shows again.
    pub fn backspace(&mut self) {
        self.typed.clear();
    }

    /// The stored value: `YYYY-MM-DD` under `Date`, `YYYY-MM-DD
    /// HH:MM:SS` under `DateTime`. Partial digits are not part of it.
    pub fn text(&self) -> String {
        match self.precision {
            Precision::Date => self.value.format("%Y-%m-%d").to_string(),
            Precision::DateTime => self.value.format("%Y-%m-%d %H:%M:%S").to_string(),
        }
    }

    /// Allocate display text for each visible segment in year-first order.
    /// The active segment shows pending digits when present. Hosts should prepare
    /// and cache this after state changes for reuse by the painter.
    pub fn segments(&self) -> Vec<SegmentText> {
        let typing = !self.typed.is_empty();
        let v = self.value;
        Segment::ALL
            .iter()
            .copied()
            .filter(|s| s.fits(self.precision))
            .map(|segment| {
                let committed: SharedString = match segment {
                    Segment::Year => format!("{:04}", v.year()).into(),
                    Segment::Month => format!("{:02}", v.month()).into(),
                    Segment::Day => format!("{:02}", v.day()).into(),
                    Segment::Hour => format!("{:02}", v.hour()).into(),
                    Segment::Minute => format!("{:02}", v.minute()).into(),
                    Segment::Second => format!("{:02}", v.second()).into(),
                };
                let active = segment == self.segment;
                SegmentText {
                    text: if typing && active {
                        self.typed.clone().into()
                    } else {
                        committed
                    },
                    active,
                    typing: typing && active,
                }
            })
            .collect()
    }

    /// The stored value. Partial digits are not in it — which is why a
    /// commit calls [`Self::complete_pending`] first and reads this only
    /// on `Ok`.
    pub fn value(&self) -> NaiveDateTime {
        self.value
    }

    /// The stored date — the `Date` precision's whole answer.
    pub fn date(&self) -> NaiveDate {
        self.value.date()
    }

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

/// Construct a date, clamping the day to the month's length. Return `None`
/// for an out-of-range year, invalid month, or zero day. Callers supply valid
/// months and nonzero days, so only the year range can fail on those paths.
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
        [
            v[0].text.to_string(),
            v[1].text.to_string(),
            v[2].text.to_string(),
        ]
    }

    fn texts_of(f: &DateTimeField) -> Vec<String> {
        f.segments()
            .into_iter()
            .map(|s| s.text.to_string())
            .collect()
    }

    /// Hosts can distinguish a pending segment entry from a new digit shortcut.
    #[test]
    fn typing_reports_the_digits_pending_in_the_active_segment() {
        let mut f = DateTimeField::open(
            d(2026, 9, 14).and_hms_opt(0, 0, 0).unwrap(),
            Precision::Date,
            Segment::Day,
        );
        assert!(!f.typing(), "nothing typed on open");
        // A leading `1` waits for a possible second digit. It can also complete
        // as day 1 when the host calls `complete_pending`.
        assert!(!f.digit(1), "the day waits");
        assert!(f.typing());
        f.backspace();
        assert!(!f.typing(), "backspace drops the pending digits");
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

        assert_eq!(
            DateTimeField::open(dt(2026, 9, 18, 0, 0, 0), Precision::Date, Segment::Hour).segment(),
            Segment::Day,
            "open falls back to the precision's last segment"
        );
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
        assert_eq!(
            full.value(),
            dt(2026, 9, 5, 10, 0, 0),
            "a typed day preserves the time"
        );
        assert_eq!(full.segment(), Segment::Hour);
        full.select(Segment::Day);
        full.step(1);
        assert_eq!(
            full.value(),
            dt(2026, 9, 6, 10, 0, 0),
            "a date-segment step preserves the time"
        );
        let mut date =
            DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::Date, Segment::Day);
        assert!(date.digit(5));
        assert_eq!(
            date.segment(),
            Segment::Day,
            "the panel's contract: the day stays the day"
        );
    }

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
        assert_eq!(
            route("a", false, false),
            None,
            "a letter is not the field's"
        );
        assert_eq!(route("tab", false, false), None, "tab belongs to the host");
        assert_eq!(
            route("up", false, true),
            None,
            "a chord falls through to the shell"
        );
        assert_eq!(route("7", false, true), None);
    }

    #[test]
    fn apply_performs_the_field_arms_and_reports_a_change() {
        let mut f =
            DateTimeField::open(dt(2026, 9, 18, 10, 0, 0), Precision::DateTime, Segment::Day);
        assert!(f.apply(FieldKey::Step(1)));
        assert_eq!(f.date(), d(2026, 9, 19));
        assert!(f.apply(FieldKey::Right));
        assert_eq!(f.segment(), Segment::Hour);
        assert!(
            f.apply(FieldKey::Digit(1)),
            "a waiting digit is a change too"
        );
        assert!(f.apply(FieldKey::Backspace));
        assert!(
            !f.apply(FieldKey::Backspace),
            "nothing typed: nothing changed"
        );
        assert!(f.apply(FieldKey::Left));
        assert!(
            !f.apply(FieldKey::Commit),
            "commit and cancel are the host's"
        );
        assert!(!f.apply(FieldKey::Cancel));
    }
}
