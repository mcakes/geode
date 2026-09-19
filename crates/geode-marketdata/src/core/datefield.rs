//! The segmented date field (header spec §5.2, 2026-09-19): the editor a
//! `Date` attribute opens in the strip instead of a text `Input`. Pure —
//! a date, an active segment and the digits typed into it this visit —
//! so the tile only routes keys here and paints what [`DateField::segments`]
//! answers. The value is a valid [`NaiveDate`] at every moment: a step
//! rolls, clamps or saturates, a digit that would make an impossible
//! segment is refused, and `enter` therefore has nothing to refuse.

use chrono::{Datelike, Duration, NaiveDate};

/// One of the three segments, in painted order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Year,
    Month,
    Day,
}

impl Segment {
    /// The segment's painted position: year 0, month 1, day 2.
    pub fn index(self) -> usize {
        match self {
            Segment::Year => 0,
            Segment::Month => 1,
            Segment::Day => 2,
        }
    }

    /// The segment's name as a notice spells it.
    pub fn name(self) -> &'static str {
        match self {
            Segment::Year => "year",
            Segment::Month => "month",
            Segment::Day => "day",
        }
    }

    /// The segment painted at `index`, `None` past the day.
    pub fn at(index: usize) -> Option<Self> {
        match index {
            0 => Some(Segment::Year),
            1 => Some(Segment::Month),
            2 => Some(Segment::Day),
            _ => None,
        }
    }

    fn left(self) -> Self {
        match self {
            Segment::Year | Segment::Month => Segment::Year,
            Segment::Day => Segment::Month,
        }
    }

    fn right(self) -> Self {
        match self {
            Segment::Year => Segment::Month,
            Segment::Month | Segment::Day => Segment::Day,
        }
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

/// The field's state: the committed date, the active segment, and the
/// digits typed into that segment since it became active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateField {
    pub date: NaiveDate,
    pub segment: Segment,
    /// Digits typed into the active segment this visit — empty once a
    /// segment completes, is left, or is backspaced. Never longer than
    /// the segment's own width, and only ever non-empty for `segment`.
    typed: String,
}

impl DateField {
    /// Open on `date` with the DAY segment active (user ruling
    /// 2026-09-19: the segment a trader changes most; `left` twice
    /// reaches the year).
    pub fn open(date: NaiveDate) -> Self {
        Self {
            date,
            segment: Segment::Day,
            typed: String::new(),
        }
    }

    /// Move to the segment on the left, clamped at the year. Leaving a
    /// segment drops its partial digits: the committed value shows again.
    pub fn left(&mut self) {
        self.select(self.segment.left());
    }

    /// Move to the segment on the right, clamped at the day.
    pub fn right(&mut self) {
        self.select(self.segment.right());
    }

    /// Make `segment` the active one (a click, or the arrows above). A
    /// re-select of the segment that is already active — a click on it,
    /// `left` at the year, `right` at the day — also drops its partial
    /// digits: every select is "start this segment afresh".
    pub fn select(&mut self, segment: Segment) {
        self.segment = segment;
        self.typed.clear();
    }

