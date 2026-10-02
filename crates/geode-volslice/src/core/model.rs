//! What a slice tile holds and shows, as pure state: the trace kinds, the
//! loaded documents, the expiry strip (rulings 4, 5) and the choices a
//! user makes over them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::NaiveDate;
use geode_core::document::DocumentRows;
use geode_core::link::DraftMark;
use geode_core::vol::Coordinate;

use crate::core::docs::{ChainExpiry, terms};

pub const DEFAULT_SPLIT: f32 = 0.7;

/// A trace kind, in header order: the digit that toggles it is its
/// position plus one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Cvi,
    Draft,
    Chain,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Cvi, Kind::Draft, Kind::Chain];

    pub fn index(self) -> usize {
        self as usize
    }

    /// The session and command spelling, and the chip's text.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Cvi => "cvi",
            Kind::Draft => "cvi draft",
            Kind::Chain => "chain",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        let t = text.trim();
        Kind::ALL
            .into_iter()
            .find(|k| k.label().eq_ignore_ascii_case(t))
    }

    /// A curve is evaluated from a document; a chain is quoted.
    pub fn is_curve(self) -> bool {
        self != Kind::Chain
    }
}

/// An ordered difference `minuend` minus `subtrahend` of two distinct kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub minuend: Kind,
    pub subtrahend: Kind,
}

impl Pair {
    pub fn new(minuend: Kind, subtrahend: Kind) -> Option<Pair> {
        (minuend != subtrahend).then_some(Pair {
            minuend,
            subtrahend,
        })
    }

    pub fn label(self) -> String {
        format!(
            "{} \u{2212} {}",
            self.minuend.label(),
            self.subtrahend.label()
        )
    }
}

/// What is loaded: the published CVI as of the frame, the followed
/// group's draft (now, whatever the as-of), the chain by expiry.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub cvi: Option<Arc<DocumentRows>>,
    pub draft: Option<(Arc<DocumentRows>, DraftMark)>,
    pub chain: Vec<ChainExpiry>,
}

impl Loaded {
    pub fn has(&self, kind: Kind) -> bool {
        match kind {
            Kind::Cvi => self.cvi.is_some(),
            Kind::Draft => self.draft.is_some(),
            Kind::Chain => !self.chain.is_empty(),
        }
    }

    pub fn kinds(&self) -> Vec<Kind> {
        Kind::ALL.into_iter().filter(|k| self.has(*k)).collect()
    }

    pub fn document(&self, kind: Kind) -> Option<&Arc<DocumentRows>> {
        match kind {
            Kind::Cvi => self.cvi.as_ref(),
            Kind::Draft => self.draft.as_ref().map(|(rows, _)| rows),
            Kind::Chain => None,
        }
    }

