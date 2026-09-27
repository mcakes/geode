//! The sheet (line-pricer spec §6.1): struct of arrays in sheet order, a
//! package's legs contiguous after it, depth 0 or 1. There is no separate
//! index to keep in step: `children` is a scan of the following rows'
//! `parent`, and `parent` itself is rebuilt by one walk after every
//! structural edit.
//!
//! Mutation goes through [`Sheet::apply`] (`edit.rs`). The other `pub`
//! mutators change no row's identity or request: [`Sheet::deliver`] and
//! [`Sheet::deliver_all`] (a result landing, one or a batch),
//! [`Sheet::mark_all_stale`] (a tick, a load, `:price`; never called by
//! `apply`) and [`Sheet::fold_packages`] (a recompute, which `apply` runs
//! after every edit).

use crate::core::shorthand::{render_line, render_package};
use crate::core::template::Template;
use chrono::{DateTime, Utc};
use geode_core::pricing::{Instrument, MarketOverrides, PriceRequest, PriceResult, Shifts};
use std::ops::Range;
use std::time::Duration;

/// Per-sheet, monotonic, never reused (spec §6.1). `u64` so it is the
/// `id` a `PriceLine` carries and the `line` axis a document stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Line,
    Package {
        template: Template,
    },
    /// Reserved for slice 2's per-underlying children (spec ruling 5).
    /// Nothing in slice 1 constructs it; `from_rows` refuses it.
    Underlying,
}

/// A line's own shifts; `None` inherits the sheet's (spec ruling 8).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct OwnShifts {
    pub spot_pct: Option<f64>,
    pub vol_pts: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineState {
    Fresh,
    Stale,
    Failed(String),
}

/// The sheet's periodic reprice (spec §9.4): the app default, off, or its
/// own interval. Three states, because storage keeps three (§7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    Default,
    Off,
    Every(Duration),
}

/// One line as the parser or a caller describes it, before it has an id.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSpec {
    pub instrument: Instrument,
    /// Signed; a sell is negative; never zero.
    pub qty: i64,
    pub shift: OwnShifts,
}

/// What one shorthand line means: a line, or a package with its legs.
#[derive(Debug, Clone, PartialEq)]
pub enum RowSpec {
    Line(LineSpec),
    Package {
        template: Template,
        legs: Vec<LineSpec>,
    },
}

/// One row in transit: what `Remove` records and `Restore` reinstates,
/// ids and results included, so undo of a removal re-requests nothing
/// (spec §6.2). Exists only inside an [`crate::core::edit::Undo`]; the
/// sheet never stores one.
#[derive(Debug, Clone, PartialEq)]
pub struct RowRecord {
    pub id: LineId,
    pub kind: RowKind,
    /// The parent's id (not index — indices move).
    pub parent: Option<LineId>,
    pub instrument: Option<Instrument>,
    pub qty: i64,
    pub shift: OwnShifts,
    pub revision: u64,
    pub result: Option<PriceResult>,
    pub state: LineState,
    pub priced_at: Option<DateTime<Utc>>,
}

/// Where an insert lands (planning decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A flat index that is a root boundary: `0`, `len()`, or the first
    /// row of a root. The new rows become roots.
    Root { at: usize },
    /// Leg slot `leg` (`0..=children.len()`) of the package at flat row
    /// `package`. The new rows become its legs.
    Leg { package: usize, leg: usize },
}

/// What `deliver` did with a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    Installed,
    UnknownLine,
    NotALine,
    /// An edit landed during the round trip; the answer is for an older
    /// request (spec §9.2). Dropped.
    OldRevision {
        current: u64,
    },
    /// A bug (spec §10.1): dropped and, by the tile, logged.
    FutureRevision {
        current: u64,
    },
}

#[derive(Debug)]
pub struct Sheet {
    pub name: String,
    pub view: String,
    /// Read through [`Sheet::sheet_shift`]; written only by
    /// `Edit::SetSheetShift`, because it feeds every inheriting line's
    /// `request()` and a direct write would leave them all unstaled.
    pub(crate) sheet_shift: OwnShifts,
    /// Sheet-wide, by underlying: spot levels now (spec ruling 1). Read
    /// through [`Sheet::overrides`]; written only by
    /// `Edit::SetSpotOverride`, for the same reason (§9.3 stales the
    /// underlying's lines explicitly).
    pub(crate) overrides: MarketOverrides,
    pub refresh: Refresh,
    // per row, in sheet order
    ids: Vec<LineId>,
    kind: Vec<RowKind>,
    parent: Vec<Option<u32>>,
    instrument: Vec<Option<Instrument>>,
    qty: Vec<i64>,
    shift: Vec<OwnShifts>,
    revision: Vec<u64>,
    result: Vec<Option<PriceResult>>,
    state: Vec<LineState>,
    priced_at: Vec<Option<DateTime<Utc>>>,
    next_id: u64,
}