    /// Step the active segment by `n`: days roll over into the next
    /// month, months clamp the day to the new month's length, years clamp
    /// Feb 29 to Feb 28. Saturates at chrono's own bounds rather than
    /// panicking, and never leaves an invalid date. Drops partial digits.
    pub fn step(&mut self, n: i64) {
        self.typed.clear();
        let date = self.date;
        self.date = match self.segment {
            // `try_days`, never `days`: the latter PANICS past its own
            // bound, and the arm below is what turns an absurd `n` into a
            // saturated date instead.
            Segment::Day => Duration::try_days(n).and_then(|d| date.checked_add_signed(d)),
            Segment::Month => {
                // Counted in whole months from year 0 so a step of any
                // size crosses year ends in one arithmetic, then clamped
                // through the same door a year step and a typed month use.
                // `checked_add`: an `n` near `i64::MAX` would otherwise
                // overflow the count itself.
                let months = i64::from(date.year()) * 12 + i64::from(date.month() - 1);
                months.checked_add(n).and_then(|total| {
                    let month = total.rem_euclid(12) as u32 + 1;
                    i32::try_from(total.div_euclid(12))
                        .ok()
                        .and_then(|year| clamped_ymd(year, month, date.day()))
                })
            }
            Segment::Year => {
                let delta = i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX });
                let year = date.year().saturating_add(delta);
                clamped_ymd(year, date.month(), date.day())
            }
        }
        .unwrap_or(if n < 0 {
            NaiveDate::MIN
        } else {
            NaiveDate::MAX
        });
    }

    /// Type digit `d` into the active segment. A typed digit REPLACES the
    /// segment's value rather than appending to it. Answers whether the
    /// segment COMPLETED — its value applied (the day clamped if the month
    /// changed) and the next segment made active (the day stays the day).
    ///
    /// - Year: four digits complete.
    /// - Month: a first digit `2`–`9` completes as `0d` at once; `0`/`1`
    ///   waits for a second digit; a second digit making `00` or more than
    ///   `12` is refused (the first digit stays).
    /// - Day: a first digit `4`–`9` completes as `0d`; `0`–`3` waits; a
    ///   second digit making `00` or more than the month holds is refused.
    ///
    /// A digit left WAITING is not lost at `enter`: the commit runs
    /// [`Self::complete_pending`] first, so a lone `1` in the month
    /// commits as `01` and a lone `2` in the day as `02`.
    pub fn digit(&mut self, d: u8) -> bool {
        let d = d.min(9);
        match self.segment {
            Segment::Year => {
                self.typed.push(char::from(b'0' + d));
                if self.typed.len() < 4 {
                    return false;
                }
                let year = self.typed.parse::<i32>().unwrap_or(self.date.year());
                self.apply(clamped_ymd(year, self.date.month(), self.date.day()));
                self.select(Segment::Month);
                true
            }
            Segment::Month => {
                let value = match self.typed.as_str() {
                    "" if d >= 2 => u32::from(d),
                    "" => {
                        self.typed.push(char::from(b'0' + d));
                        return false;
                    }
                    first => {
                        let candidate = first.parse::<u32>().unwrap_or(0) * 10 + u32::from(d);
                        if candidate == 0 || candidate > 12 {
                            return false;
                        }
                        candidate
                    }
                };
                self.apply(clamped_ymd(self.date.year(), value, self.date.day()));
                self.select(Segment::Day);
                true
            }
            Segment::Day => {
                let value = match self.typed.as_str() {
                    "" if d >= 4 => u32::from(d),
                    "" => {
                        self.typed.push(char::from(b'0' + d));
                        return false;
                    }
                    first => {
                        let candidate = first.parse::<u32>().unwrap_or(0) * 10 + u32::from(d);
                        if candidate == 0 || candidate > days_in_month(self.date) {
                            return false;
                        }
                        candidate
                    }
                };
                self.apply(NaiveDate::from_ymd_opt(
                    self.date.year(),
                    self.date.month(),
                    value,
                ));
                self.select(Segment::Day);
                true
            }
        }
    }

    /// Finish whatever is still typed into the active segment, as a commit
    /// must before it reads [`Self::value`] (user ruling 2026-09-19): a
    /// trader who typed `1` in the day and pressed `enter` meant the 1st,
    /// not the day that was there before. A single waiting digit that
    /// can stand alone completes as `0d` (the browser rule: month `1` →
    /// `01`, day `2` → `02`); `Ok(())` too when nothing is pending. A
    /// pending entry that cannot complete — `0` in the month or day, a
    /// year of fewer than four digits — is `Err(segment)` with nothing
    /// changed, for the caller to refuse the commit and name the segment.
    pub fn complete_pending(&mut self) -> Result<(), Segment> {
        if self.typed.is_empty() {
            return Ok(());
        }
        let value = self.typed.parse::<u32>().unwrap_or(0);
        let completed = match self.segment {
            Segment::Year => None,
            Segment::Month if value >= 1 => clamped_ymd(self.date.year(), value, self.date.day()),
            Segment::Day if value >= 1 => {
                NaiveDate::from_ymd_opt(self.date.year(), self.date.month(), value)
            }
            Segment::Month | Segment::Day => None,
        };
        match completed {
            Some(date) => {
                self.date = date;
                self.typed.clear();
                Ok(())
            }
            None => Err(self.segment),
        }
    }

    /// Clear what was typed into the active segment; the committed value
    /// shows again.
    pub fn backspace(&mut self) {
        self.typed.clear();
    }

    /// The committed value as `YYYY-MM-DD`. Partial digits are not part of
    /// it — the field's value is always the last complete state.
    pub fn text(&self) -> String {
        self.date.format("%Y-%m-%d").to_string()
    }

    /// What each segment paints, year to day.
    pub fn segments(&self) -> [SegmentText; 3] {
        let typing = !self.typed.is_empty();
        let seg = |segment: Segment, value: String| SegmentText {
            text: if typing && segment == self.segment {
                self.typed.clone()
            } else {
                value
            },
            active: segment == self.segment,
            typing: typing && segment == self.segment,
        };
        [
            seg(Segment::Year, format!("{:04}", self.date.year())),
            seg(Segment::Month, format!("{:02}", self.date.month())),
            seg(Segment::Day, format!("{:02}", self.date.day())),
        ]
    }

    /// The committed date. Partial digits are not in it — which is why a
    /// commit calls [`Self::complete_pending`] first and reads this only
    /// on `Ok`; read raw, mid-typing, it is the date before the digit.
    pub fn value(&self) -> NaiveDate {
        self.date
    }

    /// Install a completed segment's date; `None` (a year past chrono's
    /// range, say) leaves the date as it was.
    fn apply(&mut self, date: Option<NaiveDate>) {
        if let Some(date) = date {
            self.date = date;
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

    fn texts(f: &DateField) -> [String; 3] {
        let [y, m, day] = f.segments();
        [y.text, m.text, day.text]
    }

    #[test]
    fn the_field_opens_on_the_day_segment_with_nothing_typed() {
        let f = DateField::open(d(2026, 9, 14));
        assert_eq!(f.segment, Segment::Day);
        assert_eq!(f.text(), "2026-09-14");
        let segs = f.segments();
        assert_eq!(texts(&f), ["2026", "09", "14"]);
        assert_eq!(
            segs.map(|s| (s.active, s.typing)),
            [(false, false), (false, false), (true, false)]
        );
    }

    #[test]
    fn left_and_right_clamp_at_the_year_and_the_day() {
        let mut f = DateField::open(d(2026, 9, 14));
        f.right();
        assert_eq!(f.segment, Segment::Day, "right at the day stays");
        f.left();
        assert_eq!(f.segment, Segment::Month);
        f.left();
        assert_eq!(f.segment, Segment::Year);
        f.left();
        assert_eq!(f.segment, Segment::Year, "left at the year stays");
        f.right();
        assert_eq!(f.segment, Segment::Month);
    }

    #[test]
    fn a_day_step_rolls_over_into_the_next_month() {
        let mut f = DateField::open(d(2026, 9, 30));
        f.step(1);
        assert_eq!(f.value(), d(2026, 10, 1));
        f.step(-1);
        assert_eq!(f.value(), d(2026, 9, 30));
        f.step(10);
        assert_eq!(f.value(), d(2026, 10, 10));
        let mut f = DateField::open(d(2026, 12, 31));
        f.step(1);
        assert_eq!(f.value(), d(2027, 1, 1), "and across a year end");
    }

    #[test]
    fn a_month_step_clamps_the_day_to_the_new_months_length() {
        let mut f = DateField::open(d(2026, 1, 31));
        f.select(Segment::Month);
        f.step(1);
        assert_eq!(f.value(), d(2026, 2, 28));
        let mut f = DateField::open(d(2024, 1, 31));
        f.select(Segment::Month);
        f.step(1);
        assert_eq!(f.value(), d(2024, 2, 29), "a leap February keeps its 29th");
        f.step(-1);
        assert_eq!(
            f.value(),
            d(2024, 1, 29),
            "a clamped day stays clamped on the way back"
        );
        let mut f = DateField::open(d(2026, 9, 17));
        f.select(Segment::Month);
        f.step(-1);
        assert_eq!(
            f.value(),
            d(2026, 8, 17),
            "the mockup: 09 → 08 with the day kept"
        );
        f.step(10);
        assert_eq!(f.value(), d(2027, 6, 17), "ten months crosses the year");
    }

    #[test]
    fn a_year_step_clamps_feb_29_to_the_28th() {
        let mut f = DateField::open(d(2024, 2, 29));
        f.select(Segment::Year);
        f.step(1);
        assert_eq!(f.value(), d(2025, 2, 28));
        f.step(-1);
        assert_eq!(
            f.value(),
            d(2024, 2, 28),
            "and does not un-clamp on the way back"
        );
        let mut f = DateField::open(d(2026, 9, 12));
        f.select(Segment::Year);
        f.step(1);
        assert_eq!(f.value(), d(2027, 9, 12));
    }

    #[test]
    fn a_step_saturates_at_chronos_bounds_rather_than_panicking() {
        let mut f = DateField::open(NaiveDate::MAX);
        f.step(1);
        assert_eq!(f.value(), NaiveDate::MAX);
        f.select(Segment::Year);
        f.step(i64::MAX);
        assert_eq!(f.value(), NaiveDate::MAX);
        f.select(Segment::Month);
        f.step(i64::MIN);
        assert_eq!(f.value(), NaiveDate::MIN);
        f.select(Segment::Day);
        f.step(-1);
        assert_eq!(f.value(), NaiveDate::MIN);
        // The two arithmetic overflows: the month COUNT itself, and
        // `Duration::days`' own bound (which panics where `try_days`
        // answers `None`).
        let mut f = DateField::open(d(2026, 9, 14));
        f.select(Segment::Month);
        f.step(i64::MAX);
        assert_eq!(f.value(), NaiveDate::MAX);
        let mut f = DateField::open(d(2026, 9, 14));
        f.step(i64::MAX);
        assert_eq!(f.value(), NaiveDate::MAX);
        f.step(i64::MIN);
        assert_eq!(f.value(), NaiveDate::MIN);
    }

    #[test]
    fn a_reselect_of_the_active_segment_drops_partial_digits() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(!f.digit(2));
        f.right();
        assert_eq!(texts(&f)[2], "14", "right at the day re-selects it");
        assert!(!f.digit(2));
        f.select(Segment::Day);
        assert_eq!(texts(&f)[2], "14", "a click on the active segment too");
    }

    #[test]
    fn a_pending_single_digit_completes_at_commit() {
        let mut f = DateField::open(d(2026, 9, 14));
        f.select(Segment::Month);
        assert!(!f.digit(1));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.value(), d(2026, 1, 14));
        assert!(!f.segments()[1].typing);
        f.select(Segment::Day);
        assert!(!f.digit(2));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.value(), d(2026, 1, 2));
        assert_eq!(f.complete_pending(), Ok(()), "nothing pending is Ok");
        assert_eq!(f.value(), d(2026, 1, 2));
        // `3` waits in the day (30/31 possible) and completes as the 3rd.
        assert!(!f.digit(3));
        assert_eq!(f.complete_pending(), Ok(()));
        assert_eq!(f.value(), d(2026, 1, 3));
    }

    #[test]
    fn a_pending_entry_that_cannot_complete_refuses_naming_the_segment() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(!f.digit(0));
        assert_eq!(f.complete_pending(), Err(Segment::Day));
        assert_eq!(texts(&f)[2], "0", "refused, nothing changed");
        assert_eq!(f.value(), d(2026, 9, 14));
        f.select(Segment::Month);
        assert!(!f.digit(0));
        assert_eq!(f.complete_pending(), Err(Segment::Month));
        f.select(Segment::Year);
        assert!(!f.digit(2));
        assert!(!f.digit(0));
        assert_eq!(f.complete_pending(), Err(Segment::Year));
        assert_eq!(texts(&f)[0], "20");
        assert_eq!(f.value(), d(2026, 9, 14));
        assert_eq!(Segment::Year.name(), "year");
    }

    #[test]
    fn a_step_drops_partial_digits() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(!f.digit(1));
        assert_eq!(texts(&f)[2], "1");
        f.step(1);
        assert_eq!(f.value(), d(2026, 9, 15));
        assert_eq!(texts(&f)[2], "15");
        assert!(!f.segments()[2].typing);
    }

    #[test]
    fn month_typing_waits_on_a_leading_one_and_completes_on_the_second_digit() {
        let mut f = DateField::open(d(2026, 9, 17));
        f.select(Segment::Month);
        assert!(!f.digit(1), "1 could still be 10, 11 or 12");
        let segs = f.segments();
        assert_eq!(
            (segs[1].text.as_str(), segs[1].active, segs[1].typing),
            ("1", true, true)
        );
        assert_eq!(f.value(), d(2026, 9, 17), "nothing is applied while typing");
        assert!(f.digit(2));
        assert_eq!(f.value(), d(2026, 12, 17));
        assert_eq!(
            f.segment,
            Segment::Day,
            "a completed month advances to the day"
        );
        assert_eq!(texts(&f), ["2026", "12", "17"]);
    }

    #[test]
    fn a_first_month_digit_two_to_nine_completes_at_once() {
        let mut f = DateField::open(d(2026, 9, 17));
        f.select(Segment::Month);
        assert!(f.digit(3));
        assert_eq!(f.value(), d(2026, 3, 17));
        assert_eq!(f.segment, Segment::Day);
    }

    #[test]
    fn a_second_month_digit_past_twelve_or_making_zero_is_refused() {
        let mut f = DateField::open(d(2026, 9, 17));
        f.select(Segment::Month);
        assert!(!f.digit(1));
        assert!(!f.digit(9), "19 is not a month");
        assert_eq!(texts(&f)[1], "1", "the first digit stays for another try");
        assert_eq!(f.segment, Segment::Month);
        assert!(f.digit(0), "10 completes");
        assert_eq!(f.value(), d(2026, 10, 17));
        let mut f = DateField::open(d(2026, 9, 17));
        f.select(Segment::Month);
        assert!(!f.digit(0));
        assert!(!f.digit(0), "00 is not a month");
        assert!(f.digit(4), "04 is");
        assert_eq!(f.value(), d(2026, 4, 17));
    }

    #[test]
    fn a_typed_month_clamps_the_day() {
        let mut f = DateField::open(d(2026, 1, 31));
        f.select(Segment::Month);
        assert!(f.digit(2));
        assert_eq!(f.value(), d(2026, 2, 28));
    }

    #[test]
    fn day_typing_waits_on_zero_to_three_and_refuses_a_day_the_month_lacks() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(!f.digit(3));
        assert_eq!(texts(&f)[2], "3");
        assert!(!f.digit(1), "31 September is refused");
        assert_eq!(texts(&f)[2], "3", "the 3 stays");
        assert!(f.digit(0), "30 completes");
        assert_eq!(f.value(), d(2026, 9, 30));
        assert_eq!(f.segment, Segment::Day, "the day stays the day");
        assert!(!f.segments()[2].typing);
        assert!(!f.digit(0));
        assert!(!f.digit(0), "00 is not a day");
        assert!(f.digit(7));
        assert_eq!(f.value(), d(2026, 9, 7));
    }

    #[test]
    fn a_first_day_digit_four_to_nine_completes_as_that_day() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(f.digit(4));
        assert_eq!(f.value(), d(2026, 9, 4));
        assert!(f.digit(9));
        assert_eq!(
            f.value(),
            d(2026, 9, 9),
            "a typed digit replaces, never appends"
        );
    }

    #[test]
    fn year_typing_takes_four_digits_then_advances_to_the_month() {
        let mut f = DateField::open(d(2024, 2, 29));
        f.select(Segment::Year);
        assert!(!f.digit(2));
        assert!(!f.digit(0));
        assert!(!f.digit(2));
        assert_eq!(texts(&f)[0], "202");
        assert_eq!(f.value(), d(2024, 2, 29));
        assert!(f.digit(5));
        assert_eq!(
            f.value(),
            d(2025, 2, 28),
            "the day clamps to the new year's February"
        );
        assert_eq!(f.segment, Segment::Month);
    }

    #[test]
    fn backspace_restores_the_committed_value() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(!f.digit(2));
        assert_eq!(texts(&f)[2], "2");
        f.backspace();
        assert_eq!(texts(&f)[2], "14");
        assert!(!f.segments()[2].typing);
        assert_eq!(f.value(), d(2026, 9, 14));
        f.backspace();
        assert_eq!(texts(&f)[2], "14", "a second backspace changes nothing");
    }

    #[test]
    fn leaving_a_segment_drops_its_partial_digits() {
        let mut f = DateField::open(d(2026, 9, 14));
        assert!(!f.digit(2));
        f.left();
        assert_eq!(texts(&f), ["2026", "09", "14"]);
        assert!(!f.digit(1));
        f.select(Segment::Day);
        assert_eq!(texts(&f)[1], "09");
        assert_eq!(f.value(), d(2026, 9, 14));
    }

    #[test]
    fn value_ignores_partial_digits() {
        let mut f = DateField::open(d(2026, 9, 14));
        f.select(Segment::Year);
        assert!(!f.digit(1));
        assert!(!f.digit(9));
        assert_eq!(f.value(), d(2026, 9, 14));
        assert_eq!(f.text(), "2026-09-14");
    }

    #[test]
    fn segment_index_and_at_round_trip() {
        for i in 0..3 {
            assert_eq!(Segment::at(i).unwrap().index(), i);
        }
        assert_eq!(Segment::at(3), None);
    }
}
