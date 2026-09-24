//! The as-of dialog's pure row model. Sections stay in this order: Current
//! and Live while pinned, business-day presets, Custom, then recent publishes.
//! Each section ranks labels independently; the right-hand timestamps are
//! not searchable. `asof_view` owns rendering and modal input.

use chrono::{DateTime, Timelike, Utc};

use geode_core::clock::{Clock, Preset, presets};
use geode_core::query::AsOf;
use geode_widgets::datefield::{DateTimeField, Precision, Segment};

use crate::frame::Publish;
use crate::listfilter;
use crate::vimnav::{self, NavCommand};

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

/// Rows, preset instants, publish instants, and the pin rebuilt together.
type RebuiltRows = (
    Vec<Entry>,
    Vec<Preset>,
    Vec<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

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
        let (entries, presets, instants, pinned) = Self::rebuild(as_of, publishes, clock, now);
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

    /// Build the same row and instant mappings for opening and refreshing.
    fn rebuild(
        as_of: &AsOf,
        publishes: &[Publish],
        clock: Clock,
        now: DateTime<Utc>,
    ) -> RebuiltRows {
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
                    "{} / {} \u{b7} {} book{}",
                    p.dataset,
                    p.batch,
                    p.books,
                    if p.books == 1 { "" } else { "s" }
                ),
                right: clock.local(p.at).format("%a %H:%M:%S").to_string(),
                section: Section::Publishes,
            });
        }
        (entries, presets, instants, pinned)
    }

    /// Refresh rows using the clock captured at open, retaining the query and
    /// Custom field edit state. Restore the highlight by displayed
    /// `(section, label, right)`, falling back to row 0 if it disappears.
    /// Including the formatted timestamp distinguishes many repeated publish
    /// labels, but instants with identical display text still share an identity.
    pub fn refresh(&mut self, as_of: &AsOf, publishes: &[Publish], now: DateTime<Utc>) {
        let previous = self
            .ranked
            .get(self.highlighted)
            .map(|p| (p.section, p.label.clone(), p.right.clone()));
        let (entries, presets, instants, pinned) = Self::rebuild(as_of, publishes, self.clock, now);
        self.entries = entries;
        self.presets = presets;
        self.publishes = instants;
        self.pinned = pinned;
        self.now = now;
        self.rerank();
        if let Some((section, label, right)) = previous
            && let Some(i) = self
                .ranked
                .iter()
                .position(|p| p.section == section && p.label == label && p.right == right)
        {
            self.highlighted = i;
        }
    }

    /// Rank labels within each fixed section; an empty query keeps input order.
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
            let texts: Vec<String> = members
                .iter()
                .map(|i| self.entries[*i].label.clone())
                .collect();
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

    /// `1`–`5` with an empty query commits that preset. Return `None` for a
    /// nonempty query, `0`, a non-digit, or a digit beyond the preset list.
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
    /// instant, else the pinned instant, else `now`, truncated to the
    /// second; on the day segment; and move the highlight onto the
    /// Custom row. The query is cleared so the Custom row is always
    /// painted while the field is open.
    pub fn open_field(&mut self) {
        let seed = self
            .ranked
            .get(self.highlighted)
            .and_then(|p| self.instant_of(&p.row))
            .or(self.pinned)
            .unwrap_or(self.now)
            // The field exposes seconds but no fractional segment. Truncate the seed
            // so committing an untouched field cannot retain invisible nanoseconds.
            .with_nanosecond(0)
            .expect("0 is always a valid nanosecond value");
        let local = self.clock.local(seed).naive_local();
        self.field = Some(DateTimeField::open(
            local,
            Precision::DateTime,
            Segment::Day,
        ));
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

    /// Get the field for editing and clear any previous commit refusal.
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

    /// The `now` `build` (or the last [`refresh`](Self::refresh)) was
    /// given — for a painter that needs a "current moment" without
    /// paying for a fresh `Utc::now()` on every render (the Custom
    /// row's zone-abbreviation suffix).
    pub fn now(&self) -> DateTime<Utc> {
        self.now
    }

    /// Commit the open field after completing its pending segment and resolving
    /// local time on the captured clock. Incomplete segments and DST gaps leave
    /// a refusal for the row to paint. Without a field, commit the highlighted
    /// instant or Live; no selection and an unopened Custom row are errors.
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

/// The CHILD index `scroll_to_item` must use for the row at
/// `highlighted`, on the `as-of-rows` list `asof_view::build` paints:
/// the eyebrow `div`s ahead of each section's first row (`Section::
/// eyebrow`) are themselves children of that same list, so a painted
/// row's child index is its own painted index plus one eyebrow for
/// every section boundary already crossed by the time it is reached.
pub fn child_index_of(painted: &[Painted], highlighted: usize) -> usize {
    let mut eyebrows = 0;
    let mut last: Option<Section> = None;
    for p in painted.iter().take(highlighted + 1) {
        if last != Some(p.section) {
            if p.section.eyebrow().is_some() {
                eyebrows += 1;
            }
            last = Some(p.section);
        }
    }
    highlighted + eyebrows
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use geode_core::clock::Clock;

    fn publish(dataset: &str, batch: &str, books: usize, at: DateTime<Utc>) -> Publish {
        Publish {
            dataset: dataset.into(),
            batch: batch.into(),
            books,
            at,
        }
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
        let pubs = [publish(
            "risk",
            "EOD",
            12,
            now() - chrono::Duration::hours(1),
        )];
        let s = state(AsOf::Live, &pubs);
        assert_eq!(
            labels(&s),
            [
                "EOD T-1",
                "SOD T",
                "EOD T-2",
                "EOD T-3",
                "EOD T-5",
                "custom",
                "risk / EOD \u{b7} 12 books"
            ]
        );
        assert_eq!(s.painted()[0].right, "Fri 18 Sep 18:00");
        assert_eq!(s.painted()[6].right, "Mon 09:42:00");
        assert_eq!(
            s.highlighted(),
            0,
            "the first preset is highlighted on open"
        );
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
            publish(
                "greeks",
                "INTRADAY",
                12,
                now() - chrono::Duration::minutes(5),
            ),
        ];
        let mut s = state(AsOf::Live, &pubs);
        assert!(s.set_query("eod"));
        let l = labels(&s);
        assert!(l.iter().all(|x| x.to_lowercase().contains("eod")), "{l:?}");
        assert!(!l.contains(&"custom".to_string()));
        assert!(!l.contains(&"SOD T".to_string()));
        assert_eq!(
            l.last().unwrap(),
            "risk / EOD \u{b7} 12 books",
            "publishes stay after presets"
        );
        assert!(
            !s.painted()[0].indices.is_empty(),
            "match glyphs are carried for painting"
        );
        assert!(!s.set_query("eod"), "an unchanged query is a no-op");
    }

    #[test]
    fn the_right_column_is_never_matched() {
        let mut s = state(AsOf::Live, &[]);
        s.set_query("18");
        assert!(
            s.painted().is_empty(),
            "'18' is in every preset's right column, in no label"
        );
    }

    #[test]
    fn a_digit_jumps_only_on_an_empty_query() {
        let mut s = state(AsOf::Live, &[]);
        assert!(
            matches!(s.painted()[1].row, Row::Preset(1)),
            "row 2 is the second preset"
        );
        let sod_t = Clock::utc()
            .sod_of(chrono::NaiveDate::from_ymd_opt(2026, 9, 21).unwrap())
            .unwrap();
        assert_eq!(s.jump_digit("2"), Some(Commit::At(sod_t)), "2 is SOD T");
        assert_eq!(s.jump_digit("9"), None, "past the painted presets is inert");
        assert_eq!(s.jump_digit("0"), None);
        s.set_query("e");
        assert_eq!(
            s.jump_digit("1"),
            None,
            "with text typed, a digit is a filter character"
        );
    }

    #[test]
    fn tab_seeds_the_field_from_the_highlighted_row_or_the_pin_or_now() {
        let mut s = state(AsOf::Live, &[]);
        s.nav(NavCommand::Move(1)); // SOD T
        s.open_field();
        assert!(matches!(s.painted()[s.highlighted()].row, Row::Custom));
        assert_eq!(
            s.field().unwrap().text(),
            "2026-09-21 08:00:00",
            "seeded from SOD T"
        );
        s.close_field();
        assert!(s.field().is_none());

        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let mut s = state(AsOf::At(pinned), &[]);
        s.set_highlighted(0); // Current
        s.open_field();
        assert_eq!(
            s.field().unwrap().text(),
            "2026-09-18 16:00:00",
            "Current seeds the pin"
        );

        let mut s = state(AsOf::Live, &[]);
        s.set_query("custom");
        s.open_field();
        assert!(s.query().is_empty(), "opening the field clears the query");
        assert_eq!(
            s.field().unwrap().text(),
            "2026-09-21 10:42:00",
            "no instant on the row: now"
        );
        assert_eq!(s.field().unwrap().segment(), Segment::Day);

        // A field with no fractional segment must discard hidden nanoseconds,
        // even when committed without any segment edit.
        let now_with_ns = now()
            .with_nanosecond(123_456_789)
            .expect("a valid nanosecond value");
        let mut s = AsOfState::build(&AsOf::Live, &[], Clock::utc(), now_with_ns);
        s.set_query("custom");
        s.open_field();
        assert_eq!(
            s.field().unwrap().value().nanosecond(),
            0,
            "the seed must be truncated to the second"
        );
    }

    #[test]
    fn commit_answers_the_highlighted_row_and_the_open_fields_value() {
        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let mut s = state(AsOf::At(pinned), &[]);
        s.set_highlighted(1);
        assert_eq!(s.commit().unwrap(), Commit::Live);
        s.set_highlighted(2);
        assert_eq!(
            s.commit().unwrap(),
            Commit::At(
                Clock::utc()
                    .eod_of(chrono::NaiveDate::from_ymd_opt(2026, 9, 18).unwrap())
                    .unwrap()
            )
        );
        s.open_field();
        s.field_mut().unwrap().step(1); // day +1
        assert_eq!(
            s.commit().unwrap(),
            Commit::At(Utc.with_ymd_and_hms(2026, 9, 19, 18, 0, 0).unwrap())
        );
    }

    #[test]
    fn a_field_value_in_a_dst_gap_is_refused_and_named_on_the_row() {
        let ny = Clock::in_zone_named("America/New_York");
        let mut s = AsOfState::build(
            &AsOf::Live,
            &[],
            ny,
            Utc.with_ymd_and_hms(2026, 3, 9, 15, 0, 0).unwrap(),
        );
        s.set_query("custom");
        s.open_field();
        let f = s.field_mut().unwrap();
        // The field seeds from `now` converted to NY local time: 2026-03-09
        // 15:00 UTC is already EDT (UTC-4; NY's spring-forward is the day
        // before, 2026-03-08), so the seeded hour is 11, not 15.
        // 2026-03-08 02:30 New York does not exist.
        f.select(Segment::Day);
        f.step(-1);
        f.select(Segment::Hour);
        f.step(-9); // 11 -> 02
        f.select(Segment::Minute);
        f.step(30);
        let err = s.commit().unwrap_err();
        assert!(err.contains("does not name a valid local time"), "{err}");
        assert_eq!(s.field_refusal(), Some(err.as_str()));
        assert!(s.field().is_some(), "the field stays open");
    }

    #[test]
    fn child_index_of_counts_the_eyebrows_before_the_highlighted_row() {
        let pubs = [publish(
            "risk",
            "EOD",
            12,
            now() - chrono::Duration::hours(1),
        )];
        let s = state(AsOf::Live, &pubs);
        // Under live: Presets, Custom, Publishes each paint an eyebrow —
        // 3 eyebrows ahead of the one publish row.
        let last = s.painted().len() - 1;
        assert!(matches!(s.painted()[last].section, Section::Publishes));
        assert_eq!(child_index_of(s.painted(), last), last + 3);
        // Row 0 (the first preset) sits right after ITS OWN section's
        // eyebrow — one eyebrow ahead of it, not zero.
        assert_eq!(child_index_of(s.painted(), 0), 1);
    }

    #[test]
    fn refresh_keeps_the_query_and_the_highlighted_row_and_shows_a_new_publish() {
        let mut s = state(AsOf::Live, &[]);
        assert!(s.set_query("eod"));
        assert_eq!(s.painted()[0].label, "EOD T-1");
        s.nav(NavCommand::Move(1)); // EOD T-2
        assert_eq!(s.painted()[s.highlighted()].label, "EOD T-2");

        let pubs = [publish("risk", "EOD", 12, now())];
        s.refresh(&AsOf::Live, &pubs, now());

        assert_eq!(s.query(), "eod", "refresh keeps the live query");
        assert_eq!(
            s.painted()[s.highlighted()].label,
            "EOD T-2",
            "refresh keeps the highlighted row by identity"
        );
        assert!(
            s.painted()
                .iter()
                .any(|p| p.label == "risk / EOD \u{b7} 12 books"),
            "the new publish is painted: {:?}",
            s.painted()
        );
    }

    #[test]
    fn refresh_identity_includes_right_so_same_labelled_publishes_dont_hop() {
        // Two publishes with the IDENTICAL label (same dataset/batch/
        // books) but different instants, so their `right` (formatted
        // timestamp) columns differ. Listed later-first, matching
        // `Frame::recent_publishes`' own newest-first order — with
        // `label` alone as the identity, `refresh`'s `.position()` would
        // always land on the FIRST painted match (`Row::Publish(0)`,
        // the later one), hopping the highlight off whichever of the
        // two was actually selected.
        let earlier = now() - chrono::Duration::hours(2);
        let later = now() - chrono::Duration::hours(1);
        let pubs = [
            publish("risk", "EOD", 12, later),
            publish("risk", "EOD", 12, earlier),
        ];
        let mut s = state(AsOf::Live, &pubs);
        let earlier_row = s
            .painted()
            .iter()
            .position(|p| matches!(p.row, Row::Publish(1)))
            .expect("the earlier (second) publish is painted");
        s.set_highlighted(earlier_row);
        let expected_right = s.painted()[earlier_row].right.clone();
        let expected_label = s.painted()[earlier_row].label.clone();

        // Refresh with the SAME two publishes (nothing actually
        // changed) — a pure re-rank-and-restore, so any hop here is
        // `refresh`'s own identity logic, not new data.
        s.refresh(&AsOf::Live, &pubs, now());

        let after = &s.painted()[s.highlighted()];
        assert_eq!(after.label, expected_label);
        assert_eq!(
            after.right, expected_right,
            "the highlight must stay on the EARLIER publish, not hop to \
             the later same-labelled one"
        );
    }

    #[test]
    fn refresh_leaves_an_open_field_untouched() {
        let mut s = state(AsOf::Live, &[]);
        s.open_field();
        s.field_mut().unwrap().step(1); // day +1, a deliberate edit
        let edited = s.field().unwrap().value();

        let pubs = [publish("risk", "EOD", 12, now())];
        s.refresh(&AsOf::Live, &pubs, now());

        assert_eq!(
            s.field().unwrap().value(),
            edited,
            "a data refresh must not touch the Custom field's own in-progress edit"
        );
        assert_eq!(s.field_refusal(), None);
    }

    #[test]
    fn refresh_falls_back_to_zero_when_the_highlighted_row_is_gone() {
        // Pinned, highlighted on `current` (row 0); refresh to Live drops
        // both `current` and `live` — nothing to find by identity.
        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let mut s = state(AsOf::At(pinned), &[]);
        assert_eq!(s.highlighted(), 0);
        s.refresh(&AsOf::Live, &[], now());
        assert_eq!(s.highlighted(), 0);
        assert_eq!(s.painted()[0].label, "EOD T-1");
    }

    /// Live has no instant, so opening Custom from it uses the pin before now.
    #[test]
    fn tab_from_the_live_row_while_pinned_seeds_the_pin() {
        let pinned = Utc.with_ymd_and_hms(2026, 9, 18, 16, 0, 0).unwrap();
        let mut s = state(AsOf::At(pinned), &[]);
        assert!(s.set_highlighted(1));
        assert!(matches!(s.painted()[1].row, Row::Live));
        s.open_field();
        assert_eq!(
            s.field().unwrap().text(),
            "2026-09-18 16:00:00",
            "Live has no instant of its own — tab seeds the pin"
        );
    }

    /// A highlighted publish commits its own instant.
    #[test]
    fn commit_on_a_highlighted_publish_row_equals_that_publishs_at() {
        let at = now() - chrono::Duration::hours(3);
        let pubs = [publish("risk", "EOD", 12, at)];
        let mut s = state(AsOf::Live, &pubs);
        let row = s
            .painted()
            .iter()
            .position(|p| matches!(p.row, Row::Publish(0)))
            .expect("the publish row is painted");
        assert!(s.set_highlighted(row));
        assert_eq!(s.commit().unwrap(), Commit::At(at));
    }

    /// Custom requires an open field before it can commit.
    #[test]
    fn commit_on_custom_with_no_field_open_is_an_error() {
        let mut s = state(AsOf::Live, &[]);
        let custom_row = s
            .painted()
            .iter()
            .position(|p| matches!(p.row, Row::Custom))
            .expect("the custom row is painted");
        assert!(s.set_highlighted(custom_row));
        assert!(s.commit().is_err());
    }

    /// An out-of-range selection leaves the highlight unchanged.
    #[test]
    fn set_highlighted_out_of_range_is_refused() {
        let mut s = state(AsOf::Live, &[]);
        let len = s.painted().len();
        assert!(!s.set_highlighted(len), "one past the end must be refused");
        assert_eq!(
            s.highlighted(),
            0,
            "a refused set must not move the highlight"
        );
    }
}