impl Sheet {
    pub fn new(name: &str) -> Sheet {
        Sheet {
            name: name.to_string(),
            view: "vanilla".to_string(),
            sheet_shift: OwnShifts::default(),
            overrides: MarketOverrides::default(),
            refresh: Refresh::Default,
            ids: Vec::new(),
            kind: Vec::new(),
            parent: Vec::new(),
            instrument: Vec::new(),
            qty: Vec::new(),
            shift: Vec::new(),
            revision: Vec::new(),
            result: Vec::new(),
            state: Vec::new(),
            priced_at: Vec::new(),
            next_id: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn id(&self, row: usize) -> LineId {
        self.ids[row]
    }

    pub fn index_of(&self, id: LineId) -> Option<usize> {
        self.ids.iter().position(|i| *i == id)
    }

    pub fn kind(&self, row: usize) -> RowKind {
        self.kind[row]
    }

    pub fn parent(&self, row: usize) -> Option<usize> {
        self.parent[row].map(|p| p as usize)
    }

    pub fn depth(&self, row: usize) -> usize {
        usize::from(self.parent[row].is_some())
    }

    pub fn instrument(&self, row: usize) -> Option<&Instrument> {
        self.instrument[row].as_ref()
    }

    /// The one underlying `row` is on: its own instrument's for a line or a
    /// leg, the legs' shared one for a package. `None` for a package across
    /// several underlyings (or none), or a row with no instrument; a launch
    /// context names one underlying or nothing.
    pub fn sole_underlying(&self, row: usize) -> Option<String> {
        if !self.is_package(row) {
            return self.instrument(row).map(|i| i.underlying().to_string());
        }
        let mut found: Option<&str> = None;
        for leg in self.children(row) {
            let u = self.instrument(leg)?.underlying();
            match found {
                None => found = Some(u),
                Some(f) if f == u => {}
                Some(_) => return None,
            }
        }
        found.map(str::to_string)
    }

    pub fn qty(&self, row: usize) -> i64 {
        self.qty[row]
    }

    pub fn shift(&self, row: usize) -> OwnShifts {
        self.shift[row]
    }

    /// The sheet-wide shifts every line without its own inherits; set
    /// through `Edit::SetSheetShift` alone (the one mutation door).
    pub fn sheet_shift(&self) -> OwnShifts {
        self.sheet_shift
    }

    /// The sheet-wide spot overrides; set through
    /// `Edit::SetSpotOverride` alone.
    pub fn overrides(&self) -> &MarketOverrides {
        &self.overrides
    }

    pub fn revision(&self, row: usize) -> u64 {
        self.revision[row]
    }

    pub fn result(&self, row: usize) -> Option<&PriceResult> {
        self.result[row].as_ref()
    }

    pub fn state(&self, row: usize) -> &LineState {
        &self.state[row]
    }

    pub fn priced_at(&self, row: usize) -> Option<DateTime<Utc>> {
        self.priced_at[row]
    }

    pub fn is_line(&self, row: usize) -> bool {
        self.kind[row] == RowKind::Line
    }

    pub fn is_package(&self, row: usize) -> bool {
        matches!(self.kind[row], RowKind::Package { .. })
    }

    /// The contiguous run of legs after a package; empty for a line.
    pub fn children(&self, row: usize) -> Range<usize> {
        let start = row + 1;
        if !self.is_package(row) {
            return start..start;
        }
        let mut end = start;
        while end < self.len() && self.parent[end] == Some(row as u32) {
            end += 1;
        }
        start..end
    }

    pub fn roots(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|r| self.parent[*r].is_none())
    }

    /// `own.or(sheet)` per field, `0.0` when both are `None` (spec §6.1).
    pub fn effective_shifts(&self, row: usize) -> Shifts {
        let own = self.shift[row];
        Shifts {
            spot_pct: own.spot_pct.or(self.sheet_shift.spot_pct).unwrap_or(0.0),
            vol_pts: own.vol_pts.or(self.sheet_shift.vol_pts).unwrap_or(0.0),
        }
    }

    /// The one place a line's request is assembled (spec §6.1). `None`
    /// on a package.
    pub fn request(&self, row: usize) -> Option<PriceRequest> {
        self.instrument[row]
            .as_ref()
            .map(|instrument| PriceRequest {
                instrument: instrument.clone(),
                shifts: self.effective_shifts(row),
            })
    }

    /// Lines (never packages) that are `Stale`: what the tile submits.
    pub fn stale_lines(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len()).filter(|r| self.is_line(*r) && self.state[*r] == LineState::Stale)
    }

    pub fn record(&self, row: usize) -> RowRecord {
        RowRecord {
            id: self.ids[row],
            kind: self.kind[row],
            parent: self.parent(row).map(|p| self.ids[p]),
            instrument: self.instrument[row].clone(),
            qty: self.qty[row],
            shift: self.shift[row],
            revision: self.revision[row],
            result: self.result[row],
            state: self.state[row].clone(),
            priced_at: self.priced_at[row],
        }
    }