    pub fn chain_at(&self, expiry: NaiveDate) -> Option<&ChainExpiry> {
        self.chain
            .binary_search_by_key(&expiry, |c| c.expiry)
            .ok()
            .map(|i| &self.chain[i])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripRow {
    pub expiry: NaiveDate,
    /// Indexed by [`Kind::index`].
    pub has: [bool; 3],
}

/// The sorted, deduplicated union of every loaded trace's expiries, none
/// before `today`, each marked with the kinds that have it.
pub fn strip(loaded: &Loaded, today: NaiveDate) -> Vec<StripRow> {
    let mut rows: BTreeMap<NaiveDate, [bool; 3]> = BTreeMap::new();
    let mut mark = |expiry: NaiveDate, kind: Kind| {
        if expiry >= today {
            rows.entry(expiry).or_default()[kind.index()] = true;
        }
    };
    for kind in [Kind::Cvi, Kind::Draft] {
        if let Some(doc) = loaded.document(kind) {
            for t in terms(doc) {
                mark(t, kind);
            }
        }
    }
    for c in &loaded.chain {
        mark(c.expiry, Kind::Chain);
    }
    rows.into_iter()
        .map(|(expiry, has)| StripRow { expiry, has })
        .collect()
}

/// A tile's own choices. Everything here is saved (`core::session`)
/// except the cursor.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    /// The tile's own underlying; ignored while it follows a group.
    pub underlying: Option<String>,
    pub coordinate: Coordinate,
    pub hidden: BTreeSet<Kind>,
    /// `None` until a strip first seeds it.
    pub active: Option<BTreeSet<NaiveDate>>,
    pub cursor: usize,
    pub density: bool,
    pub diff: Option<Pair>,
    pub split: f32,
    /// A saved zoom in x units; `None` follows the data's extent.
    pub view: Option<(f64, f64)>,
}

impl Default for State {
    fn default() -> State {
        State {
            underlying: None,
            coordinate: Coordinate::default(),
            hidden: BTreeSet::new(),
            active: None,
            cursor: 0,
            density: false,
            diff: None,
            split: DEFAULT_SPLIT,
            view: None,
        }
    }
}

impl State {
    /// Fit the active set and the cursor to a new strip. Vanished
    /// expiries leave; a set left naming none, or never seeded, fronts the
    /// first row (an expired front month must not leave an empty plot). An
    /// empty strip changes nothing, so the set survives a query that
    /// briefly returns no data. Returns whether the active set changed.
    pub fn reconcile(&mut self, strip: &[StripRow]) -> bool {
        let Some(first) = strip.first() else {
            return false;
        };
        let before = self.active.clone();
        let kept: BTreeSet<NaiveDate> = self
            .active
            .iter()
            .flatten()
            .filter(|e| strip.iter().any(|r| r.expiry == **e))
            .copied()
            .collect();
        self.active = Some(if kept.is_empty() {
            BTreeSet::from([first.expiry])
        } else {
            kept
        });
        self.cursor = self.cursor.min(strip.len() - 1);
        self.active != before
    }

    /// Make one row the only active expiry. A row past the strip does nothing.
    pub fn solo(&mut self, strip: &[StripRow], row: usize) -> bool {
        let Some(r) = strip.get(row) else {
            return false;
        };
        let next = Some(BTreeSet::from([r.expiry]));
        let changed = self.active != next;
        self.active = next;
        changed
    }

    /// Add or remove one row's expiry. The last active expiry is refused
    /// (`false`, nothing changed): an emptied set would paint a blank plot
    /// until the next strip change fronted the first row, a jump the trader
    /// did not ask for. Solo another row first to move off it.
    pub fn toggle(&mut self, strip: &[StripRow], row: usize) -> bool {
        let Some(r) = strip.get(row) else {
            return false;
        };
        let set = self.active.get_or_insert_with(BTreeSet::new);
        if set.contains(&r.expiry) {
            if set.len() == 1 {
                return false;
            }
            set.remove(&r.expiry);
        } else {
            set.insert(r.expiry);
        }
        true
    }

    pub fn step_cursor(&mut self, strip_len: usize, delta: isize) {
        let last = strip_len.saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
    }

    /// The active expiries with their strip positions (the palette index),
    /// ascending.
    pub fn active_in(&self, strip: &[StripRow]) -> Vec<(usize, NaiveDate)> {
        strip
            .iter()
            .enumerate()
            .filter(|(_, r)| self.active.as_ref().is_some_and(|a| a.contains(&r.expiry)))
            .map(|(i, r)| (i, r.expiry))
            .collect()
    }

    pub fn cycle_coordinate(&mut self) {
        let all = Coordinate::ALL;
        let i = all.iter().position(|c| *c == self.coordinate).unwrap_or(0);
        self.coordinate = all[(i + 1) % all.len()];
    }

    pub fn toggle_kind(&mut self, kind: Kind) {
        if !self.hidden.remove(&kind) {
            self.hidden.insert(kind);
        }
    }

    pub fn visible(&self, loaded: &Loaded, kind: Kind) -> bool {
        loaded.has(kind) && !self.hidden.contains(&kind)
    }