    /// A result landing (spec §9.2): installed only for a line that
    /// exists at exactly the answered revision. A failure installs
    /// `Failed` and keeps the last good result (the row paints `—`
    /// either way). Folds packages — the single-result form; a whole
    /// batch goes through [`Sheet::deliver_all`], which folds once.
    pub fn deliver(
        &mut self,
        id: LineId,
        revision: u64,
        result: Result<PriceResult, String>,
        at: DateTime<Utc>,
    ) -> Delivered {
        let delivered = self.install(id, revision, result, at);
        self.fold_packages();
        delivered
    }

    /// The batch door Part 3's `Delivery::Price` arm uses: every result
    /// installed, then ONE `fold_packages` at the end (spec §9.2 folds
    /// once after the loop over a batch's results — folding per landing
    /// is quadratic in the sheet's length). Answers one [`Delivered`]
    /// per result, in the order given. `deliver` is the single-line form.
    pub fn deliver_all(
        &mut self,
        results: impl IntoIterator<Item = (LineId, u64, Result<PriceResult, String>)>,
        at: DateTime<Utc>,
    ) -> Vec<Delivered> {
        let out: Vec<Delivered> = results
            .into_iter()
            .map(|(id, revision, result)| self.install(id, revision, result, at))
            .collect();
        self.fold_packages();
        out
    }

    /// Every line `Stale` at its current revision, then one fold (spec
    /// §9.4's tick, §8.6's `:price`). The third state-only mutator beside
    /// `deliver`/`deliver_all`: a tick is not an edit, so no revision
    /// moves — a result already in flight at the current revision must
    /// still install when it lands.
    pub fn mark_all_stale(&mut self) {
        for row in 0..self.len() {
            if self.is_line(row) {
                self.state[row] = LineState::Stale;
            }
        }
        self.fold_packages();
    }

    /// One result into its row; everything `deliver` does except the
    /// fold, so a batch can fold once (spec §9.2).
    fn install(
        &mut self,
        id: LineId,
        revision: u64,
        result: Result<PriceResult, String>,
        at: DateTime<Utc>,
    ) -> Delivered {
        let Some(row) = self.index_of(id) else {
            return Delivered::UnknownLine;
        };
        if !self.is_line(row) {
            return Delivered::NotALine;
        }
        let current = self.revision[row];
        if revision < current {
            return Delivered::OldRevision { current };
        }
        if revision > current {
            return Delivered::FutureRevision { current };
        }
        match result {
            Ok(r) => {
                self.result[row] = Some(r);
                self.state[row] = LineState::Fresh;
            }
            Err(message) => self.state[row] = LineState::Failed(message),
        }
        self.priced_at[row] = Some(at);
        Delivered::Installed
    }

    /// Every package's painted numbers are `Σ qty_leg × value_leg` over
    /// its legs, its state `Failed` (naming the first failed leg) if any
    /// leg is, else `Stale` if any leg is, else `Fresh`; its result is
    /// `Some` only when every leg has one and none has failed; its
    /// `priced_at` the oldest leg's (spec §6.4, planning decision 9).
    /// Aggregation, not arithmetic (PHILOSOPHY §1).
    pub fn fold_packages(&mut self) {
        for p in 0..self.len() {
            if !self.is_package(p) {
                continue;
            }
            let legs = self.children(p);
            // An empty package has no sum (planning decision 8).
            let mut complete = !legs.is_empty();
            let mut sum = PriceResult {
                price: 0.0,
                delta: 0.0,
                gamma: 0.0,
                vega: 0.0,
                theta: 0.0,
                rho: 0.0,
            };
            let mut stale = false;
            let mut failed: Option<String> = None;
            let mut oldest: Option<DateTime<Utc>> = None;
            for leg in legs {
                match &self.state[leg] {
                    LineState::Failed(m) if failed.is_none() => {
                        failed = Some(format!(
                            "{}: {m}",
                            render_line(
                                self.qty[leg],
                                self.instrument[leg].as_ref().expect("a leg is a line")
                            )
                        ));
                    }
                    LineState::Failed(_) => {}
                    LineState::Stale => stale = true,
                    LineState::Fresh => {}
                }
                match self.result[leg] {
                    Some(r) => {
                        let q = self.qty[leg] as f64;
                        sum.price += q * r.price;
                        sum.delta += q * r.delta;
                        sum.gamma += q * r.gamma;
                        sum.vega += q * r.vega;
                        sum.theta += q * r.theta;
                        sum.rho += q * r.rho;
                    }
                    None => complete = false,
                }
                oldest = match (oldest, self.priced_at[leg]) {
                    (None, t) => t,
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) => Some(a),
                };
            }
            self.result[p] = if complete && failed.is_none() {
                Some(sum)
            } else {
                None
            };
            self.state[p] = match failed {
                Some(m) => LineState::Failed(m),
                None if stale => LineState::Stale,
                None => LineState::Fresh,
            };
            self.priced_at[p] = oldest;
        }
    }

    /// The row in the grammar (spec §6.3): a line; a package in template
    /// form while its legs match the table, else its legs one per line;
    /// an empty package as nothing.
    pub fn shorthand(&self, row: usize) -> String {
        match self.kind[row] {
            RowKind::Line | RowKind::Underlying => match &self.instrument[row] {
                Some(i) => render_line(self.qty[row], i),
                None => String::new(),
            },
            RowKind::Package { template } => {
                let legs: Vec<(i64, &Instrument)> = self
                    .children(row)
                    .filter_map(|l| self.instrument[l].as_ref().map(|i| (self.qty[l], i)))
                    .collect();
                render_package(template, &legs).unwrap_or_else(|| {
                    legs.iter()
                        .map(|(q, i)| render_line(*q, i))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            }
        }
    }

    // ---- the structural primitives `edit.rs` builds on (pub(crate)) ----

    /// The next fresh id; never reused (spec §6.1).
    pub(crate) fn fresh_id(&mut self) -> LineId {
        let id = LineId(self.next_id);
        self.next_id += 1;
        id
    }

    /// Whether `id` is currently a row.
    pub(crate) fn has_id(&self, id: LineId) -> bool {
        self.ids.contains(&id)
    }

    /// Insert one row at flat `at`, with `parent` as a flat index (or
    /// `None` for a root). The caller calls `reindex_parents` afterwards
    /// once its whole batch is in. Bumps `next_id` past a restored id.
    pub(crate) fn splice_in(&mut self, at: usize, rec: RowRecord, parent: Option<usize>) {
        self.ids.insert(at, rec.id);
        self.kind.insert(at, rec.kind);
        self.parent.insert(at, parent.map(|p| p as u32));
        self.instrument.insert(at, rec.instrument);
        self.qty.insert(at, rec.qty);
        self.shift.insert(at, rec.shift);
        self.revision.insert(at, rec.revision);
        self.result.insert(at, rec.result);
        self.state.insert(at, rec.state);
        self.priced_at.insert(at, rec.priced_at);
        self.next_id = self.next_id.max(rec.id.0 + 1);
    }

    /// Remove one row at flat `at`. Answers nothing: `parent` stores flat
    /// indices, so once one row in a range is gone every later index in
    /// that range shifts and a record taken here-after would misread its
    /// parent (this is exactly the bug `remove` avoids by calling
    /// `record(at)` for the WHOLE range first). A caller that needs the
    /// removed row's record takes it via `record(at)` before calling
    /// this, never after.
    pub(crate) fn take_out(&mut self, at: usize) {
        self.ids.remove(at);
        self.kind.remove(at);
        self.parent.remove(at);
        self.instrument.remove(at);
        self.qty.remove(at);
        self.shift.remove(at);
        self.revision.remove(at);
        self.result.remove(at);
        self.state.remove(at);
        self.priced_at.remove(at);
    }

    /// Rebuild `parent` after a structural edit: a row marked as a leg
    /// (`Some(_)`) belongs to the most recent package before it. One
    /// walk, no allocation.
    pub(crate) fn reindex_parents(&mut self) {
        let mut package: Option<u32> = None;
        for i in 0..self.len() {
            if self.is_package(i) {
                package = Some(i as u32);
                // A package is always a root in slice 1.
                self.parent[i] = None;
            } else if self.parent[i].is_some() {
                self.parent[i] = package;
            }
        }
    }

    /// Mark a leg (`Some`) or root (`None`) ahead of a `reindex_parents`.
    pub(crate) fn set_leg_marker(&mut self, row: usize, leg: bool) {
        self.parent[row] = if leg { Some(u32::MAX) } else { None };
    }

    // `edit.rs`'s `SetInstrument`/`SetQty`/`SetShift` arms are the callers.
    pub(crate) fn set_instrument(&mut self, row: usize, instrument: Instrument) {
        self.instrument[row] = Some(instrument);
    }

    pub(crate) fn set_qty(&mut self, row: usize, qty: i64) {
        self.qty[row] = qty;
    }

    pub(crate) fn set_shift(&mut self, row: usize, shift: OwnShifts) {
        self.shift[row] = shift;
    }

    /// Bump the revision and mark stale: the line's request changed.
    pub(crate) fn touch(&mut self, row: usize) {
        self.revision[row] += 1;
        self.state[row] = LineState::Stale;
    }

    /// Rotate the flat range `a.start..b.end` so block `b` comes before
    /// block `a` (the two are adjacent: `a.end == b.start`).
    // `Edit::Move` is the only caller.
    pub(crate) fn swap_adjacent_blocks(&mut self, a: Range<usize>, b: Range<usize>) {
        debug_assert_eq!(a.end, b.start);
        let whole = a.start..b.end;
        let by = a.len();
        self.ids[whole.clone()].rotate_left(by);
        self.kind[whole.clone()].rotate_left(by);
        self.parent[whole.clone()].rotate_left(by);
        self.instrument[whole.clone()].rotate_left(by);
        self.qty[whole.clone()].rotate_left(by);
        self.shift[whole.clone()].rotate_left(by);
        self.revision[whole.clone()].rotate_left(by);
        self.result[whole.clone()].rotate_left(by);
        self.state[whole.clone()].rotate_left(by);
        self.priced_at[whole].rotate_left(by);
    }

    /// A fresh record for a spec, at `revision = 1`, `Stale`, unpriced.
    pub(crate) fn new_record(&mut self, spec: &LineSpec, parent: Option<LineId>) -> RowRecord {
        RowRecord {
            id: self.fresh_id(),
            kind: RowKind::Line,
            parent,
            instrument: Some(spec.instrument.clone()),
            qty: spec.qty,
            shift: spec.shift,
            revision: 1,
            result: None,
            state: LineState::Stale,
            priced_at: None,
        }
    }

    pub(crate) fn new_package_record(
        &mut self,
        template: Template,
        id: Option<LineId>,
    ) -> RowRecord {
        RowRecord {
            id: id.unwrap_or_else(|| self.fresh_id()),
            kind: RowKind::Package { template },
            parent: None,
            instrument: None,
            qty: 1,
            shift: OwnShifts::default(),
            revision: 1,
            result: None,
            state: LineState::Fresh,
            priced_at: None,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::edit::{Edit, EditError};
    use chrono::TimeZone;
    use geode_core::pricing::{Expiry, OptionKind, Strike, Vanilla};

    pub(crate) fn spx(strike: f64, kind: OptionKind) -> Instrument {
        Instrument::Vanilla(Vanilla {
            underlying: "SPX".into(),
            expiry: Expiry::Date(chrono::NaiveDate::from_ymd_opt(2026, 12, 18).unwrap()),
            strike: Strike::Absolute(strike),
            kind,
        })
    }

    pub(crate) fn line(instrument: Instrument, qty: i64) -> RowSpec {
        RowSpec::Line(LineSpec {
            instrument,
            qty,
            shift: OwnShifts::default(),
        })
    }

    pub(crate) fn callspread(qty: i64) -> RowSpec {
        crate::core::shorthand::parse(&format!("{qty} SPX Z26 4800/5200 CS")).unwrap()
    }

    pub(crate) fn result(price: f64) -> PriceResult {
        PriceResult {
            price,
            delta: price / 10.0,
            gamma: 0.01,
            vega: 1.0,
            theta: -0.5,
            rho: 0.1,
        }
    }

    pub(crate) fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    /// `Insert` at the end of the roots.
    pub(crate) fn push(sheet: &mut Sheet, rows: Vec<RowSpec>) {
        let at = sheet.len();
        sheet
            .apply(Edit::Insert {
                place: Place::Root { at },
                rows,
            })
            .unwrap();
    }

    fn ndx(strike: f64, kind: OptionKind) -> Instrument {
        let Instrument::Vanilla(mut v) = spx(strike, kind) else {
            unreachable!()
        };
        v.underlying = "NDX".into();
        Instrument::Vanilla(v)
    }

    #[test]
    fn a_line_and_a_leg_name_their_own_underlying() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), callspread(-5)],
        );
        assert_eq!(s.sole_underlying(0), Some("SPX".into()), "a line");
        let leg = s.children(1).start;
        assert_eq!(s.sole_underlying(leg), Some("SPX".into()), "a leg");
        assert_eq!(
            s.sole_underlying(1),
            Some("SPX".into()),
            "a package on one underlying"
        );
    }

    #[test]
    fn a_package_across_two_underlyings_names_none() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(1.0, OptionKind::Call), 1),
                line(ndx(2.0, OptionKind::Call), 1),
            ],
        );
        s.apply(Edit::Group {
            first: 0,
            count: 2,
            template: Template::Custom,
            id: None,
        })
        .unwrap();
        assert_eq!(s.sole_underlying(0), None);
    }

    #[test]
    fn a_new_sheet_is_empty_and_named() {
        let s = Sheet::new("untitled-1");
        assert_eq!(s.name, "untitled-1");
        assert_eq!(s.view, "vanilla");
        assert!(s.is_empty());
        assert_eq!(s.roots().count(), 0);
        assert_eq!(s.refresh, Refresh::Default);
        assert_eq!(s.sheet_shift(), OwnShifts::default());
    }

    #[test]
    fn inserted_rows_take_fresh_ids_start_stale_and_a_package_keeps_its_legs_contiguous() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 2)]);
        push(&mut s, vec![callspread(-5)]);
        assert_eq!(s.len(), 4);
        assert_eq!(s.roots().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(s.kind(0), RowKind::Line);
        assert_eq!(
            s.kind(1),
            RowKind::Package {
                template: Template::CS
            }
        );
        assert_eq!(s.children(1), 2..4);
        assert_eq!(s.children(0), 1..1, "a line has no children");
        assert_eq!(
            (s.depth(0), s.depth(1), s.depth(2), s.depth(3)),
            (0, 0, 1, 1)
        );
        assert_eq!((s.parent(2), s.parent(3)), (Some(1), Some(1)));
        assert_eq!(
            (0..4).map(|r| s.id(r)).collect::<Vec<_>>(),
            vec![LineId(1), LineId(2), LineId(3), LineId(4)],
            "ids are monotonic in insertion order"
        );
        assert_eq!(s.index_of(LineId(4)), Some(3));
        assert_eq!(s.index_of(LineId(99)), None);
        assert_eq!(s.qty(0), 2);
        assert_eq!((s.qty(2), s.qty(3)), (-5, 5));
        assert_eq!(s.instrument(1), None, "a package has no instrument");
        assert!(s.instrument(2).is_some());
        for r in 0..4 {
            assert_eq!(s.revision(r), 1, "row {r}");
            assert_eq!(s.result(r), None);
        }
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(s.state(2), &LineState::Stale);
        assert_eq!(
            s.state(1),
            &LineState::Stale,
            "a package is stale while a leg is"
        );
        assert_eq!(
            s.stale_lines().collect::<Vec<_>>(),
            vec![0, 2, 3],
            "lines only, never the package"
        );
    }

    #[test]
    fn a_place_is_a_root_boundary_or_a_leg_slot() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(1)]);
        // Root { at } inside the leg run is refused.
        let e = s
            .apply(Edit::Insert {
                place: Place::Root { at: 1 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err();
        assert_eq!(e, EditError::NotARootBoundary(1));
        // Root { at } past the end is refused; at == len is the end.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Root { at: 4 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err(),
            EditError::NoSuchRow(4)
        );
        // A package spec at a leg place is refused (depth is at most two).
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Leg { package: 0, leg: 0 },
                rows: vec![callspread(1)],
            })
            .unwrap_err(),
            EditError::PackageInsidePackage
        );
        // A leg place on a line is refused.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Leg { package: 1, leg: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err(),
            EditError::NotAPackage(1)
        );
        // leg may be 0..=children.len(); one past is refused.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Leg { package: 0, leg: 3 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 1)],
            })
            .unwrap_err(),
            EditError::LegOutOfRange { package: 0, leg: 3 }
        );
        // A leg inserted at leg 1 lands between the two, as a leg.
        s.apply(Edit::Insert {
            place: Place::Leg { package: 0, leg: 1 },
            rows: vec![line(spx(5000.0, OptionKind::Put), 3)],
        })
        .unwrap();
        assert_eq!(s.children(0), 1..4);
        assert_eq!(s.qty(2), 3);
        assert_eq!(s.parent(2), Some(0));
        // A leg at leg == len appends; a root at len appends.
        s.apply(Edit::Insert {
            place: Place::Leg { package: 0, leg: 3 },
            rows: vec![line(spx(5100.0, OptionKind::Put), 4)],
        })
        .unwrap();
        assert_eq!(s.children(0), 1..5);
        assert_eq!(s.roots().collect::<Vec<_>>(), vec![0]);
        // Empty inserts and zero quantities are refused.
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![],
            })
            .unwrap_err(),
            EditError::EmptyInsert
        );
        assert_eq!(
            s.apply(Edit::Insert {
                place: Place::Root { at: 0 },
                rows: vec![line(spx(5000.0, OptionKind::Call), 0)],
            })
            .unwrap_err(),
            EditError::ZeroQty
        );
    }

    #[test]
    fn remove_takes_a_package_with_its_legs_and_a_leg_alone() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![
                line(spx(5000.0, OptionKind::Call), 1),
                callspread(1),
                line(spx(5100.0, OptionKind::Put), 1),
            ],
        );
        assert_eq!(s.len(), 5);
        let undo = s.apply(Edit::Remove { at: 1 }).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.roots().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(s.id(1), LineId(5));
        match &undo.inverse[..] {
            [Edit::Restore { at: 1, rows }] => {
                assert_eq!(rows.len(), 3);
                assert_eq!(rows[0].id, LineId(2));
                assert_eq!(rows[0].parent, None);
                assert_eq!(rows[1].parent, Some(LineId(2)));
                assert_eq!(rows[2].parent, Some(LineId(2)));
            }
            other => panic!("{other:?}"),
        }
        // A leg alone: the package stays, possibly empty (planning decision 8).
        push(&mut s, vec![callspread(1)]);
        s.apply(Edit::Remove { at: 3 }).unwrap();
        assert_eq!(s.children(2), 3..4);
        s.apply(Edit::Remove { at: 3 }).unwrap();
        assert_eq!(s.children(2), 3..3, "an empty package may exist");
        assert!(s.is_package(2));
        assert_eq!(s.state(2), &LineState::Fresh, "an empty package is fresh");
        assert_eq!(s.result(2), None);
        assert_eq!(
            s.apply(Edit::Remove { at: 9 }).unwrap_err(),
            EditError::NoSuchRow(9)
        );
    }

    #[test]
    fn a_request_is_the_instrument_with_effective_shifts() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), 1), callspread(1)],
        );
        assert_eq!(s.request(1), None, "a package has no request");
        let r = s.request(0).unwrap();
        assert_eq!(r.instrument, spx(5000.0, OptionKind::Call));
        assert_eq!(r.shifts, Shifts::default(), "both None → 0.0");
        s.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: None,
        }))
        .unwrap();
        assert_eq!(
            s.effective_shifts(0),
            Shifts {
                spot_pct: 2.0,
                vol_pts: 0.0
            },
            "inherits the sheet's"
        );
        // An own value wins per field.
        let mut own = s;
        own.apply(Edit::SetSheetShift(OwnShifts {
            spot_pct: Some(2.0),
            vol_pts: Some(-1.0),
        }))
        .unwrap();
        // Build a row with an own vol shift through the record/restore
        // door rather than `SetShift`.
        let mut rec = own.record(0);
        rec.shift = OwnShifts {
            spot_pct: None,
            vol_pts: Some(3.0),
        };
        own.apply(Edit::Remove { at: 0 }).unwrap();
        own.apply(Edit::Restore {
            at: 0,
            rows: vec![rec],
        })
        .unwrap();
        assert_eq!(
            own.effective_shifts(0),
            Shifts {
                spot_pct: 2.0,
                vol_pts: 3.0
            }
        );
    }

    #[test]
    fn a_delivery_for_an_old_revision_is_dropped_and_the_current_one_installed() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        let id = s.id(0);
        // Pretend an edit bumped the revision (`apply` does this for real).
        let mut rec = s.record(0);
        rec.revision = 2;
        s.apply(Edit::Remove { at: 0 }).unwrap();
        s.apply(Edit::Restore {
            at: 0,
            rows: vec![rec],
        })
        .unwrap();
        assert_eq!(s.revision(0), 2);
        assert_eq!(
            s.deliver(id, 1, Ok(result(10.0)), at(0)),
            Delivered::OldRevision { current: 2 }
        );
        assert_eq!(s.result(0), None);
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(
            s.deliver(id, 3, Ok(result(10.0)), at(0)),
            Delivered::FutureRevision { current: 2 },
            "a bug, dropped"
        );
        assert_eq!(
            s.deliver(id, 2, Ok(result(10.0)), at(5)),
            Delivered::Installed
        );
        assert_eq!(s.result(0), Some(&result(10.0)));
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(s.priced_at(0), Some(at(5)));
        assert_eq!(
            s.deliver(LineId(77), 1, Ok(result(1.0)), at(0)),
            Delivered::UnknownLine
        );
        // A failure installs Failed and keeps the last good result.
        assert_eq!(
            s.deliver(id, 2, Err("refused by the mock".into()), at(6)),
            Delivered::Installed
        );
        assert_eq!(s.state(0), &LineState::Failed("refused by the mock".into()));
        assert_eq!(s.result(0), Some(&result(10.0)));
        assert_eq!(s.priced_at(0), Some(at(6)));
        // A package row is not a line.
        push(&mut s, vec![callspread(1)]);
        assert_eq!(
            s.deliver(s.id(1), 1, Ok(result(1.0)), at(0)),
            Delivered::NotALine
        );
    }

    #[test]
    fn a_package_sums_qty_times_value_over_its_legs_with_signed_quantities() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![callspread(-5)]); // legs: -5 × 4800 call, +5 × 5200 call
        let (long, short) = (s.id(1), s.id(2));
        assert_eq!(s.result(0), None, "no sum until every leg has a result");
        s.deliver(long, 1, Ok(result(100.0)), at(0));
        assert_eq!(s.result(0), None);
        assert_eq!(s.state(0), &LineState::Stale);
        s.deliver(short, 1, Ok(result(40.0)), at(1));
        let sum = s.result(0).unwrap();
        // -5 × 100 + 5 × 40 = -300; delta: -5 × 10 + 5 × 4 = -30; gamma: 0 (−5 + 5 = 0 × 0.01)
        assert_eq!(sum.price, -300.0);
        assert_eq!(sum.delta, -30.0);
        assert_eq!(sum.gamma, 0.0);
        assert_eq!(sum.vega, 0.0);
        assert_eq!(sum.theta, 0.0);
        assert_eq!(sum.rho, 0.0);
        assert_eq!(s.state(0), &LineState::Fresh);
        assert_eq!(
            s.priced_at(0),
            Some(at(0)),
            "a package is as old as its oldest leg"
        );
        // Failed wins over Stale (planning decision 9) and names the leg.
        s.deliver(long, 1, Err("refused by the mock".into()), at(2));
        match s.state(0) {
            LineState::Failed(m) => {
                assert!(m.contains("-5 SPX Z26 4800 C"), "{m}");
                assert!(m.contains("refused by the mock"), "{m}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(s.result(0), None, "a failed leg makes the sum uncomputable");
    }

    #[test]
    fn deliver_all_installs_every_result_and_folds_once() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![callspread(-5), line(spx(5000.0, OptionKind::Call), 2)],
        );
        // rows: 0 package, 1 leg (-5 × 4800 C), 2 leg (+5 × 5200 C), 3 line
        let (long, short, plain) = (s.id(1), s.id(2), s.id(3));
        let delivered = s.deliver_all(
            [
                (long, 1, Ok(result(100.0))),
                // An edit landed on this one during the round trip.
                (short, 0, Ok(result(40.0))),
                (plain, 1, Ok(result(7.0))),
            ],
            at(3),
        );
        assert_eq!(
            delivered,
            vec![
                Delivered::Installed,
                Delivered::OldRevision { current: 1 },
                Delivered::Installed,
            ],
            "one answer per result, in order"
        );
        assert_eq!(s.result(1), Some(&result(100.0)));
        assert_eq!(s.result(2), None, "the old-revision result was dropped");
        assert_eq!(s.result(3), Some(&result(7.0)));
        assert_eq!(s.state(1), &LineState::Fresh);
        assert_eq!(s.state(2), &LineState::Stale);
        assert_eq!(s.state(3), &LineState::Fresh);
        // The one fold at the end ran: the package is stale (a leg is)
        // and has no sum until every leg has a result.
        assert_eq!(s.state(0), &LineState::Stale);
        assert_eq!(s.result(0), None);
        // The missing leg lands; the sum is the folded batch.
        let delivered = s.deliver_all([(short, 1, Ok(result(40.0)))], at(4));
        assert_eq!(delivered, vec![Delivered::Installed]);
        assert_eq!(s.state(0), &LineState::Fresh);
        // -5 × 100 + 5 × 40 = -300
        assert_eq!(s.result(0).unwrap().price, -300.0);
        assert_eq!(s.priced_at(0), Some(at(3)), "the oldest leg's");
        // Unknown and not-a-line answers come back in place too.
        assert_eq!(
            s.deliver_all(
                [
                    (LineId(77), 1, Ok(result(1.0))),
                    (s.id(0), 1, Ok(result(1.0))),
                ],
                at(5),
            ),
            vec![Delivered::UnknownLine, Delivered::NotALine]
        );
        assert_eq!(
            s.result(0).unwrap().price,
            -300.0,
            "neither touched the fold"
        );
    }

    #[test]
    fn shorthand_renders_a_line_a_template_package_and_a_custom_one() {
        let mut s = Sheet::new("t");
        push(
            &mut s,
            vec![line(spx(5000.0, OptionKind::Call), -2), callspread(3)],
        );
        assert_eq!(s.shorthand(0), "-2 SPX Z26 5000 C");
        assert_eq!(s.shorthand(1), "3 SPX Z26 4800/5200 CS");
        assert_eq!(s.shorthand(2), "3 SPX Z26 4800 C");
        // A leg removed leaves the table: one line per remaining leg.
        s.apply(Edit::Remove { at: 3 }).unwrap();
        assert_eq!(s.shorthand(1), "3 SPX Z26 4800 C");
        push(&mut s, vec![callspread(1)]);
        s.apply(Edit::Remove { at: 4 }).unwrap();
        s.apply(Edit::Remove { at: 4 }).unwrap();
        assert_eq!(s.shorthand(3), "", "an empty package renders nothing");
    }

    /// The refresh tick and `:price` (spec §9.4, §8.6; Part 3 planning
    /// decision 3): every LINE goes `Stale` — a failed one too, since a
    /// refusal may be transient — at its CURRENT revision, so a result
    /// already in flight still installs; packages fold to `Stale`.
    #[test]
    fn mark_all_stale_stales_every_line_and_bumps_no_revision() {
        let mut s = Sheet::new("t");
        push(&mut s, vec![line(spx(5000.0, OptionKind::Call), 1)]);
        push(&mut s, vec![callspread(1)]);
        let lines: Vec<usize> = (0..s.len()).filter(|r| s.is_line(*r)).collect();
        let answers: Vec<_> = lines
            .iter()
            .map(|r| (s.id(*r), s.revision(*r), Ok(result(10.0))))
            .collect();
        s.deliver_all(answers, at(0));
        // One line fails, so the sweep is seen to cover `Failed` too.
        let first = s.id(0);
        let rev = s.revision(0);
        s.apply(Edit::SetQty { row: 0, qty: 2 }).unwrap();
        assert_eq!(s.revision(0), rev, "SetQty changes no request");
        s.deliver(first, rev, Err("refused".into()), at(1));
        let before: Vec<u64> = (0..s.len()).map(|r| s.revision(r)).collect();

        s.mark_all_stale();

        for r in lines {
            assert_eq!(s.state(r), &LineState::Stale, "row {r}");
        }
        assert_eq!(s.state(1), &LineState::Stale, "the package folds to Stale");
        let after: Vec<u64> = (0..s.len()).map(|r| s.revision(r)).collect();
        assert_eq!(before, after, "a tick is not an edit");
        // A result at the unchanged revision still installs.
        let leg = 2;
        assert_eq!(
            s.deliver(s.id(leg), s.revision(leg), Ok(result(3.0)), at(2)),
            Delivered::Installed
        );
        assert_eq!(s.state(leg), &LineState::Fresh);
    }
}