    /// Every ordered pair of distinct loaded kinds, in header order.
    pub fn pairs(loaded: &Loaded) -> Vec<Pair> {
        let kinds = loaded.kinds();
        kinds
            .iter()
            .flat_map(|a| kinds.iter().filter_map(move |b| Pair::new(*a, *b)))
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::docs::ChainExpiry;
    use geode_core::document::{Column, DocumentRows, Value};
    use std::sync::Arc;

    pub(crate) fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// A CVI document with a five-node ladder per term; `atm_bump` lifts one term's `atm`.
    pub(crate) fn cvi(terms: &[&str], atm_bump: Option<(&str, f64)>) -> Arc<DocumentRows> {
        let nodes = [-20.0, -10.0, 0.0, 10.0, 20.0];
        let (mut t, mut n, mut param, mut fwd, mut atm, mut skew) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        for term in terms {
            for node in nodes {
                t.push(d(term));
                n.push(node);
                param.push(0.0);
                fwd.push(100.0);
                atm.push(0.2 + atm_bump.filter(|(b, _)| b == term).map_or(0.0, |(_, x)| x));
                skew.push(-0.1);
            }
        }
        Arc::new(DocumentRows {
            key: vec!["SPX.Z".into()],
            attributes: vec![
                ("anchor_date".into(), Value::Date(d("2026-10-01"))),
                ("spot_ref".into(), Value::F64(100.0)),
            ],
            axes: vec![
                ("term".into(), Column::Date(t)),
                ("node".into(), Column::F64(n)),
            ],
            values: vec![
                ("param".into(), Column::F64(param)),
                ("forward".into(), Column::F64(fwd)),
                ("atm".into(), Column::F64(atm)),
                ("skew".into(), Column::F64(skew)),
            ],
        })
    }

    pub(crate) fn chain(expiry: &str) -> ChainExpiry {
        let strikes: Vec<f64> = (0..7).map(|i| 85.0 + 5.0 * i as f64).collect();
        ChainExpiry {
            expiry: d(expiry),
            as_of: d("2026-10-02"),
            forward: 100.0,
            bid: strikes.iter().map(|_| 0.19).collect(),
            mid: strikes.iter().map(|_| 0.20).collect(),
            ask: strikes.iter().map(|_| 0.21).collect(),
            strikes,
        }
    }

    pub(crate) const TERMS: [&str; 3] = ["2026-10-16", "2026-12-18", "2027-03-19"];
    pub(crate) const TODAY: &str = "2026-10-02";

    pub(crate) fn fixture() -> Loaded {
        Loaded {
            cvi: Some(cvi(&TERMS, None)),
            draft: Some((cvi(&TERMS, Some(("2026-12-18", 0.01))), DraftMark::Editing)),
            chain: vec![chain("2026-11-20"), chain("2027-06-18")],
        }
    }

    #[test]
    fn the_strip_is_the_sorted_union_with_kind_marks_and_no_past() {
        let mut l = fixture();
        l.draft = Some((cvi(&["2026-09-18", "2026-10-16"], None), DraftMark::Behind));
        let s = strip(&l, d(TODAY));
        let dates: Vec<_> = s.iter().map(|r| r.expiry).collect();
        assert_eq!(
            dates,
            vec![
                d("2026-10-16"),
                d("2026-11-20"),
                d("2026-12-18"),
                d("2027-03-19"),
                d("2027-06-18")
            ]
        );
        assert_eq!(
            s[0].has,
            [true, true, false],
            "cvi and draft at the front term"
        );
        assert_eq!(s[1].has, [false, false, true], "chain only between terms");
    }

    #[test]
    fn the_first_strip_fronts_one_expiry() {
        let l = fixture();
        let s = strip(&l, d(TODAY));
        let mut st = State::default();
        assert!(st.reconcile(&s));
        assert_eq!(st.active, Some([d("2026-10-16")].into()));
        assert!(!st.reconcile(&s), "a second reconcile changes nothing");
    }

    #[test]
    fn a_restored_set_keeps_listed_expiries_and_drops_vanished_ones() {
        let s = strip(&fixture(), d(TODAY));
        let mut st = State {
            active: Some([d("2026-12-18"), d("2027-09-17")].into()),
            ..State::default()
        };
        st.reconcile(&s);
        assert_eq!(st.active, Some([d("2026-12-18")].into()));
    }

    #[test]
    fn a_restored_set_naming_no_listed_expiry_fronts_the_first() {
        let s = strip(&fixture(), d(TODAY));
        let mut st = State {
            active: Some([d("2026-09-18")].into()),
            ..State::default()
        };
        st.reconcile(&s);
        assert_eq!(st.active, Some([d("2026-10-16")].into()));
    }

    #[test]
    fn an_empty_strip_keeps_the_set_for_when_data_returns() {
        let mut st = State {
            active: Some([d("2026-12-18")].into()),
            ..State::default()
        };
        assert!(!st.reconcile(&[]));
        assert_eq!(st.active, Some([d("2026-12-18")].into()));
    }

    #[test]
    fn solo_and_toggle_act_on_a_row() {
        let s = strip(&fixture(), d(TODAY));
        let mut st = State::default();
        st.reconcile(&s);
        assert!(st.toggle(&s, 2));
        assert_eq!(
            st.active_in(&s),
            vec![(0, d("2026-10-16")), (2, d("2026-12-18"))]
        );
        assert!(st.solo(&s, 4));
        assert_eq!(st.active_in(&s), vec![(4, d("2027-06-18"))]);
        assert!(!st.solo(&s, 9), "a row past the strip does nothing");
    }

    #[test]
    fn toggling_the_last_active_expiry_is_refused() {
        let s = strip(&fixture(), d(TODAY));
        let mut st = State::default();
        st.reconcile(&s);
        let before = st.active.clone();
        assert!(!st.toggle(&s, 0), "the only active row stays");
        assert_eq!(st.active, before);
        assert!(st.toggle(&s, 1));
        assert!(st.toggle(&s, 0), "with another active, it may go");
        assert_eq!(st.active_in(&s), vec![(1, d("2026-11-20"))]);
    }

    #[test]
    fn the_cursor_steps_within_the_strip() {
        let mut st = State::default();
        st.step_cursor(5, -1);
        assert_eq!(st.cursor, 0);
        st.step_cursor(5, 3);
        st.step_cursor(5, 9);
        assert_eq!(st.cursor, 4);
    }

    #[test]
    fn kinds_are_in_header_order_and_pairs_are_ordered_and_distinct() {
        let l = fixture();
        assert_eq!(l.kinds(), vec![Kind::Cvi, Kind::Draft, Kind::Chain]);
        let pairs = State::pairs(&l);
        assert_eq!(pairs.len(), 6);
        assert_eq!(pairs[0], Pair::new(Kind::Cvi, Kind::Draft).unwrap());
        assert!(Pair::new(Kind::Chain, Kind::Chain).is_none());
        assert_eq!(
            Pair::new(Kind::Draft, Kind::Cvi).unwrap().label(),
            "cvi draft \u{2212} cvi"
        );
        assert_eq!(Kind::parse(" CVI draft "), Some(Kind::Draft));
    }

    #[test]
    fn the_coordinate_cycles_through_all_four() {
        let mut st = State::default();
        assert_eq!(st.coordinate, Coordinate::Moneyness);
        for want in [
            Coordinate::LogMoneyness,
            Coordinate::Delta,
            Coordinate::Strike,
            Coordinate::Moneyness,
        ] {
            st.cycle_coordinate();
            assert_eq!(st.coordinate, want);
        }
    }

    #[test]
    fn a_kind_is_visible_when_loaded_and_not_hidden() {
        let mut l = fixture();
        let mut st = State::default();
        st.toggle_kind(Kind::Chain);
        assert!(!st.visible(&l, Kind::Chain) && st.visible(&l, Kind::Cvi));
        l.draft = None;
        assert!(!st.visible(&l, Kind::Draft));
    }
}
